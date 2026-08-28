//! Durable delivery for organizer-created normal notices.
//!
//! PostgreSQL owns the schedule and mutation order. Hub events are a bounded
//! fast path; open clients reconcile the authoritative HTTP page after a
//! mutation event and after every reconnect.

use std::time::Duration;

use serde_json::Value;
use uuid::Uuid;

use crate::app_state::{HubEvent, SharedState};
use crate::utils::error::{AppError, AppResult};

const DELIVERY_BATCH: i64 = 100;
const CLAIM_TIMEOUT_SECONDS: i64 = 30;
const RECONCILE_INTERVAL: Duration = Duration::from_millis(500);

#[derive(sqlx::FromRow)]
struct ClaimedNotice {
    id: i64,
    game_id: i32,
    event_kind: i16,
    payload: Value,
}

async fn claim_due(st: &SharedState, claim: Uuid) -> AppResult<Vec<ClaimedNotice>> {
    sqlx::query_as::<_, ClaimedNotice>(
        r#"WITH due AS (
               SELECT id
                 FROM "GameNoticeOutbox"
                WHERE delivered_at_utc IS NULL
                  AND available_at_utc <= clock_timestamp()
                  AND (
                      claim_token IS NULL OR claimed_at_utc < clock_timestamp()
                          - ($2 * interval '1 second')
                  )
                ORDER BY available_at_utc, id
                LIMIT $3
                FOR UPDATE SKIP LOCKED
           )
           UPDATE "GameNoticeOutbox" event
              SET claim_token = $1, claimed_at_utc = clock_timestamp()
             FROM due
            WHERE event.id = due.id
        RETURNING event.id, event.game_id, event.event_kind, event.payload"#,
    )
    .bind(claim)
    .bind(CLAIM_TIMEOUT_SECONDS)
    .bind(DELIVERY_BATCH)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

async fn acknowledge(st: &SharedState, claim: Uuid, id: i64) -> AppResult<()> {
    sqlx::query(
        r#"UPDATE "GameNoticeOutbox"
              SET delivered_at_utc = clock_timestamp(),
                  claim_token = NULL, claimed_at_utc = NULL
            WHERE id = $1 AND claim_token = $2 AND delivered_at_utc IS NULL"#,
    )
    .bind(id)
    .bind(claim)
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

pub async fn reconcile_once(st: &SharedState) -> AppResult<usize> {
    let claim = Uuid::new_v4();
    let events = claim_due(st, claim).await?;
    let count = events.len();
    for event in events {
        let target = match event.event_kind {
            0 => "ReceivedGameNotice",
            1 => "ReceivedGameNoticeChanged",
            _ => continue,
        };
        st.events.publish(HubEvent {
            target,
            game_id: Some(event.game_id),
            payload: event.payload.to_string(),
        });
        acknowledge(st, claim, event.id).await?;
    }
    Ok(count)
}

async fn purge_retained_rows(st: &SharedState) -> AppResult<()> {
    sqlx::query(
        r#"DELETE FROM "GameNoticeOperations"
            WHERE (game_id, operation_id) IN (
                SELECT game_id, operation_id
                  FROM "GameNoticeOperations"
                 WHERE completed_at_utc < clock_timestamp() - interval '24 hours'
                 ORDER BY completed_at_utc
                 LIMIT 100
            )"#,
    )
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"DELETE FROM "GameNoticeOutbox" WHERE id IN (
               SELECT id FROM "GameNoticeOutbox"
                WHERE delivered_at_utc < clock_timestamp() - interval '7 days'
                ORDER BY delivered_at_utc
                LIMIT 100
           )"#,
    )
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

pub fn start_reconciler(
    state: SharedState,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(RECONCILE_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut passes = 0_u64;
        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { break; }
                }
                _ = tick.tick() => {
                    if let Err(error) = reconcile_once(&state).await {
                        tracing::warn!(%error, "normal-notice delivery reconciliation failed");
                    }
                    passes = passes.saturating_add(1);
                    if passes.is_multiple_of(120) {
                        if let Err(error) = purge_retained_rows(&state).await {
                            tracing::warn!(%error, "normal-notice delivery retention failed");
                        }
                    }
                }
            }
        }
    })
}

#[cfg(test)]
#[path = "notice_delivery/tests.rs"]
mod tests;
