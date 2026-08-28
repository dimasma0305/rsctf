//! Fail-fast admission for memory-hard credential workflows.

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

pub struct CredentialWorkPermit {
    pool: sqlx::PgPool,
    lease_token: Uuid,
    released: bool,
    _local: OwnedSemaphorePermit,
}

impl CredentialWorkPermit {
    pub async fn release(mut self) {
        let _ = sqlx::query(
            r#"DELETE FROM "CredentialMutationLeases"
                WHERE lease_token = $1"#,
        )
        .bind(self.lease_token)
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
        let pool = self.pool.clone();
        let lease_token = self.lease_token;
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = sqlx::query(
                    r#"DELETE FROM "CredentialMutationLeases"
                        WHERE lease_token = $1"#,
                )
                .bind(lease_token)
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
        .map_err(|_| AppError::TooManyRequests)?;
    let mut scope_hashes = scopes
        .iter()
        .map(|scope| Sha256::digest(scope.as_bytes()).to_vec())
        .collect::<Vec<_>>();
    scope_hashes.sort_unstable();
    scope_hashes.dedup();
    let lease_token = Uuid::new_v4();
    let lease_seconds = match class {
        CredentialWorkClass::Interactive => 45_i64,
        CredentialWorkClass::AdminBulk => 30 * 60,
    };
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    for scope_hash in &scope_hashes {
        let claimed = sqlx::query_scalar::<_, Uuid>(
            r#"INSERT INTO "CredentialMutationLeases"
                   (scope_hash, lease_token, expires_at_utc)
               VALUES ($1, $2,
                       clock_timestamp() + ($3::bigint * INTERVAL '1 second'))
               ON CONFLICT (scope_hash) DO UPDATE
                 SET lease_token = EXCLUDED.lease_token,
                     expires_at_utc = EXCLUDED.expires_at_utc,
                     created_at_utc = clock_timestamp()
               WHERE "CredentialMutationLeases".expires_at_utc <= clock_timestamp()
            RETURNING lease_token"#,
        )
        .bind(scope_hash)
        .bind(lease_token)
        .bind(lease_seconds)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if claimed != Some(lease_token) {
            transaction
                .rollback()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            return Err(AppError::TooManyRequests);
        }
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(CredentialWorkPermit {
        pool: pool.clone(),
        lease_token,
        released: false,
        _local: local,
    })
}

/// Small retention pass for durable plaintext wrappers and short admission
/// leases. Credential job-row and target rows cascade with their parent job.
pub async fn purge_expired(pool: &sqlx::PgPool, limit: i64) -> AppResult<u64> {
    let limit = limit.clamp(1, 1_000);
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let statements = [
        r#"DELETE FROM "AdminCredentialJobs" WHERE ctid IN (
              SELECT ctid FROM "AdminCredentialJobs"
               WHERE result_expires_at_utc <= clock_timestamp() LIMIT $1)"#,
        r#"DELETE FROM "AdminPasswordResetOperations" WHERE ctid IN (
              SELECT ctid FROM "AdminPasswordResetOperations"
               WHERE result_expires_at_utc <= clock_timestamp() LIMIT $1)"#,
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
        r#"DELETE FROM "ProxyTunnelLeases" WHERE ctid IN (
              SELECT ctid FROM "ProxyTunnelLeases"
               WHERE expires_at_utc <= clock_timestamp() LIMIT $1)"#,
        r#"DELETE FROM "ProxyOpenBudgets" WHERE ctid IN (
              SELECT ctid FROM "ProxyOpenBudgets"
               WHERE bucket_start_utc < clock_timestamp() - INTERVAL '2 minutes' LIMIT $1)"#,
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
               );"#,
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
            Err(AppError::TooManyRequests)
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

        first_pool.close().await;
        second_pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .expect("drop isolated schema");
        admin.close().await;
    }
}
