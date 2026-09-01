//! Fail-fast admission for memory-hard credential workflows.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};

use sha2::{Digest, Sha256};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

use crate::utils::error::{AppError, AppResult};

static INTERACTIVE: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(16)));
static ADMIN_BULK: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(1)));

#[derive(Clone, Copy)]
pub enum CredentialWorkClass {
    Interactive,
    AdminBulk,
}

impl CredentialWorkClass {
    fn code(self) -> i16 {
        match self {
            Self::Interactive => 0,
            Self::AdminBulk => 1,
        }
    }
}

pub struct CredentialWorkPermit {
    pool: sqlx::PgPool,
    lease_token: Uuid,
    lease_seconds: i64,
    work_class: CredentialWorkClass,
    slot_id: i16,
    renewed_at: std::time::Instant,
    scope_hashes: BTreeSet<Vec<u8>>,
    lease_lost: Arc<AtomicBool>,
    heartbeat_stop: Option<tokio::sync::oneshot::Sender<()>>,
    released: bool,
    _local: OwnedSemaphorePermit,
}

impl CredentialWorkPermit {
    /// Extend an admitted workflow with identities learned from a bounded DB
    /// lookup without consuming or queueing for another local work slot.
    pub async fn try_add_scopes(&mut self, scopes: &[&str]) -> AppResult<()> {
        self.ensure_owned().await?;
        let hashes = hash_scopes(scopes);
        claim_scope_hashes(&self.pool, self.lease_token, self.lease_seconds, &hashes).await?;
        self.scope_hashes.extend(hashes);
        Ok(())
    }

    /// Check the heartbeat and refresh a bounded bulk workflow's replica-safe
    /// scopes between rows.
    pub async fn renew_if_needed(&mut self) -> AppResult<()> {
        let renew_after = std::time::Duration::from_secs(
            u64::try_from((self.lease_seconds / 3).max(1)).unwrap_or(1),
        );
        if self.renewed_at.elapsed() < renew_after {
            return self.check_heartbeat();
        }
        self.ensure_owned().await
    }

    /// Confirm and renew every distributed scope immediately before a durable
    /// credential mutation. This fences a slow Argon2 completion from a lease
    /// that another replica reclaimed while it was running.
    pub async fn ensure_owned(&mut self) -> AppResult<()> {
        self.check_heartbeat()?;
        let (renewed_scopes, renewed_slot) = sqlx::query_as::<_, (i64, i64)>(
            r#"WITH scopes AS (
                   UPDATE "CredentialMutationLeases"
                      SET expires_at_utc = clock_timestamp()
                          + ($2::bigint * INTERVAL '1 second')
                    WHERE lease_token = $1
                  RETURNING 1
               ), slot AS (
                   UPDATE "CredentialMutationSlots"
                      SET expires_at_utc = clock_timestamp()
                          + ($2::bigint * INTERVAL '1 second')
                    WHERE work_class = $3 AND slot_id = $4
                      AND lease_token = $1
                  RETURNING 1
               )
               SELECT (SELECT COUNT(*) FROM scopes),
                      (SELECT COUNT(*) FROM slot)"#,
        )
        .bind(self.lease_token)
        .bind(self.lease_seconds)
        .bind(self.work_class.code())
        .bind(self.slot_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if renewed_scopes != i64::try_from(self.scope_hashes.len()).unwrap_or(i64::MAX)
            || renewed_slot != 1
        {
            self.lease_lost.store(true, Ordering::Release);
            return Err(AppError::too_many_requests(1));
        }
        self.renewed_at = std::time::Instant::now();
        Ok(())
    }

    fn check_heartbeat(&self) -> AppResult<()> {
        if self.lease_lost.load(Ordering::Acquire) {
            Err(AppError::too_many_requests(1))
        } else {
            Ok(())
        }
    }

    pub async fn release(mut self) {
        if let Some(stop) = self.heartbeat_stop.take() {
            let _ = stop.send(());
        }
        let _ = sqlx::query(
            r#"WITH released_scopes AS (
                   DELETE FROM "CredentialMutationLeases"
                    WHERE lease_token = $1
               )
               UPDATE "CredentialMutationSlots"
                  SET lease_token = NULL, expires_at_utc = NULL
                WHERE work_class = $2 AND slot_id = $3 AND lease_token = $1"#,
        )
        .bind(self.lease_token)
        .bind(self.work_class.code())
        .bind(self.slot_id)
        .execute(&self.pool)
        .await;
        self.released = true;
    }
}

impl Drop for CredentialWorkPermit {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        if let Some(stop) = self.heartbeat_stop.take() {
            let _ = stop.send(());
        }
        let pool = self.pool.clone();
        let lease_token = self.lease_token;
        let work_class = self.work_class.code();
        let slot_id = self.slot_id;
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = sqlx::query(
                    r#"WITH released_scopes AS (
                           DELETE FROM "CredentialMutationLeases"
                            WHERE lease_token = $1
                       )
                       UPDATE "CredentialMutationSlots"
                          SET lease_token = NULL, expires_at_utc = NULL
                        WHERE work_class = $2 AND slot_id = $3 AND lease_token = $1"#,
                )
                .bind(lease_token)
                .bind(work_class)
                .bind(slot_id)
                .execute(&pool)
                .await;
            });
        }
    }
}

pub async fn try_acquire(
    pool: &sqlx::PgPool,
    class: CredentialWorkClass,
    scope: &str,
) -> AppResult<CredentialWorkPermit> {
    try_acquire_scopes(pool, class, &[scope]).await
}

/// Acquire one local work slot and every replica-safe mutation scope in one
/// short transaction. Account and source scopes are claimed together so a
/// rejected duplicate never queues behind Argon2 and partial admission cannot
/// strand either key.
pub async fn try_acquire_scopes(
    pool: &sqlx::PgPool,
    class: CredentialWorkClass,
    scopes: &[&str],
) -> AppResult<CredentialWorkPermit> {
    if scopes.is_empty() {
        return Err(AppError::internal("credential admission requires a scope"));
    }
    let semaphore = match class {
        CredentialWorkClass::Interactive => INTERACTIVE.clone(),
        CredentialWorkClass::AdminBulk => ADMIN_BULK.clone(),
    };
    let local = semaphore
        .try_acquire_owned()
        .map_err(|_| AppError::too_many_requests(1))?;
    let lease_token = Uuid::new_v4();
    let lease_seconds = match class {
        CredentialWorkClass::Interactive => 45_i64,
        CredentialWorkClass::AdminBulk => 60,
    };
    let scope_hashes = hash_scopes(scopes);
    let slot_id = claim_workflow(pool, class, lease_token, lease_seconds, &scope_hashes).await?;
    let lease_lost = Arc::new(AtomicBool::new(false));
    let (heartbeat_stop, mut stopped) = tokio::sync::oneshot::channel();
    let heartbeat_pool = pool.clone();
    let heartbeat_lost = lease_lost.clone();
    tokio::spawn(async move {
        let interval_seconds = u64::try_from((lease_seconds / 3).max(1)).unwrap_or(1);
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_seconds));
        interval.tick().await;
        loop {
            tokio::select! {
                _ = &mut stopped => break,
                _ = interval.tick() => {
                    let renewed = sqlx::query_as::<_, (i64, i64)>(
                        r#"WITH scopes AS (
                               UPDATE "CredentialMutationLeases"
                                  SET expires_at_utc = clock_timestamp()
                                      + ($2::bigint * INTERVAL '1 second')
                                WHERE lease_token = $1
                              RETURNING 1
                           ), slot AS (
                               UPDATE "CredentialMutationSlots"
                                  SET expires_at_utc = clock_timestamp()
                                      + ($2::bigint * INTERVAL '1 second')
                                WHERE work_class = $3 AND slot_id = $4
                                  AND lease_token = $1
                              RETURNING 1
                           )
                           SELECT (SELECT COUNT(*) FROM scopes),
                                  (SELECT COUNT(*) FROM slot)"#,
                    )
                    .bind(lease_token)
                    .bind(lease_seconds)
                    .bind(class.code())
                    .bind(slot_id)
                    .fetch_one(&heartbeat_pool)
                    .await;
                    if !matches!(renewed, Ok((scopes, 1)) if scopes > 0) {
                        heartbeat_lost.store(true, Ordering::Release);
                        break;
                    }
                }
            }
        }
    });
    Ok(CredentialWorkPermit {
        pool: pool.clone(),
        lease_token,
        lease_seconds,
        work_class: class,
        slot_id,
        renewed_at: std::time::Instant::now(),
        scope_hashes,
        lease_lost,
        heartbeat_stop: Some(heartbeat_stop),
        released: false,
        _local: local,
    })
}

fn hash_scopes(scopes: &[&str]) -> BTreeSet<Vec<u8>> {
    scopes
        .iter()
        .map(|scope| Sha256::digest(scope.as_bytes()).to_vec())
        .collect()
}

async fn claim_scope_hashes(
    pool: &sqlx::PgPool,
    lease_token: Uuid,
    lease_seconds: i64,
    scope_hashes: &BTreeSet<Vec<u8>>,
) -> AppResult<()> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    set_short_admission_lock_timeout(&mut transaction).await?;
    claim_scope_hashes_locked(&mut transaction, lease_token, lease_seconds, scope_hashes).await?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

async fn claim_workflow(
    pool: &sqlx::PgPool,
    class: CredentialWorkClass,
    lease_token: Uuid,
    lease_seconds: i64,
    scope_hashes: &BTreeSet<Vec<u8>>,
) -> AppResult<i16> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    set_short_admission_lock_timeout(&mut transaction).await?;
    let slot_id = sqlx::query_scalar::<_, i16>(
        r#"WITH candidate AS (
               SELECT work_class, slot_id
                 FROM "CredentialMutationSlots"
                WHERE work_class = $1
                  AND (lease_token IS NULL OR expires_at_utc <= clock_timestamp())
                ORDER BY slot_id
                FOR UPDATE SKIP LOCKED
                LIMIT 1
           )
           UPDATE "CredentialMutationSlots" slot
              SET lease_token = $2,
                  expires_at_utc = clock_timestamp()
                      + ($3::bigint * INTERVAL '1 second')
             FROM candidate
            WHERE slot.work_class = candidate.work_class
              AND slot.slot_id = candidate.slot_id
           RETURNING slot.slot_id"#,
    )
    .bind(class.code())
    .bind(lease_token)
    .bind(lease_seconds)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(admission_database_error)?
    .ok_or_else(|| AppError::too_many_requests(1))?;
    claim_scope_hashes_locked(&mut transaction, lease_token, lease_seconds, scope_hashes).await?;
    transaction
        .commit()
        .await
        .map_err(admission_database_error)?;
    Ok(slot_id)
}

async fn set_short_admission_lock_timeout(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> AppResult<()> {
    sqlx::query("SET LOCAL lock_timeout = '250ms'")
        .execute(&mut **transaction)
        .await
        .map_err(admission_database_error)?;
    Ok(())
}

async fn claim_scope_hashes_locked(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    lease_token: Uuid,
    lease_seconds: i64,
    scope_hashes: &BTreeSet<Vec<u8>>,
) -> AppResult<()> {
    let hashes = scope_hashes.iter().cloned().collect::<Vec<_>>();
    let claimed = sqlx::query_scalar::<_, i64>(
        r#"WITH desired AS MATERIALIZED (
               SELECT scope_hash
                 FROM UNNEST($1::bytea[]) AS input(scope_hash)
                ORDER BY scope_hash
           ), claimed AS (
               INSERT INTO "CredentialMutationLeases"
                   (scope_hash, lease_token, expires_at_utc)
               SELECT scope_hash, $2,
                      clock_timestamp() + ($3::bigint * INTERVAL '1 second')
                 FROM desired
               ON CONFLICT (scope_hash) DO UPDATE
                 SET lease_token = EXCLUDED.lease_token,
                     expires_at_utc = EXCLUDED.expires_at_utc,
                     created_at_utc = clock_timestamp()
               WHERE "CredentialMutationLeases".lease_token = EXCLUDED.lease_token
                  OR "CredentialMutationLeases".expires_at_utc <= clock_timestamp()
               RETURNING scope_hash
           ) SELECT COUNT(*)::bigint FROM claimed"#,
    )
    .bind(&hashes)
    .bind(lease_token)
    .bind(lease_seconds)
    .fetch_one(&mut **transaction)
    .await
    .map_err(admission_database_error)?;
    if claimed != i64::try_from(hashes.len()).unwrap_or(i64::MAX) {
        return Err(AppError::too_many_requests(1));
    }
    Ok(())
}

fn admission_database_error(error: sqlx::Error) -> AppError {
    if error
        .as_database_error()
        .and_then(|database| database.code())
        .as_deref()
        == Some("55P03")
    {
        AppError::too_many_requests(1)
    } else {
        AppError::internal(error.to_string())
    }
}

/// Small retention pass for encrypted credential results and short admission
/// leases. Durable administrator-issuance identities are tombstoned instead of
/// deleted so an expired replay can never re-run a credential mutation.
pub async fn purge_expired(pool: &sqlx::PgPool, limit: i64) -> AppResult<u64> {
    let limit = limit.clamp(1, 1_000);
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let statements = [
        r#"WITH expired AS MATERIALIZED (
               SELECT job.operation_id
                 FROM "AdminCredentialJobs" job
                WHERE (job.status = 1
                       AND job.result_expires_at_utc <= clock_timestamp())
                   OR (job.status = 0
                       AND job.created_at_utc < clock_timestamp() - INTERVAL '1 day'
                       AND NOT EXISTS (
                           SELECT 1 FROM "AdminCredentialJobRows" row
                            WHERE row.operation_id = job.operation_id
                              AND row.status = 0
                              AND row.lease_expires_at_utc > clock_timestamp()
                       ))
                   OR (job.status = 2 AND EXISTS (
                           SELECT 1 FROM "AdminCredentialJobRows" row
                            WHERE row.operation_id = job.operation_id
                              AND (row.result_ciphertext IS NOT NULL
                                   OR row.result_nonce IS NOT NULL)
                       ))
                ORDER BY job.created_at_utc, job.operation_id
                LIMIT $1 FOR UPDATE SKIP LOCKED
           ), wiped AS (
               DELETE FROM "AdminCredentialJobRows" row
                USING expired
                WHERE row.operation_id = expired.operation_id
           ), released AS (
               DELETE FROM "AdminCredentialTargetLeases" target
                USING expired
                WHERE target.operation_id = expired.operation_id
           )
           UPDATE "AdminCredentialJobs" job
              SET status = 2,
                  completed_at_utc = COALESCE(job.completed_at_utc, clock_timestamp()),
                  result_expires_at_utc = COALESCE(job.result_expires_at_utc, clock_timestamp())
             FROM expired
            WHERE job.operation_id = expired.operation_id"#,
        r#"UPDATE "AdminPasswordResetOperations"
              SET status = 2,
                  result_ciphertext = NULL,
                  result_nonce = NULL,
                  completed_at_utc = COALESCE(completed_at_utc, clock_timestamp())
            WHERE ctid IN (
              SELECT ctid FROM "AdminPasswordResetOperations"
               WHERE (status = 1 AND result_expires_at_utc <= clock_timestamp())
                  OR (status = 0
                      AND lease_expires_at_utc <= clock_timestamp()
                      AND created_at_utc < clock_timestamp() - INTERVAL '1 day')
                  OR (status = 2
                      AND (result_ciphertext IS NOT NULL OR result_nonce IS NOT NULL))
               ORDER BY created_at_utc, operation_id
               LIMIT $1 FOR UPDATE SKIP LOCKED)"#,
        r#"DELETE FROM "PasswordResetAttempts" WHERE ctid IN (
              SELECT ctid FROM "PasswordResetAttempts"
               WHERE created_at_utc < clock_timestamp() - INTERVAL '1 day' LIMIT $1)"#,
        r#"DELETE FROM "PasswordResetTickets" WHERE ctid IN (
              SELECT ticket.ctid FROM "PasswordResetTickets" ticket
               WHERE ticket.expires_at_utc <= clock_timestamp()
                 AND NOT EXISTS (SELECT 1 FROM "PasswordResetAttempts" attempt
                                  WHERE attempt.token_hash = ticket.token_hash)
               LIMIT $1)"#,
        r#"DELETE FROM "CredentialMutationLeases" WHERE ctid IN (
              SELECT ctid FROM "CredentialMutationLeases"
               WHERE expires_at_utc <= clock_timestamp() LIMIT $1)"#,
        r#"UPDATE "CredentialMutationSlots"
              SET lease_token = NULL, expires_at_utc = NULL
            WHERE (work_class, slot_id) IN (
              SELECT work_class, slot_id FROM "CredentialMutationSlots"
               WHERE expires_at_utc <= clock_timestamp()
               ORDER BY work_class, slot_id LIMIT $1 FOR UPDATE SKIP LOCKED)"#,
    ];
    let mut removed = 0_u64;
    for statement in statements {
        removed = removed.saturating_add(
            sqlx::query(statement)
                .bind(limit)
                .execute(&mut *transaction)
                .await
                .map_err(|error| AppError::internal(error.to_string()))?
                .rows_affected(),
        );
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use std::str::FromStr;

    #[test]
    fn admission_keys_are_fixed_width_and_domain_separated_by_caller() {
        assert_eq!(Sha256::digest(b"password:user:stamp").len(), 32);
        assert_ne!(
            Sha256::digest(b"password:user:stamp"),
            Sha256::digest(b"email:user:stamp")
        );
    }

    #[tokio::test]
    #[ignore = "requires disposable PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn account_and_source_scopes_are_atomic_across_pools() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("connect test database");
        let schema = format!("credential_admission_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .expect("create isolated schema");
        let options = PgConnectOptions::from_str(&database_url)
            .expect("parse test database URL")
            .options([("search_path", schema.as_str())]);
        let first_pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options.clone())
            .await
            .expect("connect first replica pool");
        let second_pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .expect("connect second replica pool");
        sqlx::raw_sql(
            r#"CREATE TABLE "CredentialMutationLeases" (
                 scope_hash BYTEA PRIMARY KEY,
                 lease_token UUID NOT NULL,
                 expires_at_utc TIMESTAMPTZ NOT NULL,
                 created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
               );
               CREATE TABLE "CredentialMutationSlots" (
                 work_class SMALLINT NOT NULL,
                 slot_id SMALLINT NOT NULL,
                 lease_token UUID NULL,
                 expires_at_utc TIMESTAMPTZ NULL,
                 PRIMARY KEY (work_class, slot_id)
               );
               INSERT INTO "CredentialMutationSlots" (work_class, slot_id)
               SELECT 0, slot_id FROM generate_series(0, 15) AS slot_id;
               INSERT INTO "CredentialMutationSlots" (work_class, slot_id)
               VALUES (1, 0);"#,
        )
        .execute(&first_pool)
        .await
        .expect("create admission fixture");

        let first = try_acquire_scopes(
            &first_pool,
            CredentialWorkClass::Interactive,
            &["password:user:stamp", "credential-source:192.0.2.1"],
        )
        .await
        .expect("claim first replica scopes");
        assert!(matches!(
            try_acquire_scopes(
                &second_pool,
                CredentialWorkClass::Interactive,
                &["password:user:stamp", "credential-source:192.0.2.2"],
            )
            .await,
            Err(AppError::TooManyRequests { .. })
        ));
        first.release().await;
        let reclaimed = try_acquire_scopes(
            &second_pool,
            CredentialWorkClass::Interactive,
            &["password:user:stamp", "credential-source:192.0.2.2"],
        )
        .await
        .expect("released account scope is reusable");
        reclaimed.release().await;

        sqlx::query(
            r#"UPDATE "CredentialMutationSlots"
                  SET lease_token = $1, expires_at_utc = clock_timestamp() + INTERVAL '1 hour'
                WHERE work_class = 0"#,
        )
        .bind(Uuid::new_v4())
        .execute(&first_pool)
        .await
        .unwrap();
        assert!(matches!(
            try_acquire(
                &second_pool,
                CredentialWorkClass::Interactive,
                "password:another-user:stamp"
            )
            .await,
            Err(AppError::TooManyRequests { .. })
        ));

        first_pool.close().await;
        second_pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .expect("drop isolated schema");
        admin.close().await;
    }

    #[tokio::test]
    #[ignore = "requires disposable PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn retention_wipes_expired_secrets_but_preserves_operation_tombstones() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("connect test database");
        let schema = format!("credential_retention_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .expect("create isolated schema");
        let options = PgConnectOptions::from_str(&database_url)
            .expect("parse test database URL")
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .expect("connect isolated schema");
        sqlx::raw_sql(
            r#"
            CREATE TABLE "AdminCredentialJobs" (
                operation_id UUID PRIMARY KEY,
                status SMALLINT NOT NULL,
                result_expires_at_utc TIMESTAMPTZ NULL,
                created_at_utc TIMESTAMPTZ NOT NULL,
                completed_at_utc TIMESTAMPTZ NULL
            );
            CREATE TABLE "AdminCredentialJobRows" (
                operation_id UUID NOT NULL,
                status SMALLINT NOT NULL,
                lease_expires_at_utc TIMESTAMPTZ NOT NULL,
                result_ciphertext BYTEA NULL,
                result_nonce BYTEA NULL
            );
            CREATE TABLE "AdminCredentialTargetLeases" (
                operation_id UUID NOT NULL
            );
            CREATE TABLE "AdminPasswordResetOperations" (
                operation_id UUID PRIMARY KEY,
                status SMALLINT NOT NULL,
                lease_expires_at_utc TIMESTAMPTZ NOT NULL,
                result_ciphertext BYTEA NULL,
                result_nonce BYTEA NULL,
                result_expires_at_utc TIMESTAMPTZ NOT NULL,
                created_at_utc TIMESTAMPTZ NOT NULL,
                completed_at_utc TIMESTAMPTZ NULL
            );
            CREATE TABLE "PasswordResetTickets" (
                token_hash BYTEA PRIMARY KEY,
                expires_at_utc TIMESTAMPTZ NOT NULL
            );
            CREATE TABLE "PasswordResetAttempts" (
                token_hash BYTEA NOT NULL,
                created_at_utc TIMESTAMPTZ NOT NULL
            );
            CREATE TABLE "CredentialMutationLeases" (
                expires_at_utc TIMESTAMPTZ NOT NULL
            );
            CREATE TABLE "CredentialMutationSlots" (
                work_class SMALLINT NOT NULL,
                slot_id SMALLINT NOT NULL,
                lease_token UUID NULL,
                expires_at_utc TIMESTAMPTZ NULL,
                PRIMARY KEY (work_class, slot_id)
            );

            INSERT INTO "AdminCredentialJobs" VALUES
                ('00000000-0000-0000-0000-000000000001', 1, NOW() - INTERVAL '1 hour', NOW() - INTERVAL '2 hours', NOW() - INTERVAL '1 hour'),
                ('00000000-0000-0000-0000-000000000002', 1, NOW() + INTERVAL '1 hour', NOW() - INTERVAL '1 hour', NOW()),
                ('00000000-0000-0000-0000-000000000003', 0, NULL, NOW() - INTERVAL '2 days', NULL),
                ('00000000-0000-0000-0000-000000000004', 0, NULL, NOW() - INTERVAL '2 days', NULL);
            INSERT INTO "AdminCredentialJobRows" VALUES
                ('00000000-0000-0000-0000-000000000001', 1, NOW(), '\x01', '\x000000000000000000000000'),
                ('00000000-0000-0000-0000-000000000002', 1, NOW(), '\x02', '\x000000000000000000000000'),
                ('00000000-0000-0000-0000-000000000003', 1, NOW() - INTERVAL '1 day', '\x03', '\x000000000000000000000000'),
                ('00000000-0000-0000-0000-000000000004', 0, NOW() + INTERVAL '1 hour', NULL, NULL);
            INSERT INTO "AdminCredentialTargetLeases" VALUES
                ('00000000-0000-0000-0000-000000000001'),
                ('00000000-0000-0000-0000-000000000003');

            INSERT INTO "AdminPasswordResetOperations" VALUES
                ('00000000-0000-0000-0000-000000000005', 1, NOW(), '\x05', '\x000000000000000000000000', NOW() - INTERVAL '1 hour', NOW() - INTERVAL '2 hours', NOW() - INTERVAL '1 hour'),
                ('00000000-0000-0000-0000-000000000006', 1, NOW(), '\x06', '\x000000000000000000000000', NOW() + INTERVAL '1 hour', NOW() - INTERVAL '1 hour', NOW()),
                ('00000000-0000-0000-0000-000000000007', 0, NOW() - INTERVAL '1 day', NULL, NULL, NOW() + INTERVAL '1 hour', NOW() - INTERVAL '2 days', NULL),
                ('00000000-0000-0000-0000-000000000008', 0, NOW() + INTERVAL '1 hour', NULL, NULL, NOW() + INTERVAL '1 hour', NOW() - INTERVAL '2 days', NULL);
            "#,
        )
        .execute(&pool)
        .await
        .expect("create credential retention fixtures");

        assert_eq!(purge_expired(&pool, 16).await.unwrap(), 4);
        assert_eq!(purge_expired(&pool, 16).await.unwrap(), 0);

        let import_identity_count: i64 =
            sqlx::query_scalar(r#"SELECT COUNT(*) FROM "AdminCredentialJobs""#)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(import_identity_count, 4);
        let import_tombstones: i64 =
            sqlx::query_scalar(r#"SELECT COUNT(*) FROM "AdminCredentialJobs" WHERE status = 2"#)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(import_tombstones, 2);
        let retained_import_rows: Vec<(Uuid, i16, bool)> = sqlx::query_as(
            r#"SELECT operation_id, status,
                      result_ciphertext IS NOT NULL OR result_nonce IS NOT NULL
                 FROM "AdminCredentialJobRows" ORDER BY operation_id"#,
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            retained_import_rows,
            vec![
                (
                    Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap(),
                    1,
                    true,
                ),
                (
                    Uuid::parse_str("00000000-0000-0000-0000-000000000004").unwrap(),
                    0,
                    false,
                ),
            ]
        );
        let target_lease_count: i64 =
            sqlx::query_scalar(r#"SELECT COUNT(*) FROM "AdminCredentialTargetLeases""#)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(target_lease_count, 0);

        let reset_rows: Vec<(Uuid, i16, bool)> = sqlx::query_as(
            r#"SELECT operation_id, status,
                      result_ciphertext IS NOT NULL OR result_nonce IS NOT NULL
                 FROM "AdminPasswordResetOperations" ORDER BY operation_id"#,
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(reset_rows.len(), 4);
        assert_eq!(reset_rows[0].1, 2);
        assert!(!reset_rows[0].2);
        assert_eq!(reset_rows[1].1, 1);
        assert!(reset_rows[1].2);
        assert_eq!(reset_rows[2].1, 2);
        assert!(!reset_rows[2].2);
        assert_eq!(reset_rows[3].1, 0);

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .expect("drop isolated schema");
        admin.close().await;
    }
}
