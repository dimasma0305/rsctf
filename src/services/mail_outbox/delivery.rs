//! Bounded delivery of one already-configured SMTP batch.

use futures::{stream, StreamExt};

use uuid::Uuid;

use super::{
    deliver_one, LeasedMail, MailSender, CLAIM_LIMIT, LEASE_SECONDS, MAX_ATTEMPTS,
    MAX_CONCURRENT_DELIVERIES,
};
use crate::utils::error::{AppError, AppResult};

pub(super) async fn claim_pending(
    pool: &sqlx::PgPool,
    limit: i64,
) -> AppResult<(Uuid, Vec<LeasedMail>)> {
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

pub(super) async fn reconcile_with_sender(
    pool: &sqlx::PgPool,
    limit: i64,
    sender: MailSender,
) -> AppResult<usize> {
    if !sender.is_configured() {
        return Ok(0);
    }
    let (lease_token, jobs) = claim_pending(pool, limit).await?;
    let claimed = jobs.len();
    if jobs.is_empty() {
        return Ok(0);
    }
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
