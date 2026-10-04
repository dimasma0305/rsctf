//! Scoreboard cache invalidation and lightweight real-time refresh hints.

use chrono::{DateTime, Utc};

use crate::app_state::SharedState;

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
        super::invalidate_combined_scoreboard(st, game_id),
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
