//! Durable, bounded account-mail admission and SMTP delivery.
//!
//! API requests only persist an intent. A shutdown-aware worker claims at most
//! four database-backed delivery slots across every replica, then performs SMTP
//! I/O without retaining a database connection or transaction.

use std::time::Duration;

use futures::{stream, StreamExt};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::services::mail::MailSender;
use crate::utils::error::{AppError, AppResult};

const MAIL_ADMISSION_LOCK_ID: i64 = 0x5253_4354_464d_4149; // "RSCTFMAI"
const MAX_ACTIVE_MESSAGES: i64 = 4_096;
const MAX_ACTIVE_MESSAGE_BYTES: i64 = 64 * 1024 * 1024;
const MAX_STORED_ROWS: i64 = 65_536;
const MAX_RECENT_PER_ACCOUNT: i64 = 8;
const MAX_RECENT_PER_DESTINATION: i64 = 8;
const MAX_RECENT_PER_SOURCE: i64 = 20;
const ADMISSION_WINDOW_SECONDS: i64 = 5 * 60;
const MAX_ATTEMPTS: i16 = 8;
const LEASE_SECONDS: i64 = 30;
const CLAIM_LIMIT: i64 = 4;
const MAX_CONCURRENT_DELIVERIES: usize = 4;
const TERMINAL_RETENTION_SECONDS: i64 = 7 * 24 * 60 * 60;
const MAINTENANCE_LIMIT: i64 = 128;
const POLL_INTERVAL: Duration = Duration::from_secs(1);
const CLEANUP_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i16)]
pub enum MailPurpose {
    RegistrationConfirmation = 0,
    PasswordRecovery = 1,
    EmailChange = 2,
}

#[derive(Debug)]
pub struct MailIntent<'a> {
    pub operation_id: Uuid,
    pub purpose: MailPurpose,
    pub account_id: Uuid,
    pub security_generation: &'a str,
    pub destination: &'a str,
    pub source: Option<&'a str>,
    pub subject: &'a str,
    pub html_body: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnqueueOutcome {
    Inserted,
    Replayed,
}

#[derive(Debug, sqlx::FromRow)]
struct LeasedMail {
    operation_id: Uuid,
    destination: String,
    subject: String,
    html_body: String,
    attempts: i16,
    delivery_slot: i16,
}

fn digest(value: impl AsRef<[u8]>) -> Vec<u8> {
    Sha256::digest(value.as_ref()).to_vec()
}

fn request_digest(intent: &MailIntent<'_>, generation: &[u8], destination: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(b"rsctf-mail-intent-v1\0");
    hasher.update((intent.purpose as i16).to_be_bytes());
    hasher.update(intent.account_id.as_bytes());
    hasher.update(generation);
    hasher.update(destination);
    hasher.finalize().to_vec()
}

fn validate_intent(intent: &MailIntent<'_>) -> AppResult<()> {
    crate::services::mail::validate_recipient(intent.destination)?;
    if intent.destination.as_bytes().len() > 320
        || intent.subject.is_empty()
        || intent.subject.as_bytes().len() > 256
        || intent.html_body.is_empty()
        || intent.html_body.as_bytes().len() > 65_536
        || intent.security_generation.as_bytes().len() > 256
        || intent
            .source
            .is_some_and(|source| source.as_bytes().len() > 256)
    {
        return Err(AppError::bad_request(
            "Mail intent exceeds supported limits",
        ));
    }
    Ok(())
}

/// Persist one canonical intent inside the caller's account transaction.
///
/// The operation identity is exact-replay safe. A different operation for the
/// same account and purpose supersedes older links before the new row becomes
/// visible. Admission is serialized only for this low-volume mutation so the
/// deployment-wide count and byte ceilings cannot be raced by replicas.
pub async fn enqueue_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    intent: MailIntent<'_>,
) -> AppResult<EnqueueOutcome> {
    validate_intent(&intent)?;
    let normalized_destination = intent.destination.trim().to_lowercase();
    let generation_digest = digest(intent.security_generation);
    let destination_digest = digest(&normalized_destination);
    let source_digest = intent.source.map(|source| digest(source.trim()));
    let canonical_request_digest = request_digest(&intent, &generation_digest, &destination_digest);

    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(MAIL_ADMISSION_LOCK_ID)
        .execute(&mut **transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    let existing: Option<(i16, Uuid, Vec<u8>)> = sqlx::query_as(
        r#"SELECT purpose, account_id, request_digest
             FROM "MailOutbox"
            WHERE operation_id = $1
            FOR UPDATE"#,
    )
    .bind(intent.operation_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if let Some((purpose, account_id, stored_digest)) = existing {
        if purpose == intent.purpose as i16
            && account_id == intent.account_id
            && stored_digest == canonical_request_digest
        {
            return Ok(EnqueueOutcome::Replayed);
        }
        return Err(AppError::conflict(
            "Mail operation identity was already used for another request",
        ));
    }

    let (
        active_count,
        active_bytes,
        total_count,
        account_recent,
        destination_recent,
        source_recent,
    ): (i64, i64, i64, i64, i64, i64) = sqlx::query_as(
        r#"SELECT active.message_count, active.message_bytes,
                  (SELECT COUNT(*)::BIGINT FROM "MailOutbox"),
                  (SELECT COUNT(*)::BIGINT
                     FROM "MailOutbox"
                    WHERE account_id = $1
                      AND created_at_utc >= clock_timestamp()
                          - ($4::BIGINT * INTERVAL '1 second')),
                  (SELECT COUNT(*)::BIGINT
                     FROM "MailOutbox"
                    WHERE destination_digest = $2
                      AND created_at_utc >= clock_timestamp()
                          - ($4::BIGINT * INTERVAL '1 second')),
                  (SELECT COUNT(*)::BIGINT
                     FROM "MailOutbox"
                    WHERE $3::BYTEA IS NOT NULL
                      AND source_digest = $3
                      AND created_at_utc >= clock_timestamp()
                          - ($4::BIGINT * INTERVAL '1 second'))
             FROM (
                 SELECT COUNT(*)::BIGINT AS message_count,
                        COALESCE(SUM(octet_length(destination)
                                     + octet_length(subject)
                                     + octet_length(html_body)), 0)::BIGINT AS message_bytes
                   FROM "MailOutbox"
                  WHERE delivered_at_utc IS NULL AND dead_at_utc IS NULL
             ) active"#,
    )
    .bind(intent.account_id)
    .bind(&destination_digest)
    .bind(&source_digest)
    .bind(ADMISSION_WINDOW_SECONDS)
    .fetch_one(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let message_bytes = i64::try_from(
        normalized_destination.as_bytes().len()
            + intent.subject.as_bytes().len()
            + intent.html_body.as_bytes().len(),
    )
    .unwrap_or(i64::MAX);
    if active_count >= MAX_ACTIVE_MESSAGES
        || active_bytes.saturating_add(message_bytes) > MAX_ACTIVE_MESSAGE_BYTES
        || total_count >= MAX_STORED_ROWS
        || account_recent >= MAX_RECENT_PER_ACCOUNT
        || destination_recent >= MAX_RECENT_PER_DESTINATION
        || (source_digest.is_some() && source_recent >= MAX_RECENT_PER_SOURCE)
    {
        return Err(AppError::retry_after(
            u64::try_from(ADMISSION_WINDOW_SECONDS).unwrap_or(300),
        ));
    }

    // A delivery that already owns a slot may be in SMTP I/O. Mark it
    // superseded now and let the worker observe that state before transmission
    // or at lease completion; unleased messages can be made terminal here.
    sqlx::query(
        r#"UPDATE "MailOutbox"
              SET superseded_at_utc = clock_timestamp(),
                  dead_at_utc = CASE
                      WHEN delivered_at_utc IS NULL AND lease_token IS NULL
                      THEN clock_timestamp()
                      ELSE dead_at_utc
                  END,
                  last_error = CASE
                      WHEN delivered_at_utc IS NULL AND lease_token IS NULL
                      THEN 'superseded'
                      ELSE last_error
                  END,
                  html_body = CASE
                      WHEN delivered_at_utc IS NULL AND lease_token IS NULL
                      THEN ''
                      ELSE html_body
                  END
            WHERE account_id = $1
              AND purpose = $2
              AND operation_id <> $3
              AND superseded_at_utc IS NULL"#,
    )
    .bind(intent.account_id)
    .bind(intent.purpose as i16)
    .bind(intent.operation_id)
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    sqlx::query(
        r#"INSERT INTO "MailOutbox"
             (operation_id, purpose, account_id,
              security_generation_digest, destination, destination_digest,
              source_digest, request_digest, subject, html_body)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"#,
    )
    .bind(intent.operation_id)
    .bind(intent.purpose as i16)
    .bind(intent.account_id)
    .bind(generation_digest)
    .bind(normalized_destination)
    .bind(destination_digest)
    .bind(source_digest)
    .bind(canonical_request_digest)
    .bind(intent.subject)
    .bind(intent.html_body)
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(EnqueueOutcome::Inserted)
}

async fn claim_pending(pool: &sqlx::PgPool, limit: i64) -> AppResult<(Uuid, Vec<LeasedMail>)> {
    let lease_token = Uuid::new_v4();
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let slots: Vec<i16> = sqlx::query_scalar(
        r#"SELECT slot_id
             FROM "MailDeliverySlots"
            WHERE lease_expires_at_utc IS NULL
               OR lease_expires_at_utc <= clock_timestamp()
            ORDER BY slot_id
            LIMIT $1
            FOR UPDATE SKIP LOCKED"#,
    )
    .bind(limit.clamp(1, CLAIM_LIMIT))
    .fetch_all(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if slots.is_empty() {
        transaction
            .commit()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok((lease_token, Vec::new()));
    }
    let jobs: Vec<(Uuid, String, String, String, i16)> = sqlx::query_as(
        r#"SELECT operation_id, destination, subject, html_body, attempts
             FROM "MailOutbox"
            WHERE delivered_at_utc IS NULL
              AND dead_at_utc IS NULL
              AND superseded_at_utc IS NULL
              AND attempts < $2
              AND available_at_utc <= clock_timestamp()
              AND (lease_expires_at_utc IS NULL
                   OR lease_expires_at_utc <= clock_timestamp())
            ORDER BY available_at_utc, created_at_utc, operation_id
            LIMIT $1
            FOR UPDATE SKIP LOCKED"#,
    )
    .bind(i64::try_from(slots.len()).unwrap_or(CLAIM_LIMIT))
    .bind(MAX_ATTEMPTS)
    .fetch_all(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let mut leased = Vec::with_capacity(jobs.len());
    for (slot, (operation_id, destination, subject, html_body, attempts)) in
        slots.into_iter().zip(jobs)
    {
        sqlx::query(
            r#"UPDATE "MailDeliverySlots"
                  SET lease_token = $1,
                      lease_expires_at_utc = clock_timestamp()
                          + ($2::BIGINT * INTERVAL '1 second')
                WHERE slot_id = $3"#,
        )
        .bind(lease_token)
        .bind(LEASE_SECONDS)
        .bind(slot)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        sqlx::query(
            r#"UPDATE "MailOutbox"
                  SET lease_token = $1,
                      lease_expires_at_utc = clock_timestamp()
                          + ($2::BIGINT * INTERVAL '1 second'),
                      delivery_slot = $3,
                      attempts = attempts + 1
                WHERE operation_id = $4"#,
        )
        .bind(lease_token)
        .bind(LEASE_SECONDS)
        .bind(slot)
        .bind(operation_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        leased.push(LeasedMail {
            operation_id,
            destination,
            subject,
            html_body,
            attempts: attempts + 1,
            delivery_slot: slot,
        });
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok((lease_token, leased))
}

fn retry_delay_seconds(attempts: i16, operation_id: Uuid) -> i64 {
    let exponent = u32::from(attempts.saturating_sub(1).clamp(0, 8) as u16);
    let base = 2_i64.saturating_pow(exponent).clamp(2, 300);
    let jitter = i64::from(operation_id.as_bytes()[0] % 7);
    base.saturating_add(jitter).min(300)
}

async fn finish_job(
    pool: &sqlx::PgPool,
    lease_token: Uuid,
    job: &LeasedMail,
    delivered: bool,
    retryable: bool,
) -> AppResult<()> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let retry = !delivered && retryable && job.attempts < MAX_ATTEMPTS;
    let affected = sqlx::query(
        r#"UPDATE "MailOutbox"
              SET delivered_at_utc = CASE WHEN $3 THEN clock_timestamp()
                                          ELSE delivered_at_utc END,
                  dead_at_utc = CASE WHEN NOT $3 AND NOT $4 THEN clock_timestamp()
                                     ELSE dead_at_utc END,
                  available_at_utc = CASE WHEN NOT $3 AND $4
                      THEN clock_timestamp() + ($5::BIGINT * INTERVAL '1 second')
                      ELSE available_at_utc END,
                  last_error = CASE WHEN $3 THEN NULL
                                    WHEN $4 THEN 'smtp_delivery_failed'
                                    ELSE CASE WHEN $6 THEN 'smtp_retry_budget_exhausted'
                                              ELSE 'superseded' END END,
                  html_body = CASE WHEN $3 OR NOT $4 THEN '' ELSE html_body END,
                  lease_token = NULL,
                  lease_expires_at_utc = NULL,
                  delivery_slot = NULL
            WHERE operation_id = $1 AND lease_token = $2"#,
    )
    .bind(job.operation_id)
    .bind(lease_token)
    .bind(delivered)
    .bind(retry)
    .bind(retry_delay_seconds(job.attempts, job.operation_id))
    .bind(retryable)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"UPDATE "MailDeliverySlots"
              SET lease_token = NULL, lease_expires_at_utc = NULL
            WHERE slot_id = $1 AND lease_token = $2"#,
    )
    .bind(job.delivery_slot)
    .bind(lease_token)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if affected.rows_affected() != 1 {
        return Err(AppError::internal("Mail delivery lease was lost"));
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

async fn deliver_one(
    pool: &sqlx::PgPool,
    sender: MailSender,
    lease_token: Uuid,
    job: LeasedMail,
) -> AppResult<()> {
    let superseded: bool = sqlx::query_scalar(
        r#"SELECT superseded_at_utc IS NOT NULL
             FROM "MailOutbox"
            WHERE operation_id = $1 AND lease_token = $2"#,
    )
    .bind(job.operation_id)
    .bind(lease_token)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .unwrap_or(true);
    let delivered = !superseded
        && sender
            .send_required(&job.destination, &job.subject, &job.html_body)
            .await
            .is_ok();
    finish_job(pool, lease_token, &job, delivered, !superseded).await?;
    if delivered {
        tracing::info!(operation_id = %job.operation_id, attempts = job.attempts, "mail delivered");
    } else {
        tracing::warn!(operation_id = %job.operation_id, attempts = job.attempts, "mail delivery deferred or exhausted");
    }
    Ok(())
}

async fn expire_stale(pool: &sqlx::PgPool, limit: i64) -> AppResult<u64> {
    let affected = sqlx::query(
        r#"WITH stale AS MATERIALIZED (
               SELECT operation_id
                 FROM "MailOutbox"
                WHERE delivered_at_utc IS NULL
                  AND dead_at_utc IS NULL
                  AND (attempts >= $1 OR superseded_at_utc IS NOT NULL)
                  AND COALESCE(lease_expires_at_utc, '-infinity'::TIMESTAMPTZ)
                      <= clock_timestamp()
                ORDER BY created_at_utc, operation_id
                LIMIT $2
                FOR UPDATE SKIP LOCKED
           )
           UPDATE "MailOutbox" job
              SET dead_at_utc = clock_timestamp(),
                  lease_token = NULL,
                  lease_expires_at_utc = NULL,
                  delivery_slot = NULL,
                  last_error = CASE WHEN job.superseded_at_utc IS NOT NULL
                                    THEN 'superseded'
                                    ELSE 'smtp_retry_budget_exhausted' END,
                  html_body = ''
             FROM stale
            WHERE job.operation_id = stale.operation_id"#,
    )
    .bind(MAX_ATTEMPTS)
    .bind(limit.clamp(1, 1_024))
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(affected.rows_affected())
}

async fn purge_terminal(pool: &sqlx::PgPool, limit: i64) -> AppResult<u64> {
    let affected = sqlx::query(
        r#"WITH expired AS MATERIALIZED (
               SELECT operation_id
                 FROM "MailOutbox"
                WHERE COALESCE(delivered_at_utc, dead_at_utc)
                      < clock_timestamp() - ($2::BIGINT * INTERVAL '1 second')
                ORDER BY COALESCE(delivered_at_utc, dead_at_utc), operation_id
                LIMIT $1
                FOR UPDATE SKIP LOCKED
           )
           DELETE FROM "MailOutbox" job
            USING expired
            WHERE job.operation_id = expired.operation_id"#,
    )
    .bind(limit.clamp(1, 1_024))
    .bind(TERMINAL_RETENTION_SECONDS)
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(affected.rows_affected())
}

/// Drain one bounded batch with one effective database/environment SMTP config.
pub async fn reconcile(pool: &sqlx::PgPool, limit: i64) -> AppResult<usize> {
    expire_stale(pool, MAINTENANCE_LIMIT).await?;
    let (lease_token, jobs) = claim_pending(pool, limit).await?;
    let claimed = jobs.len();
    if jobs.is_empty() {
        return Ok(0);
    }
    let sender = MailSender::from_database(pool).await?;
    let results = stream::iter(jobs.into_iter().map(|job| {
        let sender = sender.clone();
        async move { deliver_one(pool, sender, lease_token, job).await }
    }))
    .buffer_unordered(MAX_CONCURRENT_DELIVERIES)
    .collect::<Vec<_>>()
    .await;
    for result in results {
        result?;
    }
    Ok(claimed)
}

/// Start the shutdown-aware control-plane mail owner. Fixed database slots keep
/// SMTP concurrency at four even during an active-active rolling restart.
pub fn start_reconciler(
    state: SharedState,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut next_cleanup = tokio::time::Instant::now();
        loop {
            if *shutdown.borrow() {
                break;
            }
            let now = tokio::time::Instant::now();
            if now >= next_cleanup {
                if let Err(error) = purge_terminal(state.pg(), MAINTENANCE_LIMIT).await {
                    tracing::error!(%error, "mail outbox retention cleanup failed");
                }
                next_cleanup = now + CLEANUP_INTERVAL;
            }
            if let Err(error) = reconcile(state.pg(), CLAIM_LIMIT).await {
                tracing::error!(%error, "mail outbox reconciler pass failed");
            }
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
                () = tokio::time::sleep(POLL_INTERVAL) => {}
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use std::str::FromStr;

    #[test]
    fn retry_delay_is_bounded_and_operation_stable() {
        let operation_id = Uuid::nil();
        let first = retry_delay_seconds(1, operation_id);
        assert_eq!(first, retry_delay_seconds(1, operation_id));
        for attempt in 1..=i16::MAX {
            assert!((2..=300).contains(&retry_delay_seconds(attempt, operation_id)));
        }
    }

    #[test]
    fn canonical_digest_excludes_rendered_token_but_binds_security_intent() {
        let operation_id = Uuid::new_v4();
        let account_id = Uuid::new_v4();
        let first = MailIntent {
            operation_id,
            purpose: MailPurpose::PasswordRecovery,
            account_id,
            security_generation: "stamp-a",
            destination: "PLAYER@example.test",
            source: Some("192.0.2.1"),
            subject: "Reset",
            html_body: "token one",
        };
        let second = MailIntent {
            operation_id,
            purpose: MailPurpose::PasswordRecovery,
            account_id,
            security_generation: "stamp-a",
            destination: "PLAYER@example.test",
            source: Some("192.0.2.1"),
            subject: "Reset",
            html_body: "token two",
        };
        let generation = digest(first.security_generation);
        let destination = digest(first.destination.trim().to_lowercase());
        assert_eq!(
            request_digest(&first, &generation, &destination),
            request_digest(&second, &generation, &destination)
        );
        let changed_generation = digest("stamp-b");
        assert_ne!(
            request_digest(&first, &generation, &destination),
            request_digest(&first, &changed_generation, &destination)
        );
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn exact_replay_supersession_and_global_delivery_slots_are_durable() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("mail_outbox_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "AspNetUsers" (id UUID PRIMARY KEY);
            CREATE TABLE "MailOutbox" (
              operation_id UUID PRIMARY KEY,
              purpose SMALLINT NOT NULL,
              account_id UUID NOT NULL REFERENCES "AspNetUsers"(id) ON DELETE CASCADE,
              security_generation_digest BYTEA NOT NULL,
              destination VARCHAR(320) NOT NULL,
              destination_digest BYTEA NOT NULL,
              source_digest BYTEA,
              request_digest BYTEA NOT NULL,
              subject VARCHAR(256) NOT NULL,
              html_body TEXT NOT NULL,
              attempts SMALLINT NOT NULL DEFAULT 0,
              available_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
              lease_token UUID,
              lease_expires_at_utc TIMESTAMPTZ,
              delivery_slot SMALLINT,
              delivered_at_utc TIMESTAMPTZ,
              dead_at_utc TIMESTAMPTZ,
              superseded_at_utc TIMESTAMPTZ,
              last_error VARCHAR(256),
              created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
            );
            CREATE TABLE "MailDeliverySlots" (
              slot_id SMALLINT PRIMARY KEY,
              lease_token UUID,
              lease_expires_at_utc TIMESTAMPTZ
            );
            INSERT INTO "MailDeliverySlots" (slot_id) VALUES (0), (1), (2), (3);
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let mut accounts = Vec::new();
        for _ in 0..6 {
            let account_id = Uuid::new_v4();
            sqlx::query(r#"INSERT INTO "AspNetUsers" (id) VALUES ($1)"#)
                .bind(account_id)
                .execute(&pool)
                .await
                .unwrap();
            accounts.push(account_id);
        }
        let original_operation = Uuid::new_v4();
        let mut transaction = pool.begin().await.unwrap();
        assert_eq!(
            enqueue_in_transaction(
                &mut transaction,
                MailIntent {
                    operation_id: original_operation,
                    purpose: MailPurpose::PasswordRecovery,
                    account_id: accounts[0],
                    security_generation: "stamp-a",
                    destination: "first@example.test",
                    source: Some("192.0.2.1"),
                    subject: "Reset",
                    html_body: "first body",
                },
            )
            .await
            .unwrap(),
            EnqueueOutcome::Inserted
        );
        transaction.commit().await.unwrap();

        let mut replay = pool.begin().await.unwrap();
        assert_eq!(
            enqueue_in_transaction(
                &mut replay,
                MailIntent {
                    operation_id: original_operation,
                    purpose: MailPurpose::PasswordRecovery,
                    account_id: accounts[0],
                    security_generation: "stamp-a",
                    destination: "first@example.test",
                    source: Some("192.0.2.1"),
                    subject: "Reset changed",
                    html_body: "a newly rendered token must not replace the committed body",
                },
            )
            .await
            .unwrap(),
            EnqueueOutcome::Replayed
        );
        replay.commit().await.unwrap();

        for (index, account_id) in accounts.iter().copied().enumerate() {
            let destination = format!("player-{index}@example.test");
            let source = format!("192.0.2.{}", index + 10);
            let mut transaction = pool.begin().await.unwrap();
            enqueue_in_transaction(
                &mut transaction,
                MailIntent {
                    operation_id: Uuid::new_v4(),
                    purpose: MailPurpose::PasswordRecovery,
                    account_id,
                    security_generation: "stamp-b",
                    destination: &destination,
                    source: Some(&source),
                    subject: "Reset",
                    html_body: "new body",
                },
            )
            .await
            .unwrap();
            transaction.commit().await.unwrap();
        }
        let original_terminal: bool = sqlx::query_scalar(
            r#"SELECT superseded_at_utc IS NOT NULL AND dead_at_utc IS NOT NULL
                 FROM "MailOutbox" WHERE operation_id = $1"#,
        )
        .bind(original_operation)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(original_terminal);

        let (_, first_claim) = claim_pending(&pool, 64).await.unwrap();
        let (_, second_claim) = claim_pending(&pool, 64).await.unwrap();
        assert_eq!(first_claim.len(), 4);
        assert!(second_claim.is_empty());

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
