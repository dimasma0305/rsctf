use std::time::Duration;

use crate::app_state::SharedState;

const POLL_SECONDS: u64 = 15;
const MAX_CONCURRENT_SCANS: usize = 2;
const MAX_CONCURRENT_PUSHES: usize = 2;

pub fn start(
    state: SharedState,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(POLL_SECONDS));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut scans = tokio::task::JoinSet::new();
        let mut pushes = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { break; }
                }
                _ = ticker.tick() => {
                    let capacity = MAX_CONCURRENT_SCANS.saturating_sub(scans.len());
                    if capacity > 0 {
                        match crate::controllers::admin::claim_repo_scan(
                            state.pg(),
                            None,
                            capacity as i64,
                        ).await {
                            Ok(claimed) => {
                                for (binding_id, lease_token) in claimed {
                                    let state = state.clone();
                                    scans.spawn(async move {
                                        if let Err(error) = crate::controllers::admin::run_claimed_repo_scan(
                                            &state,
                                            binding_id,
                                            lease_token,
                                            false,
                                        ).await {
                                            tracing::warn!(binding_id, %error, "scheduled repository scan failed");
                                        }
                                    });
                                }
                            }
                            Err(error) => tracing::warn!(%error, "repository scheduler claim failed"),
                        }
                    }
                    let push_capacity = MAX_CONCURRENT_PUSHES.saturating_sub(pushes.len());
                    if push_capacity == 0 { continue; }
                    match crate::controllers::edit::claim_repo_push_jobs(
                        state.pg(),
                        push_capacity as i64,
                    ).await {
                        Ok(claimed) => {
                            for batch in claimed {
                                let binding_id = batch.binding_id;
                                let state = state.clone();
                                pushes.spawn(async move {
                                    if let Err(error) = crate::controllers::edit::run_claimed_repo_push_job(
                                        &state,
                                        batch,
                                    ).await {
                                        tracing::warn!(binding_id, %error, "scheduled repository push failed");
                                    }
                                });
                            }
                        }
                        Err(error) => tracing::warn!(%error, "repository push scheduler claim failed"),
                    }
                }
                completed = scans.join_next(), if !scans.is_empty() => {
                    if let Some(Err(error)) = completed {
                        tracing::warn!(%error, "repository scheduler worker panicked");
                    }
                }
                completed = pushes.join_next(), if !pushes.is_empty() => {
                    if let Some(Err(error)) = completed {
                        tracing::warn!(%error, "repository push worker panicked");
                    }
                }
            }
        }
        scans.shutdown().await;
        pushes.shutdown().await;
    })
}
