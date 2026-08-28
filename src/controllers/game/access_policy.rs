//! Shared event-window and specialized-standings visibility policy.

use super::*;

pub(crate) fn game_is_live(game: &game::Model, now: DateTime<Utc>) -> bool {
    event_window_is_live(
        game.practice_mode,
        game.start_time_utc,
        game.end_time_utc,
        now,
    )
}

fn event_window_is_live(
    practice_mode: bool,
    start_time_utc: DateTime<Utc>,
    end_time_utc: DateTime<Utc>,
    now: DateTime<Utc>,
) -> bool {
    now >= start_time_utc && (practice_mode || now < end_time_utc)
}

pub(crate) async fn require_live_event_window(
    st: &SharedState,
    game_id: i32,
) -> AppResult<game::Model> {
    let game = load_game_cached(st, game_id).await?;
    let now = Utc::now();
    if game_is_live(&game, now) {
        return Ok(game);
    }
    if now < game.start_time_utc {
        return Err(AppError::game_not_started());
    }
    Err(AppError::game_ended())
}

pub(crate) fn can_view_engine_standings(
    game: &game::Model,
    is_monitor: bool,
    now: DateTime<Utc>,
) -> bool {
    engine_standings_visible(game.hidden, game.start_time_utc, is_monitor, now)
}

fn engine_standings_visible(
    hidden: bool,
    start_time_utc: DateTime<Utc>,
    is_monitor: bool,
    now: DateTime<Utc>,
) -> bool {
    is_monitor || (!hidden && now >= start_time_utc)
}

#[cfg(test)]
mod tests {
    use super::{engine_standings_visible, event_window_is_live};
    use chrono::{Duration, Utc};

    #[test]
    fn live_window_never_opens_before_start_and_practice_only_relaxes_end() {
        let start = Utc::now();
        let end = start + Duration::hours(1);
        assert!(!event_window_is_live(
            false,
            start,
            end,
            start - Duration::milliseconds(1)
        ));
        assert!(!event_window_is_live(
            true,
            start,
            end,
            start - Duration::milliseconds(1)
        ));
        assert!(event_window_is_live(false, start, end, start));
        assert!(!event_window_is_live(false, start, end, end));
        assert!(event_window_is_live(true, start, end, end));
    }

    #[test]
    fn specialized_standings_hide_private_and_prestart_metadata_from_players() {
        let start = Utc::now();
        let before = start - Duration::milliseconds(1);
        assert!(!engine_standings_visible(false, start, false, before));
        assert!(!engine_standings_visible(true, start, false, start));
        assert!(engine_standings_visible(false, start, false, start));
        assert!(engine_standings_visible(true, start, true, before));
    }
}
