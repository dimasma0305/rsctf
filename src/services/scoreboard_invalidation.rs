//! Scoreboard cache invalidation and lightweight real-time refresh hints.

use chrono::{DateTime, Utc};

use crate::app_state::SharedState;
use crate::services::cache::Cache;

/// Remove every standard-scoreboard representation after a solve changes the
/// dynamic Jeopardy values, then remove the derived Overall projection.
pub(crate) async fn invalidate_standard_scoreboard(st: &SharedState, game_id: i32) {
    let legacy_live = format!("_ScoreBoard_{game_id}");
    let wire_live = format!("_ScoreBoardWireV2_{game_id}");
    let legacy_public = format!("_ScoreBoardFrozen_{game_id}");
    let wire_public = format!("_ScoreBoardWireV2Frozen_{game_id}");
    tokio::join!(
        st.cache.remove(&legacy_live),
        st.cache.remove(&wire_live),
        st.cache.remove(&legacy_public),
        st.cache.remove(&wire_public),
        invalidate_combined_scoreboard(st, game_id),
    );
}

/// Suppress refresh hints while standings are frozen because even message
/// timing would reveal solve activity. The authoritative final refresh is
/// driven by the game lifecycle transition at event end.
pub(crate) fn scoreboard_refresh_is_publicly_safe(
    freeze: Option<DateTime<Utc>>,
    end: DateTime<Utc>,
    now: DateTime<Utc>,
) -> bool {
    !crate::utils::scoring::public_scoreboard_frozen(freeze, end, now, false)
}

/// Best-effort wake-up only. HTTP remains authoritative and the browser keeps
/// its bounded polling fallback for lost messages and disconnected replicas.
pub(crate) fn publish_scoreboard_changed(st: &SharedState, game_id: i32, format: &'static str) {
    st.publish_event(
        "ReceivedScoreboardChanged",
        Some(game_id),
        serde_json::json!({ "format": format }).to_string(),
    );
}

#[cfg(test)]
mod tests {
    use super::scoreboard_refresh_is_publicly_safe;
    use chrono::{TimeDelta, Utc};

    #[test]
    fn public_freeze_hides_refresh_timing_until_the_event_ends() {
        let now = Utc::now();
        let freeze = now - TimeDelta::minutes(1);
        let end = now + TimeDelta::minutes(1);

        assert!(!scoreboard_refresh_is_publicly_safe(Some(freeze), end, now));
        assert!(scoreboard_refresh_is_publicly_safe(Some(freeze), end, end));
    }
}

pub(crate) fn combined_cache_key(game_id: i32, is_monitor: bool) -> String {
    if is_monitor {
        format!("_CombinedScoreBoardByChallenge_{game_id}")
    } else {
        format!("_CombinedScoreBoardByChallengeFrozen_{game_id}")
    }
}

pub(crate) async fn invalidate_combined_scoreboard(st: &SharedState, game_id: i32) {
    let live = combined_cache_key(game_id, true);
    let frozen = combined_cache_key(game_id, false);
    tokio::join!(st.cache.remove(&live), st.cache.remove(&frozen),);
}

pub(crate) fn stale_scoreboard_key(current_key: &str) -> String {
    format!("{current_key}:stale")
}

pub(crate) fn scoreboard_cache_key(game_id: i32, is_monitor: bool) -> String {
    if is_monitor {
        format!("_AdScoreBoard_{game_id}")
    } else {
        format!("_AdScoreBoardFrozen_{game_id}")
    }
}

pub(crate) async fn hard_invalidate_ad_scoreboard(st: &SharedState, game_id: i32) {
    // A board fill fences publication with Games.xmin. Most scoring inputs live
    // in child tables, so advance that revision before eviction: an older
    // detached fill can no longer republish stale data after this returns.
    if let Err(error) = sqlx::query(r#"UPDATE "Games" SET id = id WHERE id = $1"#)
        .bind(game_id)
        .execute(st.pg())
        .await
    {
        tracing::warn!(game = game_id, %error, "A&D scoreboard revision barrier failed");
    }
    hard_invalidate_ad_scoreboard_cache(st.cache.as_ref(), game_id).await;
    invalidate_combined_scoreboard(st, game_id).await;
}

pub(crate) async fn hard_invalidate_ad_scoreboard_cache(
    cache: &dyn crate::services::cache::Cache,
    game_id: i32,
) {
    let live = scoreboard_cache_key(game_id, true);
    let frozen = scoreboard_cache_key(game_id, false);
    let live_stale = stale_scoreboard_key(&live);
    let frozen_stale = stale_scoreboard_key(&frozen);
    tokio::join!(
        cache.remove(&live),
        cache.remove(&live_stale),
        cache.remove(&frozen),
        cache.remove(&frozen_stale),
    );
}

pub(crate) fn live_hill_snapshot_cache_key(game_id: i32) -> String {
    format!("adlivehills:{game_id}")
}

pub(crate) async fn invalidate_live_hill_snapshot(st: &SharedState, game_id: i32) {
    invalidate_live_hill_snapshot_cache(st.cache.as_ref(), game_id).await;
    invalidate_live_lifecycle_cache(st.cache.as_ref(), game_id).await;
}

pub(crate) async fn invalidate_live_hill_snapshot_cache(
    cache: &dyn crate::services::cache::Cache,
    game_id: i32,
) {
    cache.remove(&live_hill_snapshot_cache_key(game_id)).await;
}

pub(crate) fn live_lifecycle_cache_key(game_id: i32) -> String {
    format!("kothlifecycle:{game_id}")
}

pub(crate) async fn invalidate_live_lifecycle_cache(cache: &dyn Cache, game_id: i32) {
    cache.remove(&live_lifecycle_cache_key(game_id)).await;
}

pub(crate) fn live_hill_cache_key(game_id: i32) -> String {
    format!("kothlivehills:{game_id}")
}

pub(crate) async fn invalidate_live_hill_cache(cache: &dyn Cache, game_id: i32) {
    cache.remove(&live_hill_cache_key(game_id)).await;
}

pub(crate) async fn flush_ad_scoreboard(st: &SharedState, game_id: i32) {
    hard_invalidate_ad_scoreboard(st, game_id).await;
    invalidate_live_hill_cache(st.cache.as_ref(), game_id).await;
    st.cache.remove(&format!("_KothScoreBoard_{game_id}")).await;
    st.cache
        .remove(&format!("_KothScoreBoardWireV2_{game_id}"))
        .await;
    st.cache
        .remove(&format!("_KothScoreBoardFrozen_{game_id}"))
        .await;
    st.cache
        .remove(&format!("_KothScoreBoardWireV2Frozen_{game_id}"))
        .await;
    st.cache.remove(&format!("_KothTimeline_{game_id}")).await;
    st.cache
        .remove(&format!("_KothTimelineFrozen_{game_id}"))
        .await;
}

pub(crate) async fn flush_game_scoreboards(st: &SharedState, game_id: i32) {
    st.cache.remove(&format!("_ScoreBoard_{game_id}")).await;
    st.cache
        .remove(&format!("_ScoreBoardWireV2_{game_id}"))
        .await;
    st.cache
        .remove(&format!("_ScoreBoardFrozen_{game_id}"))
        .await;
    st.cache
        .remove(&format!("_ScoreBoardWireV2Frozen_{game_id}"))
        .await;
    flush_ad_scoreboard(st, game_id).await;
}

pub(crate) async fn flush_scoreboards_for_games(st: &SharedState, game_ids: &[i32]) {
    for &game_id in game_ids {
        for key in [
            format!("_ScoreBoard_{game_id}"),
            format!("_ScoreBoardFrozen_{game_id}"),
            format!("_ScoreBoardWireV2_{game_id}"),
            format!("_ScoreBoardWireV2Frozen_{game_id}"),
            format!("_KothScoreBoard_{game_id}"),
            format!("_KothScoreBoardFrozen_{game_id}"),
            format!("_KothScoreBoardWireV2_{game_id}"),
            format!("_KothScoreBoardWireV2Frozen_{game_id}"),
            format!("_KothTimeline_{game_id}"),
            format!("_KothTimelineFrozen_{game_id}"),
        ] {
            st.cache.remove(&key).await;
        }
        hard_invalidate_ad_scoreboard(st, game_id).await;
    }
}
