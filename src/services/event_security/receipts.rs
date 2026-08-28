use base64::Engine;
use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::LazyLock;
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::utils::enums::SolveReceiptMode;
use crate::utils::error::{AppError, AppResult};

pub const RECEIPT_TTL_SECONDS: i64 = 10 * 60;
const RECEIPT_AUDIT_RETENTION_DAYS: i64 = 365;
const RECEIPT_MAINTENANCE_BATCH: i64 = 500;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReceiptClaims {
    purpose: String,
    receipt_id: Uuid,
    game_id: i32,
    challenge_id: i32,
    participation_id: i32,
    user_id: Option<Uuid>,
    variant_id: Option<Uuid>,
    answer_hash: String,
    issuer_identity: String,
    nonce: Uuid,
    issued_at: i64,
    expires_at: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueSolveReceipt {
    pub attempt_id: Uuid,
    pub game_id: i32,
    pub challenge_id: i32,
    pub participation_id: i32,
    pub user_id: Option<Uuid>,
    pub variant_id: Option<Uuid>,
    pub answer: String,
    pub issuer_identity: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IssuedSolveReceipt {
    pub proof: String,
    #[serde(with = "crate::utils::datetime::millis")]
    pub expires_at_utc: DateTime<Utc>,
}

pub struct ValidatedReceipt {
    id: Uuid,
}

fn receipt_key(secret: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"rsctf:solve-receipt:token:v1\0");
    digest.update(secret.as_bytes());
    digest.finalize().into()
}

fn answer_hash(secret: &str, game_id: i32, challenge_id: i32, answer: &str) -> AppResult<[u8; 32]> {
    super::validate_credential_key(secret)?;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .map_err(|_| AppError::internal("initialize solve receipt answer hash"))?;
    mac.update(b"rsctf:solve-receipt:answer:v1\0");
    mac.update(&game_id.to_be_bytes());
    mac.update(&challenge_id.to_be_bytes());
    mac.update(answer.as_bytes());
    Ok(mac.finalize().into_bytes().into())
}

fn encode<T: Serialize>(secret: &str, claims: &T) -> AppResult<String> {
    super::validate_credential_key(secret)?;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(claims)
            .map_err(|error| AppError::internal(format!("encode solve receipt: {error}")))?,
    );
    let mut mac = Hmac::<Sha256>::new_from_slice(&receipt_key(secret))
        .map_err(|_| AppError::internal("initialize solve receipt signer"))?;
    mac.update(payload.as_bytes());
    let signature =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    Ok(format!("{payload}.{signature}"))
}

fn decode<T: DeserializeOwned>(secret: &str, token: &str) -> AppResult<T> {
    if token.len() > 4096 {
        return Err(AppError::bad_request("Solve receipt is too large"));
    }
    let (payload, signature) = token
        .split_once('.')
        .ok_or_else(|| AppError::bad_request("Invalid solve receipt"))?;
    let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|_| AppError::bad_request("Invalid solve receipt"))?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&receipt_key(secret))
        .map_err(|_| AppError::bad_request("Invalid solve receipt"))?;
    mac.update(payload.as_bytes());
    mac.verify_slice(&signature)
        .map_err(|_| AppError::bad_request("Invalid solve receipt"))?;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| AppError::bad_request("Invalid solve receipt"))?;
    serde_json::from_slice(&payload).map_err(|_| AppError::bad_request("Invalid solve receipt"))
}

fn token_hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

fn verifier_attempt_hash(secret: &str, issuer: &str, attempt_id: Uuid) -> AppResult<[u8; 32]> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .map_err(|_| AppError::internal("initialize receipt attempt hash"))?;
    mac.update(b"rsctf:solve-receipt:attempt:v1\0");
    mac.update(issuer.as_bytes());
    mac.update(attempt_id.as_bytes());
    Ok(mac.finalize().into_bytes().into())
}

fn deterministic_uuid(secret: &str, domain: &[u8], attempt_hash: [u8; 32]) -> AppResult<Uuid> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .map_err(|_| AppError::internal("initialize receipt identifier"))?;
    mac.update(domain);
    mac.update(&attempt_hash);
    let digest = mac.finalize().into_bytes();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    Ok(Uuid::from_bytes(bytes))
}

static RECEIPT_ISSUANCE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(64);
static RECEIPT_ISSUER_SHARDS: LazyLock<[tokio::sync::Semaphore; 256]> =
    LazyLock::new(|| std::array::from_fn(|_| tokio::sync::Semaphore::new(4)));

fn issuer_shard(issuer: &str, participation_id: i32) -> usize {
    let mut hasher = DefaultHasher::new();
    issuer.hash(&mut hasher);
    participation_id.hash(&mut hasher);
    hasher.finish() as usize % RECEIPT_ISSUER_SHARDS.len()
}

pub async fn issue_solve_receipt(
    st: &SharedState,
    request: IssueSolveReceipt,
) -> AppResult<IssuedSolveReceipt> {
    super::validate_credential_key(&st.config.event_vpn_credential_key)?;
    if request.attempt_id.is_nil() {
        return Err(AppError::bad_request("Invalid solve receipt attempt"));
    }
    let _permit = RECEIPT_ISSUANCE
        .try_acquire()
        .map_err(|_| AppError::too_many_requests(1))?;
    let issuer = request.issuer_identity.trim();
    if !(1..=128).contains(&issuer.len()) || request.answer.is_empty() || request.answer.len() > 127
    {
        return Err(AppError::bad_request("Invalid solve receipt request"));
    }
    crate::middlewares::rate_limiter::admit_solve_receipt_issuance(
        issuer,
        request.participation_id,
    )
    .await
    .map_err(AppError::too_many_requests)?;
    let policy: Option<(i16, Option<String>, Option<Uuid>)> = sqlx::query_as(
        r#"SELECT challenge.solve_receipt_mode,
                  challenge.receipt_verifier_identity,
                  variant.id
             FROM "GameChallenges" challenge
             JOIN "Participations" participation
               ON participation.game_id = challenge.game_id
              AND participation.id = $3
             LEFT JOIN "ChallengeVariants" variant
               ON variant.game_id = challenge.game_id
              AND variant.challenge_id = challenge.id
              AND variant.participation_id = participation.id
              AND variant.frozen_at_utc IS NOT NULL
            WHERE challenge.game_id = $1 AND challenge.id = $2
              AND challenge.is_enabled = TRUE AND challenge.review_status = 0"#,
    )
    .bind(request.game_id)
    .bind(request.challenge_id)
    .bind(request.participation_id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((mode, expected_issuer, canonical_variant)) = policy else {
        return Err(AppError::not_found("Challenge or participation not found"));
    };
    if mode == SolveReceiptMode::Disabled as i16
        || expected_issuer.as_deref() != Some(issuer)
        || request.variant_id != canonical_variant
    {
        return Err(AppError::Forbidden);
    }
    let _issuer_permit = RECEIPT_ISSUER_SHARDS[issuer_shard(issuer, request.participation_id)]
        .try_acquire()
        .map_err(|_| AppError::too_many_requests(1))?;
    let attempt_hash = verifier_attempt_hash(
        &st.config.event_vpn_credential_key,
        issuer,
        request.attempt_id,
    )?;
    let receipt_id = deterministic_uuid(
        &st.config.event_vpn_credential_key,
        b"rsctf:solve-receipt:id:v1\0",
        attempt_hash,
    )?;
    let nonce = deterministic_uuid(
        &st.config.event_vpn_credential_key,
        b"rsctf:solve-receipt:nonce:v1\0",
        attempt_hash,
    )?;
    let answer_hash = answer_hash(
        &st.config.event_vpn_credential_key,
        request.game_id,
        request.challenge_id,
        &request.answer,
    )?;
    let issued = sqlx::query_as::<_, (Uuid, DateTime<Utc>, DateTime<Utc>)>(
        r#"INSERT INTO "SolveReceipts"
             (id, game_id, challenge_id, participation_id, user_id, variant_id,
              answer_hash, issuer_identity, token_hash, nonce_hash, attempt_hash,
              issued_at_utc, expires_at_utc)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8,
                   $10, $9, $10,
                   date_trunc('second', clock_timestamp()),
                   date_trunc('second', clock_timestamp()) + ($11 * interval '1 second'))
           ON CONFLICT (attempt_hash) DO UPDATE
               SET attempt_hash = EXCLUDED.attempt_hash
             WHERE "SolveReceipts".id = EXCLUDED.id
               AND "SolveReceipts".game_id = EXCLUDED.game_id
               AND "SolveReceipts".challenge_id = EXCLUDED.challenge_id
               AND "SolveReceipts".participation_id = EXCLUDED.participation_id
               AND "SolveReceipts".user_id IS NOT DISTINCT FROM EXCLUDED.user_id
               AND "SolveReceipts".variant_id IS NOT DISTINCT FROM EXCLUDED.variant_id
               AND "SolveReceipts".answer_hash = EXCLUDED.answer_hash
               AND "SolveReceipts".issuer_identity = EXCLUDED.issuer_identity
         RETURNING id, issued_at_utc, expires_at_utc"#,
    )
    .bind(receipt_id)
    .bind(request.game_id)
    .bind(request.challenge_id)
    .bind(request.participation_id)
    .bind(request.user_id)
    .bind(request.variant_id)
    .bind(answer_hash.as_slice())
    .bind(issuer)
    .bind(Sha256::digest(nonce.as_bytes()).as_slice())
    .bind(attempt_hash.as_slice())
    .bind(RECEIPT_TTL_SECONDS)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::conflict("Solve receipt attempt was reused with different input"))?;
    let claims = ReceiptClaims {
        purpose: "solve-receipt".to_string(),
        receipt_id,
        game_id: request.game_id,
        challenge_id: request.challenge_id,
        participation_id: request.participation_id,
        user_id: request.user_id,
        variant_id: request.variant_id,
        answer_hash: hex::encode(answer_hash),
        issuer_identity: issuer.to_string(),
        nonce,
        issued_at: issued.1.timestamp(),
        expires_at: issued.2.timestamp(),
    };
    let proof = encode(&st.config.event_vpn_credential_key, &claims)?;
    sqlx::query(r#"UPDATE "SolveReceipts" SET token_hash = $2 WHERE id = $1"#)
        .bind(receipt_id)
        .bind(token_hash(&proof).as_slice())
        .execute(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(IssuedSolveReceipt {
        proof,
        expires_at_utc: DateTime::from_timestamp(claims.expires_at, 0)
            .ok_or_else(|| AppError::internal("invalid solve receipt expiry"))?,
    })
}

#[allow(clippy::too_many_arguments)]
pub async fn validate_receipt_for_submission(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    secret: &str,
    proof: Option<&str>,
    mode: SolveReceiptMode,
    game_id: i32,
    challenge_id: i32,
    participation_id: i32,
    user_id: Uuid,
    answer: &str,
) -> AppResult<Option<ValidatedReceipt>> {
    if mode == SolveReceiptMode::Disabled {
        if proof.is_some() {
            return Err(AppError::bad_request(
                "This challenge does not accept solve receipts",
            ));
        }
        return Ok(None);
    }
    let Some(proof) = proof else {
        return if mode == SolveReceiptMode::Required {
            Err(AppError::bad_request(
                "A trusted solve receipt is required for this challenge",
            ))
        } else {
            Ok(None)
        };
    };
    let claims: ReceiptClaims = decode(secret, proof)?;
    let now = Utc::now().timestamp();
    let expected_answer = answer_hash(secret, game_id, challenge_id, answer)?;
    if claims.purpose != "solve-receipt"
        || claims.game_id != game_id
        || claims.challenge_id != challenge_id
        || claims.participation_id != participation_id
        || claims.user_id.is_some_and(|expected| expected != user_id)
        || claims.answer_hash != hex::encode(expected_answer)
        || claims.issued_at > now + 5
        || claims.expires_at < now
        || claims.expires_at - claims.issued_at > RECEIPT_TTL_SECONDS
    {
        return Err(AppError::bad_request(
            "Solve receipt does not match this submission",
        ));
    }
    let valid = sqlx::query_scalar::<_, Uuid>(
        r#"SELECT id FROM "SolveReceipts"
            WHERE id = $1 AND game_id = $2 AND challenge_id = $3
              AND participation_id = $4
              AND (user_id IS NULL OR user_id = $5)
              AND answer_hash = $6 AND token_hash = $7
              AND issued_at_utc = to_timestamp($8)
              AND expires_at_utc = to_timestamp($9)
              AND expires_at_utc >= clock_timestamp()
              AND consumed_at_utc IS NULL
            FOR UPDATE"#,
    )
    .bind(claims.receipt_id)
    .bind(game_id)
    .bind(challenge_id)
    .bind(participation_id)
    .bind(user_id)
    .bind(expected_answer.as_slice())
    .bind(token_hash(proof).as_slice())
    .bind(claims.issued_at)
    .bind(claims.expires_at)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if valid.is_none() {
        return Err(AppError::bad_request(
            "Solve receipt is expired or already used",
        ));
    }
    Ok(Some(ValidatedReceipt {
        id: claims.receipt_id,
    }))
}

pub async fn consume_receipt(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    receipt: ValidatedReceipt,
    submission_id: i32,
) -> AppResult<()> {
    let updated = sqlx::query(
        r#"WITH archived AS (
               INSERT INTO "SolveReceiptAudit"
                    (receipt_id, game_id, challenge_id, participation_id, user_id,
                     variant_id, issuer_identity, attempt_hash, token_hash,
                     consumed_submission_id, issued_at_utc, consumed_at_utc)
               SELECT id, game_id, challenge_id, participation_id, user_id,
                      variant_id, issuer_identity, attempt_hash, token_hash,
                      $2, issued_at_utc, clock_timestamp()
                 FROM "SolveReceipts" WHERE id = $1 AND consumed_at_utc IS NULL
               ON CONFLICT (receipt_id) DO NOTHING
               RETURNING receipt_id
           )
           DELETE FROM "SolveReceipts" receipt USING archived
            WHERE receipt.id = archived.receipt_id"#,
    )
    .bind(receipt.id)
    .bind(submission_id)
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if updated.rows_affected() != 1 {
        return Err(AppError::conflict(
            "Solve receipt was consumed concurrently",
        ));
    }
    Ok(())
}

async fn cleanup_solve_receipt_audit(pool: &sqlx::PgPool) -> Result<u64, sqlx::Error> {
    sqlx::query(
        r#"WITH expired AS (
               SELECT receipt_id FROM "SolveReceiptAudit"
                WHERE consumed_at_utc < clock_timestamp() - ($1 * interval '1 day')
             ORDER BY consumed_at_utc, receipt_id
                FOR UPDATE SKIP LOCKED LIMIT $2
           )
           DELETE FROM "SolveReceiptAudit" audit USING expired
            WHERE audit.receipt_id = expired.receipt_id"#,
    )
    .bind(RECEIPT_AUDIT_RETENTION_DAYS)
    .bind(RECEIPT_MAINTENANCE_BATCH)
    .execute(pool)
    .await
    .map(|result| result.rows_affected())
}

/// Keeps expired one-use secrets out of the hot lookup index in bounded
/// batches. Consumed provenance is moved synchronously to the audit table.
pub fn start_receipt_maintenance(
    st: SharedState,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { break; }
                }
                _ = interval.tick() => {
                    if let Err(error) = sqlx::query(
                        r#"WITH expired AS (
                               SELECT id FROM "SolveReceipts"
                                WHERE expires_at_utc < clock_timestamp()
                             ORDER BY expires_at_utc, id
                                FOR UPDATE SKIP LOCKED LIMIT 500
                           )
                           DELETE FROM "SolveReceipts" receipt USING expired
                            WHERE receipt.id = expired.id"#,
                    )
                    .execute(st.pg())
                    .await
                    {
                        tracing::warn!(%error, "expired solve-receipt cleanup failed");
                    }
                    if let Err(error) = sqlx::query(
                        r#"WITH expired AS (
                               SELECT batch_id FROM "EventTelemetryBatches"
                                WHERE created_at_utc < clock_timestamp() - interval '7 days'
                             ORDER BY created_at_utc, batch_id
                                FOR UPDATE SKIP LOCKED LIMIT 500
                           )
                           DELETE FROM "EventTelemetryBatches" batch USING expired
                            WHERE batch.batch_id = expired.batch_id"#,
                    )
                    .execute(st.pg())
                    .await
                    {
                        tracing::warn!(%error, "event sensor batch cleanup failed");
                    }
                    if let Err(error) = cleanup_solve_receipt_audit(st.pg()).await {
                        tracing::warn!(%error, "solve-receipt audit retention cleanup failed");
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use std::str::FromStr;

    const KEY: &str = "solve-receipt-test-key-0123456789ab";

    #[test]
    fn receipt_token_is_bound_to_answer_and_ten_minute_expiry() {
        let answer = answer_hash(KEY, 1, 2, "flag{a}").unwrap();
        assert_ne!(answer, answer_hash(KEY, 1, 2, "flag{b}").unwrap());
        let claims = ReceiptClaims {
            purpose: "solve-receipt".into(),
            receipt_id: Uuid::new_v4(),
            game_id: 1,
            challenge_id: 2,
            participation_id: 3,
            user_id: None,
            variant_id: None,
            answer_hash: hex::encode(answer),
            issuer_identity: "verifier".into(),
            nonce: Uuid::new_v4(),
            issued_at: 10,
            expires_at: 10 + RECEIPT_TTL_SECONDS,
        };
        let token = encode(KEY, &claims).unwrap();
        let decoded: ReceiptClaims = decode(KEY, &token).unwrap();
        assert_eq!(decoded.answer_hash, claims.answer_hash);
        assert_eq!(decoded.expires_at - decoded.issued_at, 600);
    }

    #[test]
    fn verifier_attempts_derive_stable_opaque_identifiers() {
        let attempt = Uuid::new_v4();
        assert_eq!(
            verifier_attempt_hash(KEY, "verifier", attempt).unwrap(),
            verifier_attempt_hash(KEY, "verifier", attempt).unwrap()
        );
        assert_ne!(
            verifier_attempt_hash(KEY, "verifier", attempt).unwrap(),
            verifier_attempt_hash(KEY, "other", attempt).unwrap()
        );
        assert_eq!(issuer_shard("verifier", 7), issuer_shard("verifier", 7));

        let source = include_str!("receipts.rs");
        let issuance = source
            .split("pub async fn issue_solve_receipt")
            .nth(1)
            .expect("receipt issuance source");
        assert!(
            issuance
                .find("admit_solve_receipt_issuance")
                .expect("admission")
                < issuance.find("let policy:").expect("policy query"),
            "receipt database lookup must remain behind bounded admission"
        );
    }

    #[tokio::test]
    #[ignore = "requires disposable PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn receipt_audit_retention_is_bounded_and_preserves_recent_rows() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("receipt_retention_{}", Uuid::new_v4().simple());
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
            r#"CREATE TABLE "SolveReceiptAudit" (
                   receipt_id UUID PRIMARY KEY,
                   consumed_at_utc TIMESTAMPTZ NOT NULL
               );
               CREATE INDEX ix_solve_receipt_audit_retention
                   ON "SolveReceiptAudit" (consumed_at_utc, receipt_id);"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        for _ in 0..501 {
            sqlx::query(
                r#"INSERT INTO "SolveReceiptAudit" (receipt_id, consumed_at_utc)
                   VALUES ($1, clock_timestamp() - interval '366 days')"#,
            )
            .bind(Uuid::new_v4())
            .execute(&pool)
            .await
            .unwrap();
        }
        let recent = Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO "SolveReceiptAudit" (receipt_id, consumed_at_utc)
               VALUES ($1, clock_timestamp())"#,
        )
        .bind(recent)
        .execute(&pool)
        .await
        .unwrap();

        assert_eq!(cleanup_solve_receipt_audit(&pool).await.unwrap(), 500);
        assert_eq!(cleanup_solve_receipt_audit(&pool).await.unwrap(), 1);
        assert!(sqlx::query_scalar::<_, bool>(
            r#"SELECT EXISTS (SELECT 1 FROM "SolveReceiptAudit" WHERE receipt_id = $1)"#,
        )
        .bind(recent)
        .fetch_one(&pool)
        .await
        .unwrap());

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
