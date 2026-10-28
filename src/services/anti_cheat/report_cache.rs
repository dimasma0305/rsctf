//! Report invalidation shared by evidence writers and HTTP readers.

use crate::app_state::SharedState;

/// Outlives every report entry so fills can detect concurrent invalidation.
const EPOCH_TTL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// A game's cached report and its invalidation epoch. A fill records the
/// epoch before building and discards its result if the epoch moved, so an
/// invalidation that lands mid-build cannot be overwritten by stale data.
pub(crate) struct ReportKeys {
    pub(crate) report: String,
    pub(crate) epoch: String,
}

impl ReportKeys {
    pub(crate) fn for_game(game_id: i32) -> Self {
        Self {
            report: format!("_AntiCheatReportWireV1_{game_id}"),
            epoch: format!("_AntiCheatReportEpochV1_{game_id}"),
        }
    }
}

pub(crate) async fn invalidate_keys(cache: &dyn crate::services::cache::Cache, keys: &ReportKeys) {
    cache
        .set_authoritative(
            &keys.epoch,
            uuid::Uuid::new_v4().as_bytes(),
            Some(EPOCH_TTL),
        )
        .await;
    cache.remove(&keys.report).await;
}

/// Drop the cached report so late evidence (for example a scan of a writeup
/// uploaded after a sealed event) or a monitor action is visible on the next
/// read, including when a report build is already running.
pub(crate) async fn invalidate_report(st: &SharedState, game_id: i32) {
    invalidate_keys(st.cache.as_ref(), &ReportKeys::for_game(game_id)).await;
}
