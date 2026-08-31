//! Bounded retention maintenance for durable HTTP mutation/link replay rows.

use std::time::Duration;

use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};

const CLEANUP_BATCH: i64 = 64;
const CLEANUP_INTERVAL: Duration = Duration::from_secs(30);

async fn tick(st: &SharedState) -> AppResult<()> {
    sqlx::query(
        r#"DELETE FROM "MutationOperations" WHERE ctid IN (
               SELECT ctid FROM "MutationOperations"
                WHERE expires_at_utc <= clock_timestamp()
                ORDER BY expires_at_utc
                LIMIT $1
           )"#,
    )
    .bind(CLEANUP_BATCH)
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"DELETE FROM "AccountLinkAttempts" WHERE token_digest IN (
               SELECT token_digest FROM "AccountLinkAttempts"
                WHERE expires_at_utc <= clock_timestamp()
                ORDER BY expires_at_utc
                LIMIT $1
           )"#,
    )
    .bind(CLEANUP_BATCH)
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

pub fn start(
    st: SharedState,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(CLEANUP_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if let Err(error) = tick(&st).await {
                        tracing::warn!(%error, "durable mutation retention cleanup deferred");
                    }
                }
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_request_is_a_fixed_size_index_ordered_delete() {
        let source = include_str!("mutation_retention.rs");
        assert_eq!(source.matches("LIMIT $1").count(), 2);
        assert_eq!(source.matches(".bind(CLEANUP_BATCH)").count(), 2);
        assert!(source.contains("ORDER BY expires_at_utc"));
        assert!(source.contains("MissedTickBehavior::Delay"));
    }
}
