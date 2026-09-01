//! Deployment-wide admission for anonymous account-mail preparation.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};

use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::{
    digest, MailPurpose, ADMISSION_WINDOW_SECONDS, MAX_CONCURRENT_PREPARATIONS,
    MAX_RECENT_PER_ACCOUNT, MAX_RECENT_PER_SOURCE, PREPARATION_LEASE_SECONDS,
};
use crate::utils::error::{AppError, AppResult};

static LOCAL_PREPARATION_SLOTS: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_PREPARATIONS)));

/// One bounded message-preparation owner. The local permit bounds work during a
/// database outage; the fixed database row bounds the same work across replicas.
pub struct MailPreparationPermit {
    pool: sqlx::PgPool,
    lease_token: Uuid,
    slot_id: i16,
    lease_lost: Arc<AtomicBool>,
    heartbeat_stop: Option<tokio::sync::oneshot::Sender<()>>,
    released: bool,
    _local: tokio::sync::OwnedSemaphorePermit,
}

impl MailPreparationPermit {
    pub async fn ensure_owned(&mut self) -> AppResult<()> {
        if self.lease_lost.load(Ordering::Acquire) {
            return Err(AppError::too_many_requests(1));
        }
        let renewed = sqlx::query(
            r#"UPDATE "MailPreparationSlots"
                  SET lease_expires_at_utc = clock_timestamp()
                      + ($3::BIGINT * INTERVAL '1 second')
                WHERE slot_id = $1 AND lease_token = $2"#,
        )
        .bind(self.slot_id)
        .bind(self.lease_token)
        .bind(PREPARATION_LEASE_SECONDS)
        .execute(&self.pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .rows_affected();
        if renewed != 1 {
            self.lease_lost.store(true, Ordering::Release);
            return Err(AppError::too_many_requests(1));
        }
        Ok(())
    }

    /// Fence this owner on the caller's final mail transaction. Stopping the
    /// heartbeat first avoids a second pool checkout; the row lock prevents a
    /// reclaimer from taking this slot until the outbox transaction commits.
    pub async fn ensure_owned_in_transaction(
        &mut self,
        transaction: &mut Transaction<'_, Postgres>,
    ) -> AppResult<()> {
        if let Some(stop) = self.heartbeat_stop.take() {
            let _ = stop.send(());
        }
        if self.lease_lost.load(Ordering::Acquire) {
            return Err(AppError::too_many_requests(1));
        }
        let renewed = sqlx::query(
            r#"UPDATE "MailPreparationSlots"
                  SET lease_expires_at_utc = clock_timestamp()
                      + ($3::BIGINT * INTERVAL '1 second')
                WHERE slot_id = $1 AND lease_token = $2"#,
        )
        .bind(self.slot_id)
        .bind(self.lease_token)
        .bind(PREPARATION_LEASE_SECONDS)
        .execute(&mut **transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .rows_affected();
        if renewed != 1 {
            self.lease_lost.store(true, Ordering::Release);
            return Err(AppError::too_many_requests(1));
        }
        Ok(())
    }

    /// Revalidate the global preparation lease and the account-specific rate
    /// boundary after an anonymous lookup resolves an account, but before a
    /// token, URL, or message body is constructed.
    pub async fn bind_account(&mut self, account_id: Uuid) -> AppResult<()> {
        if self.lease_lost.load(Ordering::Acquire) {
            return Err(AppError::too_many_requests(1));
        }
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(preparation_database_error)?;
        set_short_preparation_timeouts(&mut transaction).await?;
        let (renewed, account_recent): (i64, i64) = sqlx::query_as(
            r#"WITH renewed AS (
                   UPDATE "MailPreparationSlots"
                      SET lease_expires_at_utc = clock_timestamp()
                          + ($3::BIGINT * INTERVAL '1 second')
                    WHERE slot_id = $1 AND lease_token = $2
                  RETURNING 1
               )
               SELECT (SELECT COUNT(*) FROM renewed),
                      (SELECT COUNT(*)::BIGINT FROM (
                           SELECT 1 FROM "MailOutbox"
                            WHERE account_id = $4
                              AND created_at_utc >= clock_timestamp()
                                  - ($5::BIGINT * INTERVAL '1 second')
                            LIMIT $6
                       ) recent)"#,
        )
        .bind(self.slot_id)
        .bind(self.lease_token)
        .bind(PREPARATION_LEASE_SECONDS)
        .bind(account_id)
        .bind(ADMISSION_WINDOW_SECONDS)
        .bind(MAX_RECENT_PER_ACCOUNT.saturating_add(1))
        .fetch_one(&mut *transaction)
        .await
        .map_err(preparation_database_error)?;
        if renewed != 1 {
            self.lease_lost.store(true, Ordering::Release);
            return Err(AppError::too_many_requests(1));
        }
        if account_recent >= MAX_RECENT_PER_ACCOUNT {
            return Err(AppError::retry_after(
                u64::try_from(ADMISSION_WINDOW_SECONDS).unwrap_or(300),
            ));
        }
        transaction
            .commit()
            .await
            .map_err(preparation_database_error)?;
        Ok(())
    }

    pub async fn release(mut self) {
        if let Some(stop) = self.heartbeat_stop.take() {
            let _ = stop.send(());
        }
        let _ = sqlx::query(
            r#"UPDATE "MailPreparationSlots"
                  SET lease_token = NULL, lease_expires_at_utc = NULL
                WHERE slot_id = $1 AND lease_token = $2"#,
        )
        .bind(self.slot_id)
        .bind(self.lease_token)
        .execute(&self.pool)
        .await;
        self.released = true;
    }
}

impl Drop for MailPreparationPermit {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        if let Some(stop) = self.heartbeat_stop.take() {
            let _ = stop.send(());
        }
        let pool = self.pool.clone();
        let slot_id = self.slot_id;
        let lease_token = self.lease_token;
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = sqlx::query(
                    r#"UPDATE "MailPreparationSlots"
                          SET lease_token = NULL, lease_expires_at_utc = NULL
                        WHERE slot_id = $1 AND lease_token = $2"#,
                )
                .bind(slot_id)
                .bind(lease_token)
                .execute(&pool)
                .await;
            });
        }
    }
}

/// Reserve bounded work before an anonymous account-mail flow performs an
/// identity lookup or constructs a token/message. `None` means the exact
/// operation already has a durable outbox intent and needs no preparation.
pub async fn try_prepare(
    pool: &sqlx::PgPool,
    operation_id: Uuid,
    purpose: MailPurpose,
    destination: &str,
    source: Option<&str>,
) -> AppResult<Option<MailPreparationPermit>> {
    if operation_id.is_nil()
        || destination.len() > 320
        || source.is_some_and(|source| source.len() > 256)
    {
        return Err(AppError::bad_request(
            "Mail preparation exceeds supported limits",
        ));
    }
    crate::services::mail::validate_recipient(destination)?;
    let normalized_destination = destination.trim().to_lowercase();
    let destination_digest = digest(&normalized_destination);
    let source_digest = source.map(|source| digest(source.trim()));
    let local = LOCAL_PREPARATION_SLOTS
        .clone()
        .try_acquire_owned()
        .map_err(|_| AppError::too_many_requests(1))?;
    let lease_token = Uuid::new_v4();
    let mut transaction = pool.begin().await.map_err(preparation_database_error)?;
    set_short_preparation_timeouts(&mut transaction).await?;

    let existing: Option<(i16, Vec<u8>)> = sqlx::query_as(
        r#"SELECT purpose, destination_digest
             FROM "MailOutbox"
            WHERE operation_id = $1"#,
    )
    .bind(operation_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(preparation_database_error)?;
    if let Some((stored_purpose, stored_destination)) = existing {
        if stored_purpose != purpose as i16 || stored_destination != destination_digest {
            return Err(AppError::conflict(
                "Mail operation identity was already used for another request",
            ));
        }
        transaction
            .commit()
            .await
            .map_err(preparation_database_error)?;
        return Ok(None);
    }

    // Only source/deployment admission is safe before an anonymous account
    // lookup. A destination count would differ for registered and unknown
    // addresses and would turn Retry-After into an account-enumeration oracle.
    // Destination/account ceilings remain authoritative in the final enqueue
    // and are folded into the enumeration-safe success response by recovery.
    let source_recent: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)::BIGINT FROM (
               SELECT 1 FROM "MailOutbox"
                WHERE $1::BYTEA IS NOT NULL
                  AND source_digest = $1
                  AND created_at_utc >= clock_timestamp()
                      - ($2::BIGINT * INTERVAL '1 second')
                LIMIT $3
           ) recent_source"#,
    )
    .bind(&source_digest)
    .bind(ADMISSION_WINDOW_SECONDS)
    .bind(MAX_RECENT_PER_SOURCE.saturating_add(1))
    .fetch_one(&mut *transaction)
    .await
    .map_err(preparation_database_error)?;
    if source_digest.is_some() && source_recent >= MAX_RECENT_PER_SOURCE {
        return Err(AppError::retry_after(
            u64::try_from(ADMISSION_WINDOW_SECONDS).unwrap_or(300),
        ));
    }

    let slot_id: Option<i16> = sqlx::query_scalar(
        r#"SELECT slot_id
             FROM "MailPreparationSlots"
            WHERE lease_expires_at_utc IS NULL
               OR lease_expires_at_utc <= clock_timestamp()
            ORDER BY slot_id
            LIMIT 1
            FOR UPDATE SKIP LOCKED"#,
    )
    .fetch_optional(&mut *transaction)
    .await
    .map_err(preparation_database_error)?;
    let Some(slot_id) = slot_id else {
        return Err(AppError::too_many_requests(1));
    };
    sqlx::query(
        r#"UPDATE "MailPreparationSlots"
              SET lease_token = $1,
                  lease_expires_at_utc = clock_timestamp()
                      + ($2::BIGINT * INTERVAL '1 second')
            WHERE slot_id = $3"#,
    )
    .bind(lease_token)
    .bind(PREPARATION_LEASE_SECONDS)
    .bind(slot_id)
    .execute(&mut *transaction)
    .await
    .map_err(preparation_database_error)?;
    transaction
        .commit()
        .await
        .map_err(preparation_database_error)?;
    let lease_lost = Arc::new(AtomicBool::new(false));
    let (heartbeat_stop, mut stopped) = tokio::sync::oneshot::channel();
    let heartbeat_pool = pool.clone();
    let heartbeat_lost = lease_lost.clone();
    tokio::spawn(async move {
        let interval_seconds = u64::try_from((PREPARATION_LEASE_SECONDS / 3).max(1)).unwrap_or(1);
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_seconds));
        interval.tick().await;
        loop {
            tokio::select! {
                _ = &mut stopped => break,
                _ = interval.tick() => {
                    let renewed = sqlx::query(
                        r#"UPDATE "MailPreparationSlots"
                              SET lease_expires_at_utc = clock_timestamp()
                                  + ($3::BIGINT * INTERVAL '1 second')
                            WHERE slot_id = $1 AND lease_token = $2"#,
                    )
                    .bind(slot_id)
                    .bind(lease_token)
                    .bind(PREPARATION_LEASE_SECONDS)
                    .execute(&heartbeat_pool)
                    .await;
                    if !matches!(renewed, Ok(result) if result.rows_affected() == 1) {
                        heartbeat_lost.store(true, Ordering::Release);
                        break;
                    }
                }
            }
        }
    });
    Ok(Some(MailPreparationPermit {
        pool: pool.clone(),
        lease_token,
        slot_id,
        lease_lost,
        heartbeat_stop: Some(heartbeat_stop),
        released: false,
        _local: local,
    }))
}

async fn set_short_preparation_timeouts(
    transaction: &mut Transaction<'_, Postgres>,
) -> AppResult<()> {
    sqlx::query("SET LOCAL lock_timeout = '300ms'")
        .execute(&mut **transaction)
        .await
        .map_err(preparation_database_error)?;
    sqlx::query("SET LOCAL statement_timeout = '700ms'")
        .execute(&mut **transaction)
        .await
        .map_err(preparation_database_error)?;
    Ok(())
}

fn preparation_database_error(error: sqlx::Error) -> AppError {
    match error
        .as_database_error()
        .and_then(|database| database.code())
        .as_deref()
    {
        Some("55P03") | Some("57014") => AppError::too_many_requests(1),
        _ => AppError::internal(error.to_string()),
    }
}
