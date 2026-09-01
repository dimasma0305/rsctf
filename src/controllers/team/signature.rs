use std::sync::{Arc, LazyLock};
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use base64::Engine as _;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

use super::SignatureVerifyModel;
use crate::app_state::SharedState;
use crate::utils::enums::{ParticipationStatus, Role};
use crate::utils::error::{AppError, AppResult};

pub(super) const BODY_LIMIT_BYTES: usize = 256;
const PUBLIC_KEY_BASE64_BYTES: usize = 44;
const SIGNATURE_BASE64_BYTES: usize = 88;
const VERIFY_QUERY_TIMEOUT: Duration = Duration::from_secs(3);
const VERIFY_CONCURRENCY: usize = 16;

static VERIFY_SLOTS: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(VERIFY_CONCURRENCY)));

struct ParsedSignature {
    team_id: i32,
    public_key_text: String,
    public_key: [u8; 32],
    signature: [u8; 64],
}

fn invalid() -> AppError {
    AppError::bad_request("Invalid team signature envelope")
}

fn decode_canonical<const N: usize>(encoded: &str, exact_len: usize) -> AppResult<[u8; N]> {
    if encoded.len() != exact_len
        || !encoded
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
    {
        return Err(invalid());
    }
    let mut decoded = [0_u8; N];
    let written = base64::engine::general_purpose::STANDARD
        .decode_slice(encoded.as_bytes(), &mut decoded)
        .map_err(|_| invalid())?;
    if written != N || base64::engine::general_purpose::STANDARD.encode(decoded) != encoded {
        return Err(invalid());
    }
    Ok(decoded)
}

fn parse(model: SignatureVerifyModel) -> AppResult<ParsedSignature> {
    let (team_id_text, signature_text) = model.team_token.split_once(':').ok_or_else(invalid)?;
    if signature_text.contains(':')
        || team_id_text.is_empty()
        || team_id_text.len() > 10
        || !team_id_text.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid());
    }
    let team_id = team_id_text.parse::<i32>().map_err(|_| invalid())?;
    if team_id <= 0 || team_id.to_string() != team_id_text {
        return Err(invalid());
    }
    Ok(ParsedSignature {
        team_id,
        public_key: decode_canonical(&model.public_key, PUBLIC_KEY_BASE64_BYTES)?,
        public_key_text: model.public_key,
        signature: decode_canonical(signature_text, SIGNATURE_BASE64_BYTES)?,
    })
}

fn verify_crypto(parsed: &ParsedSignature) -> bool {
    let Ok(key) = VerifyingKey::from_bytes(&parsed.public_key) else {
        return false;
    };
    let signature = Signature::from_bytes(&parsed.signature);
    key.verify(
        format!("RSCTF_TEAM_{}", parsed.team_id).as_bytes(),
        &signature,
    )
    .is_ok()
}

const LIVE_CANONICAL_TEAM_SQL: &str = r#"
SELECT EXISTS (
    SELECT 1
      FROM "Games" game
      JOIN "Participations" participation
        ON participation.game_id = game.id
       AND participation.team_id = $2
       AND participation.status = $3
      JOIN "Teams" team ON team.id = participation.team_id
     WHERE game.public_key = $1
       AND statement_timestamp() BETWEEN game.start_time_utc AND game.end_time_utc
       AND NOT team.deletion_pending
       AND NOT EXISTS (
           SELECT 1
             FROM (
                 SELECT team.captain_id AS user_id
                 UNION
                 SELECT member.user_id
                   FROM "TeamMembers" member
                  WHERE member.team_id = team.id
             ) roster
             LEFT JOIN "AspNetUsers" account ON account.id = roster.user_id
            WHERE account.id IS NULL OR account.role = $4
       )
)
"#;

async fn is_live_canonical_team(
    pool: &sqlx::PgPool,
    public_key: &str,
    team_id: i32,
) -> AppResult<bool> {
    tokio::time::timeout(
        VERIFY_QUERY_TIMEOUT,
        sqlx::query_scalar::<_, bool>(LIVE_CANONICAL_TEAM_SQL)
            .bind(public_key)
            .bind(team_id)
            .bind(ParticipationStatus::Accepted as i16)
            .bind(Role::Banned as i16)
            .fetch_one(pool),
    )
    .await
    .map_err(|_| AppError::unavailable("team signature policy lookup timed out"))?
    .map_err(|error| AppError::internal(error.to_string()))
}

/// Verify a canonical game-issued team token for a currently accepted, live
/// participation. `publicKey` remains on the compatibility wire contract but
/// is only an identifier: PostgreSQL anchors it to the owning game and current
/// roster policy before this endpoint returns 200.
pub async fn verify_signature(
    State(st): State<SharedState>,
    Json(model): Json<SignatureVerifyModel>,
) -> AppResult<StatusCode> {
    let parsed = parse(model)?;
    let permit = Arc::clone(&VERIFY_SLOTS)
        .try_acquire_owned()
        .map_err(|_| AppError::too_many_requests(1))?;
    let public_key = parsed.public_key_text.clone();
    let team_id = parsed.team_id;
    let verified = tokio::task::spawn_blocking(move || verify_crypto(&parsed))
        .await
        .map_err(|error| AppError::internal(format!("signature verifier failed: {error}")))?;
    drop(permit);
    if !verified || !is_live_canonical_team(st.pg(), &public_key, team_id).await? {
        return Err(AppError::Unauthorized);
    }
    Ok(StatusCode::OK)
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use ed25519_dalek::{Signer, SigningKey};
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use uuid::Uuid;

    use super::*;

    fn valid_model(team_id: i32) -> SignatureVerifyModel {
        let key = SigningKey::from_bytes(&[7_u8; 32]);
        let signature = key.sign(format!("RSCTF_TEAM_{team_id}").as_bytes());
        SignatureVerifyModel {
            team_token: format!(
                "{team_id}:{}",
                base64::engine::general_purpose::STANDARD.encode(signature.to_bytes())
            ),
            public_key: base64::engine::general_purpose::STANDARD
                .encode(key.verifying_key().to_bytes()),
        }
    }

    #[test]
    fn envelope_is_exact_and_bounded_before_decoding() {
        let valid = valid_model(42);
        let parsed = parse(valid).unwrap();
        assert_eq!(parsed.team_id, 42);
        assert!(verify_crypto(&parsed));

        for token in [
            "",
            "42",
            "0:AAAA",
            "-1:AAAA",
            "+1:AAAA",
            "01:AAAA",
            "2147483648:AAAA",
            "1:AAAA:AAAA",
        ] {
            let mut model = valid_model(1);
            model.team_token = token.to_owned();
            assert!(parse(model).is_err(), "accepted {token:?}");
        }

        let mut oversized_key = valid_model(1);
        oversized_key.public_key = "A".repeat(PUBLIC_KEY_BASE64_BYTES + 4);
        assert!(parse(oversized_key).is_err());
        let mut malformed_key = valid_model(1);
        malformed_key.public_key = "!".repeat(PUBLIC_KEY_BASE64_BYTES);
        assert!(parse(malformed_key).is_err());
        let mut malformed_signature = valid_model(1);
        malformed_signature.team_token = format!("1:{}", "A".repeat(87));
        assert!(parse(malformed_signature).is_err());
    }

    #[test]
    fn attacker_generated_key_is_crypto_valid_but_not_a_trust_root() {
        let parsed = parse(valid_model(7)).unwrap();
        assert!(verify_crypto(&parsed));
        assert_eq!(parsed.public_key_text.len(), PUBLIC_KEY_BASE64_BYTES);
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn canonical_policy_rejects_nonlive_and_ineligible_participations() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("rsctf_team_signature_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "AspNetUsers" (id UUID PRIMARY KEY, role SMALLINT NOT NULL);
            CREATE TABLE "Teams" (
                id INTEGER PRIMARY KEY,
                captain_id UUID NOT NULL,
                deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TABLE "TeamMembers" (team_id INTEGER NOT NULL, user_id UUID NOT NULL);
            CREATE TABLE "Games" (
                id INTEGER PRIMARY KEY,
                public_key TEXT NOT NULL,
                start_time_utc TIMESTAMPTZ NOT NULL,
                end_time_utc TIMESTAMPTZ NOT NULL
            );
            CREATE TABLE "Participations" (
                game_id INTEGER NOT NULL,
                team_id INTEGER NOT NULL,
                status SMALLINT NOT NULL
            );
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let captain = Uuid::new_v4();
        sqlx::query(r#"INSERT INTO "AspNetUsers" VALUES ($1, $2)"#)
            .bind(captain)
            .bind(Role::User as i16)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "Teams" (id, captain_id) VALUES (7, $1)"#)
            .bind(captain)
            .execute(&pool)
            .await
            .unwrap();
        let key = valid_model(7).public_key;
        sqlx::query(
            r#"INSERT INTO "Games" VALUES
               (1, $1, now() - interval '1 hour', now() + interval '1 hour'),
               (2, 'ended', now() - interval '2 hours', now() - interval '1 hour')"#,
        )
        .bind(&key)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(r#"INSERT INTO "Participations" VALUES (1, 7, $1)"#)
            .bind(ParticipationStatus::Accepted as i16)
            .execute(&pool)
            .await
            .unwrap();

        assert!(is_live_canonical_team(&pool, &key, 7).await.unwrap());
        assert!(!is_live_canonical_team(&pool, "attacker", 7).await.unwrap());
        sqlx::query(r#"UPDATE "Participations" SET status = $1"#)
            .bind(ParticipationStatus::Rejected as i16)
            .execute(&pool)
            .await
            .unwrap();
        assert!(!is_live_canonical_team(&pool, &key, 7).await.unwrap());
        sqlx::query(r#"UPDATE "Participations" SET status = $1"#)
            .bind(ParticipationStatus::Accepted as i16)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            r#"UPDATE "Games"
                  SET end_time_utc = now() - interval '1 second'
                WHERE id = 1"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert!(!is_live_canonical_team(&pool, &key, 7).await.unwrap());
        sqlx::query(
            r#"UPDATE "Games"
                  SET end_time_utc = now() + interval '1 hour'
                WHERE id = 1"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(r#"UPDATE "AspNetUsers" SET role = $1"#)
            .bind(Role::Banned as i16)
            .execute(&pool)
            .await
            .unwrap();
        assert!(!is_live_canonical_team(&pool, &key, 7).await.unwrap());

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
