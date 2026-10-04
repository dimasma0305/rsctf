//! Atomic publication of account links after durable SMTP acknowledgment.

use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::controllers::account::REGISTRATION_LOCK_ID;
use crate::utils::error::{AppError, AppResult};

const LINK_LOCK_SEED: i64 = 194_207;
const MAX_RECOVERY_BATCH: i64 = 128;

fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

/// Serialize one account/purpose generation across staging, delivery, and
/// consumption. The key contains no token or destination material.
pub(crate) async fn lock_generation(
    transaction: &mut Transaction<'_, Postgres>,
    account_id: Uuid,
    purpose: i16,
) -> AppResult<()> {
    // Registration/email identity serialization always precedes the narrower
    // link-generation fence. Account controllers use this same global key.
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(REGISTRATION_LOCK_ID)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
    sqlx::query(
        r#"SELECT pg_advisory_xact_lock(
               hashtextextended($1::text || ':' || $2::text, $3)
           )"#,
    )
    .bind(account_id)
    .bind(purpose)
    .bind(LINK_LOCK_SEED)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(())
}

async fn pending_identity(
    transaction: &mut Transaction<'_, Postgres>,
    operation_id: Uuid,
) -> AppResult<Option<(Uuid, i16)>> {
    sqlx::query_as(
        r#"SELECT account_id, purpose
             FROM "AccountLinkAttempts"
            WHERE mail_operation_id = $1
              AND delivered_at_utc IS NULL
              AND consumed_at_utc IS NULL
              AND expires_at_utc > clock_timestamp()"#,
    )
    .bind(operation_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)
}

/// Acquire the global identity and per-generation fences before the caller
/// updates the outbox row. This preserves one lock order with resend staging.
pub(super) async fn fence_success(
    transaction: &mut Transaction<'_, Postgres>,
    operation_id: Uuid,
) -> AppResult<bool> {
    let Some((account_id, purpose)) = pending_identity(transaction, operation_id).await? else {
        return Ok(false);
    };
    lock_generation(transaction, account_id, purpose).await?;
    Ok(pending_identity(transaction, operation_id).await?.is_some())
}

/// Activate one link whose outbox delivery was acknowledged in this same
/// transaction. Already-activated operations are immutable no-ops, so a late
/// worker response can never reactivate an older generation.
pub(super) async fn acknowledge_fenced_success(
    transaction: &mut Transaction<'_, Postgres>,
    operation_id: Uuid,
) -> AppResult<bool> {
    let Some((account_id, purpose)) = pending_identity(transaction, operation_id).await? else {
        return Ok(false);
    };
    sqlx::query(
        r#"UPDATE "AccountLinkAttempts"
              SET is_current = FALSE
            WHERE account_id = $1 AND purpose = $2 AND is_current
              AND mail_operation_id IS DISTINCT FROM $3"#,
    )
    .bind(account_id)
    .bind(purpose)
    .bind(operation_id)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    let activated = sqlx::query(
        r#"UPDATE "AccountLinkAttempts" attempt
              SET is_current = TRUE,
                  delivered_at_utc = outbox.delivered_at_utc
             FROM "MailOutbox" outbox
            WHERE attempt.mail_operation_id = $1
              AND outbox.operation_id = $1
              AND outbox.account_id = attempt.account_id
              AND outbox.delivered_at_utc IS NOT NULL
              AND attempt.delivered_at_utc IS NULL
              AND attempt.consumed_at_utc IS NULL
              AND attempt.expires_at_utc > clock_timestamp()
              AND ((attempt.purpose = 0 AND outbox.purpose = 0)
                   OR (attempt.purpose = 1 AND outbox.purpose = 2))"#,
    )
    .bind(operation_id)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    if activated.rows_affected() != 1 {
        return Err(AppError::conflict(
            "Account-link delivery generation changed concurrently",
        ));
    }
    Ok(true)
}

async fn acknowledge_success(
    transaction: &mut Transaction<'_, Postgres>,
    operation_id: Uuid,
) -> AppResult<bool> {
    if !fence_success(transaction, operation_id).await? {
        return Ok(false);
    }
    acknowledge_fenced_success(transaction, operation_id).await
}

async fn activate_committed_operation(pool: &sqlx::PgPool, operation_id: Uuid) -> AppResult<bool> {
    let mut transaction = pool.begin().await.map_err(database_error)?;
    let activated = acknowledge_success(&mut transaction, operation_id).await?;
    transaction.commit().await.map_err(database_error)?;
    Ok(activated)
}

/// Recover successful deliveries committed by an older rolling-upgrade worker.
/// The query and work are bounded; exact and concurrent passes are harmless.
pub(super) async fn recover_committed(pool: &sqlx::PgPool, limit: i64) -> AppResult<usize> {
    let operation_ids: Vec<Uuid> = sqlx::query_scalar(
        r#"SELECT attempt.mail_operation_id
             FROM "AccountLinkAttempts" attempt
             JOIN "MailOutbox" outbox
               ON outbox.operation_id = attempt.mail_operation_id
              AND outbox.account_id = attempt.account_id
            WHERE attempt.mail_operation_id IS NOT NULL
              AND attempt.delivered_at_utc IS NULL
              AND attempt.consumed_at_utc IS NULL
              AND attempt.expires_at_utc > clock_timestamp()
              AND outbox.delivered_at_utc IS NOT NULL
              AND ((attempt.purpose = 0 AND outbox.purpose = 0)
                   OR (attempt.purpose = 1 AND outbox.purpose = 2))
            ORDER BY outbox.delivered_at_utc, attempt.mail_operation_id
            LIMIT $1"#,
    )
    .bind(limit.clamp(1, MAX_RECOVERY_BATCH))
    .fetch_all(pool)
    .await
    .map_err(database_error)?;
    let mut activated = 0;
    for operation_id in operation_ids {
        if activate_committed_operation(pool, operation_id).await? {
            activated += 1;
        }
    }
    Ok(activated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use std::str::FromStr;

    #[test]
    fn activation_contract_never_reads_or_stores_plaintext_tokens() {
        let source = include_str!("link_activation.rs");
        assert!(source.contains("mail_operation_id"));
        assert!(source.contains("delivered_at_utc IS NOT NULL"));
        assert!(source.contains("LIMIT $1"));
        assert!(!source.contains(&["token ", "="].concat()));
        assert!(!source.contains(&["destination ", "="].concat()));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn delivery_ack_is_atomic_and_concurrent_recovery_never_reactivates_old_links() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("mail_link_activation_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(6)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"CREATE TABLE "AspNetUsers" (id UUID PRIMARY KEY);
               CREATE TABLE "MailOutbox" (
                 operation_id UUID PRIMARY KEY,
                 purpose SMALLINT NOT NULL,
                 account_id UUID NOT NULL REFERENCES "AspNetUsers"(id),
                 delivered_at_utc TIMESTAMPTZ
               );
               CREATE TABLE "AccountLinkAttempts" (
                 token_digest BYTEA PRIMARY KEY,
                 purpose SMALLINT NOT NULL,
                 account_id UUID NOT NULL REFERENCES "AspNetUsers"(id),
                 generation BIGINT NOT NULL,
                 security_generation_digest BYTEA NOT NULL,
                 destination_digest BYTEA NOT NULL,
                 expires_at_utc TIMESTAMPTZ NOT NULL,
                 is_current BOOLEAN NOT NULL DEFAULT FALSE,
                 delivered_at_utc TIMESTAMPTZ,
                 consumed_at_utc TIMESTAMPTZ,
                 result JSONB,
                 created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                 mail_operation_id UUID UNIQUE REFERENCES "MailOutbox"(operation_id)
               );
               CREATE UNIQUE INDEX one_current_link
                 ON "AccountLinkAttempts" (account_id, purpose)
                 WHERE is_current AND consumed_at_utc IS NULL;"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let account_id = Uuid::new_v4();
        sqlx::query(r#"INSERT INTO "AspNetUsers" (id) VALUES ($1)"#)
            .bind(account_id)
            .execute(&pool)
            .await
            .unwrap();
        let old_operation = Uuid::new_v4();
        let candidate_operation = Uuid::new_v4();
        for (operation_id, delivered) in [(old_operation, true), (candidate_operation, false)] {
            sqlx::query(
                r#"INSERT INTO "MailOutbox"
                     (operation_id,purpose,account_id,delivered_at_utc)
                   VALUES ($1,2,$2,CASE WHEN $3 THEN clock_timestamp() ELSE NULL END)"#,
            )
            .bind(operation_id)
            .bind(account_id)
            .bind(delivered)
            .execute(&pool)
            .await
            .unwrap();
        }
        let digest = |value: &[u8]| Sha256::digest(value).to_vec();
        for (generation, operation_id, token, current, delivered) in [
            (1_i64, old_operation, b"old".as_slice(), true, true),
            (
                2_i64,
                candidate_operation,
                b"candidate".as_slice(),
                false,
                false,
            ),
        ] {
            sqlx::query(
                r#"INSERT INTO "AccountLinkAttempts"
                     (token_digest,purpose,account_id,generation,
                      security_generation_digest,destination_digest,
                      expires_at_utc,is_current,delivered_at_utc,mail_operation_id)
                   VALUES ($1,1,$2,$3,$4,$5,clock_timestamp()+INTERVAL '1 hour',$6,
                           CASE WHEN $7 THEN clock_timestamp() ELSE NULL END,$8)"#,
            )
            .bind(digest(token))
            .bind(account_id)
            .bind(generation)
            .bind(digest(b"stamp"))
            .bind(digest(b"MAIL@EXAMPLE.TEST"))
            .bind(current)
            .bind(delivered)
            .bind(operation_id)
            .execute(&pool)
            .await
            .unwrap();
        }

        assert_eq!(recover_committed(&pool, 128).await.unwrap(), 0);
        let current_before_delivery: Vec<i64> =
            sqlx::query_scalar(r#"SELECT generation FROM "AccountLinkAttempts" WHERE is_current"#)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(current_before_delivery, vec![1]);

        sqlx::raw_sql(
            r#"CREATE FUNCTION reject_link_activation()
               RETURNS trigger LANGUAGE plpgsql AS $$
               BEGIN
                 IF NEW.is_current AND NOT OLD.is_current THEN
                   RAISE EXCEPTION 'synthetic activation failure';
                 END IF;
                 RETURN NEW;
               END $$;
               CREATE TRIGGER reject_link_activation
                 BEFORE UPDATE ON "AccountLinkAttempts"
                 FOR EACH ROW EXECUTE FUNCTION reject_link_activation();"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let mut failed_ack = pool.begin().await.unwrap();
        sqlx::query(
            r#"UPDATE "MailOutbox" SET delivered_at_utc=clock_timestamp()
                WHERE operation_id=$1"#,
        )
        .bind(candidate_operation)
        .execute(&mut *failed_ack)
        .await
        .unwrap();
        assert!(acknowledge_success(&mut failed_ack, candidate_operation)
            .await
            .is_err());
        failed_ack.rollback().await.unwrap();
        let rolled_back: (bool, i64) = sqlx::query_as(
            r#"SELECT outbox.delivered_at_utc IS NULL,
                      (SELECT generation FROM "AccountLinkAttempts" WHERE is_current)
                 FROM "MailOutbox" outbox WHERE operation_id=$1"#,
        )
        .bind(candidate_operation)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(rolled_back, (true, 1));
        sqlx::raw_sql(
            r#"DROP TRIGGER reject_link_activation ON "AccountLinkAttempts";
               DROP FUNCTION reject_link_activation();"#,
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            r#"UPDATE "MailOutbox" SET delivered_at_utc=clock_timestamp()
                WHERE operation_id=$1"#,
        )
        .bind(candidate_operation)
        .execute(&pool)
        .await
        .unwrap();
        let (left, right) =
            tokio::join!(recover_committed(&pool, 128), recover_committed(&pool, 128));
        assert_eq!(left.unwrap() + right.unwrap(), 1);
        let current_after_delivery: Vec<i64> =
            sqlx::query_scalar(r#"SELECT generation FROM "AccountLinkAttempts" WHERE is_current"#)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(current_after_delivery, vec![2]);

        let newer_operation = Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO "MailOutbox"
                 (operation_id,purpose,account_id,delivered_at_utc)
               VALUES ($1,2,$2,clock_timestamp())"#,
        )
        .bind(newer_operation)
        .bind(account_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "AccountLinkAttempts"
                 (token_digest,purpose,account_id,generation,
                  security_generation_digest,destination_digest,
                  expires_at_utc,is_current,mail_operation_id)
               VALUES ($1,1,$2,3,$3,$4,clock_timestamp()+INTERVAL '1 hour',FALSE,$5)"#,
        )
        .bind(digest(b"newer"))
        .bind(account_id)
        .bind(digest(b"stamp"))
        .bind(digest(b"MAIL@EXAMPLE.TEST"))
        .bind(newer_operation)
        .execute(&pool)
        .await
        .unwrap();
        assert!(activate_committed_operation(&pool, newer_operation)
            .await
            .unwrap());
        assert!(!activate_committed_operation(&pool, candidate_operation)
            .await
            .unwrap());
        let current_after_old_replay: Vec<i64> =
            sqlx::query_scalar(r#"SELECT generation FROM "AccountLinkAttempts" WHERE is_current"#)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(current_after_old_replay, vec![3]);

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
