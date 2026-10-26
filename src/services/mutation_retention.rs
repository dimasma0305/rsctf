//! Bounded, opportunistic retention for durable mutation/link replay rows.

use std::time::Duration;

use crate::app_state::SharedState;
mod cleanup;
pub use cleanup::RetentionProgress;

const CLEANUP_INTERVAL: Duration = Duration::from_secs(30);

/// The shared BEGIN helper preserves ownership if shutdown cancels the caller
/// during transaction startup. Catch-up commits atomically after a bounded pass.
pub async fn purge_expired(pool: &sqlx::PgPool) -> Result<RetentionProgress, sqlx::Error> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool).await?;
    let progress = cleanup::purge_in_transaction(&mut transaction).await?;
    transaction.commit().await?;
    Ok(progress)
}

pub fn start(
    st: SharedState,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(CLEANUP_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut backlogged = false;
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    match purge_expired(st.pg()).await {
                        Ok(progress) => {
                            tracing::debug!(?progress, "durable mutation retention pass completed");
                            let remaining = progress.oldest_expired_seconds > 0;
                            if remaining != backlogged {
                                tracing::info!(?progress, "durable mutation retention backlog changed");
                                backlogged = remaining;
                            }
                        }
                        Err(error) => tracing::warn!(%error, "durable mutation retention cleanup deferred"),
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
mod tests;
