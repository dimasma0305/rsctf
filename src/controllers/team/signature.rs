//! Bounded, authoritative verification of event-issued team credentials.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

use crate::app_state::SharedState;
use crate::middlewares::rate_limiter::{admit_public_security, PublicSecurityWork};
use crate::utils::enums::ParticipationStatus;
use crate::utils::error::{AppError, AppResult};

use super::SignatureVerifyModel;

pub(super) const SIGNATURE_VERIFY_BODY_BYTES: usize = 256;
static SIGNATURE_VERIFY_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

/// `POST /api/team/verify` — verify an event-issued team credential.
///
/// The public key remains in the compatibility request shape, but it is only a
/// lookup identifier: the server requires that exact key on a non-deleted live
/// game and an Accepted participation for the signed team. A caller-generated
/// key can therefore never create its own trust root.
///
/// Returns an empty 200 for a current credential, 400 for malformed input, 401
/// for an invalid/untrusted credential, and 429 with `Retry-After` when either
/// the deployment-wide or local verifier budget is full.
pub async fn verify_signature(
    State(st): State<SharedState>,
    Json(model): Json<SignatureVerifyModel>,
) -> AppResult<StatusCode> {
    let parsed = parse_signature_envelope(&model)?;
    admit_public_security(PublicSecurityWork::TeamSignature).await?;
    let _permit = SIGNATURE_VERIFY_SLOTS
        .try_acquire()
        .map_err(|_| AppError::too_many_requests(1))?;

    if !trusted_team_scope(st.pg(), &model.public_key, parsed.team_id, Utc::now()).await? {
        return Err(AppError::Unauthorized);
    }
    let verified = tokio::task::spawn_blocking(move || verify_ed25519(parsed))
        .await
        .map_err(|error| AppError::internal(format!("signature verifier task failed: {error}")))?;
    if verified {
        Ok(StatusCode::OK)
    } else {
        Err(AppError::Unauthorized)
    }
}

struct ParsedSignature {
    team_id: i32,
    public_key: [u8; 32],
    signature: [u8; 64],
}

fn verify_ed25519(parsed: ParsedSignature) -> bool {
    let Ok(verifying_key) = VerifyingKey::from_bytes(&parsed.public_key) else {
        return false;
    };
    verifying_key
        .verify(
            format!("RSCTF_TEAM_{}", parsed.team_id).as_bytes(),
            &Signature::from_bytes(&parsed.signature),
        )
        .is_ok()
}

async fn trusted_team_scope(
    pool: &sqlx::PgPool,
    canonical_public_key: &str,
    team_id: i32,
    now: DateTime<Utc>,
) -> AppResult<bool> {
    sqlx::query_scalar(
        r#"SELECT EXISTS (
               SELECT 1
                 FROM "Games" game
                 JOIN "Participations" participation
                   ON participation.game_id = game.id
                 JOIN "Teams" team ON team.id = participation.team_id
                WHERE game.public_key = $1
                  AND participation.team_id = $2
                  AND participation.status = $3
                  AND game.deletion_pending = FALSE
                  AND team.deletion_pending = FALSE
                  AND game.start_time_utc <= $4
                  AND $4 < game.end_time_utc
           )"#,
    )
    .bind(canonical_public_key)
    .bind(team_id)
    .bind(ParticipationStatus::Accepted as i16)
    .bind(now)
    .fetch_one(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

fn canonical_base64<const N: usize>(value: &str, encoded_len: usize) -> Option<[u8; N]> {
    if value.len() != encoded_len || !value.is_ascii() {
        return None;
    }
    let decoded = crate::utils::codec::base64_decode(value)?;
    if crate::utils::codec::base64_encode(&decoded) != value {
        return None;
    }
    decoded.try_into().ok()
}

fn parse_signature_envelope(model: &SignatureVerifyModel) -> AppResult<ParsedSignature> {
    let public_key = canonical_base64::<32>(&model.public_key, 44)
        .ok_or_else(|| AppError::bad_request("Invalid signature"))?;
    if model.team_token.len() > 99 || model.team_token.matches(':').count() != 1 {
        return Err(AppError::bad_request("Invalid signature"));
    }
    let (id, encoded_signature) = model
        .team_token
        .split_once(':')
        .ok_or_else(|| AppError::bad_request("Invalid signature"))?;
    if id.is_empty() || id.len() > 10 || !id.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(AppError::bad_request("Invalid signature"));
    }
    let team_id = id
        .parse::<i32>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| AppError::bad_request("Invalid signature"))?;
    let signature = canonical_base64::<64>(encoded_signature, 88)
        .ok_or_else(|| AppError::bad_request("Invalid signature"))?;
    Ok(ParsedSignature {
        team_id,
        public_key,
        signature,
    })
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use chrono::Duration;
    use ed25519_dalek::{Signer, SigningKey};
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use uuid::Uuid;

    use super::*;

    fn signature_model(id: &str, seed: u8) -> SignatureVerifyModel {
        let signing = SigningKey::from_bytes(&[seed; 32]);
        let signature = signing.sign(format!("RSCTF_TEAM_{id}").as_bytes());
        SignatureVerifyModel {
            team_token: format!(
                "{id}:{}",
                crate::utils::codec::base64_encode(&signature.to_bytes())
            ),
            public_key: crate::utils::codec::base64_encode(signing.verifying_key().as_bytes()),
        }
    }

    #[test]
    fn exact_canonical_envelope_verifies() {
        let parsed = parse_signature_envelope(&signature_model("2147483647", 7)).unwrap();
        assert_eq!(parsed.team_id, i32::MAX);
        assert!(verify_ed25519(parsed));
    }

    #[test]
    fn malformed_envelopes_fail_before_decode_or_verifier_work() {
        let valid = signature_model("7", 7);
        let unpadded_key = valid.public_key.trim_end_matches('=').to_string();
        let unpadded_signature = valid
            .team_token
            .split_once(':')
            .unwrap()
            .1
            .trim_end_matches('=')
            .to_string();
        for model in [
            SignatureVerifyModel {
                public_key: "A".repeat(1_000_000),
                team_token: valid.team_token.clone(),
            },
            SignatureVerifyModel {
                public_key: valid.public_key.clone(),
                team_token: format!("7:{}", "A".repeat(1_000_000)),
            },
            SignatureVerifyModel {
                public_key: valid.public_key.clone(),
                team_token: "7:extra:delimiter".to_string(),
            },
            signature_model("0", 7),
            SignatureVerifyModel {
                public_key: valid.public_key.clone(),
                team_token: format!("-1:{}", valid.team_token.split_once(':').unwrap().1),
            },
            SignatureVerifyModel {
                public_key: valid.public_key.clone(),
                team_token: format!("2147483648:{}", valid.team_token.split_once(':').unwrap().1),
            },
            SignatureVerifyModel {
                public_key: unpadded_key,
                team_token: valid.team_token.clone(),
            },
            SignatureVerifyModel {
                public_key: "*".repeat(44),
                team_token: valid.team_token.clone(),
            },
            SignatureVerifyModel {
                public_key: valid.public_key.clone(),
                team_token: format!("7:{unpadded_signature}"),
            },
            SignatureVerifyModel {
                public_key: valid.public_key,
                team_token: format!("7:{}", "*".repeat(88)),
            },
        ] {
            assert!(parse_signature_envelope(&model).is_err());
        }
    }

    #[test]
    fn attacker_signature_cannot_verify_under_the_canonical_key() {
        let attacker = signature_model("7", 8);
        let trusted = signature_model("7", 7);
        let attacker_signature = parse_signature_envelope(&attacker).unwrap().signature;
        let trusted_key = parse_signature_envelope(&trusted).unwrap().public_key;
        assert!(!verify_ed25519(ParsedSignature {
            team_id: 7,
            public_key: trusted_key,
            signature: attacker_signature,
        }));
    }

    #[tokio::test]
    #[ignore = "requires disposable PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn trust_anchor_requires_live_accepted_non_deleted_scope() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("team_signature_{}", Uuid::new_v4().simple());
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
            r#"CREATE TABLE "Games" (
                 id INTEGER PRIMARY KEY, public_key TEXT NOT NULL,
                 start_time_utc TIMESTAMPTZ NOT NULL,
                 end_time_utc TIMESTAMPTZ NOT NULL,
                 deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
               );
               CREATE TABLE "Teams" (
                 id INTEGER PRIMARY KEY,
                 deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
               );
               CREATE TABLE "Participations" (
                 game_id INTEGER NOT NULL, team_id INTEGER NOT NULL,
                 status SMALLINT NOT NULL
               );"#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let now = Utc::now();
        let trusted_key = signature_model("7", 7).public_key;
        sqlx::query(r#"INSERT INTO "Games" VALUES (1, $1, $2, $3, FALSE)"#)
            .bind(&trusted_key)
            .bind(now - Duration::minutes(1))
            .bind(now + Duration::minutes(1))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "Teams" VALUES (7, FALSE)"#)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "Participations" VALUES (1, 7, $1)"#)
            .bind(ParticipationStatus::Accepted as i16)
            .execute(&pool)
            .await
            .unwrap();

        assert!(trusted_team_scope(&pool, &trusted_key, 7, now)
            .await
            .unwrap());
        assert!(
            !trusted_team_scope(&pool, &signature_model("7", 8).public_key, 7, now)
                .await
                .unwrap()
        );

        for status in [
            ParticipationStatus::Pending,
            ParticipationStatus::Rejected,
            ParticipationStatus::Suspended,
        ] {
            sqlx::query(r#"UPDATE "Participations" SET status = $1"#)
                .bind(status as i16)
                .execute(&pool)
                .await
                .unwrap();
            assert!(!trusted_team_scope(&pool, &trusted_key, 7, now)
                .await
                .unwrap());
        }
        sqlx::query(r#"UPDATE "Participations" SET status = $1"#)
            .bind(ParticipationStatus::Accepted as i16)
            .execute(&pool)
            .await
            .unwrap();
        for (start, end) in [
            (now + Duration::seconds(1), now + Duration::minutes(2)),
            (now - Duration::minutes(2), now),
        ] {
            sqlx::query(r#"UPDATE "Games" SET start_time_utc = $1, end_time_utc = $2"#)
                .bind(start)
                .bind(end)
                .execute(&pool)
                .await
                .unwrap();
            assert!(!trusted_team_scope(&pool, &trusted_key, 7, now)
                .await
                .unwrap());
        }
        sqlx::query(
            r#"UPDATE "Games"
                  SET start_time_utc = $1, end_time_utc = $2,
                      deletion_pending = TRUE"#,
        )
        .bind(now - Duration::minutes(1))
        .bind(now + Duration::minutes(1))
        .execute(&pool)
        .await
        .unwrap();
        assert!(!trusted_team_scope(&pool, &trusted_key, 7, now)
            .await
            .unwrap());

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
