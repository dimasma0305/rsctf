use crate::utils::enums::ParticipationStatus;
use chrono::{DateTime, Utc};

pub(super) fn can_view_game_metadata(
    status: Option<ParticipationStatus>,
    start_time: DateTime<Utc>,
    now: DateTime<Utc>,
) -> bool {
    status == Some(ParticipationStatus::Accepted) && now >= start_time
}

#[cfg(test)]
mod tests {
    use super::can_view_game_metadata;
    use crate::utils::enums::ParticipationStatus;
    use chrono::{Duration, Utc};

    #[test]
    fn only_accepted_started_participations_receive_challenge_metadata() {
        let start = Utc::now();
        assert!(!can_view_game_metadata(
            Some(ParticipationStatus::Accepted),
            start,
            start - Duration::milliseconds(1)
        ));
        for status in [
            None,
            Some(ParticipationStatus::Pending),
            Some(ParticipationStatus::Rejected),
            Some(ParticipationStatus::Suspended),
            Some(ParticipationStatus::Unsubmitted),
        ] {
            assert!(!can_view_game_metadata(status, start, start));
        }
        assert!(can_view_game_metadata(
            Some(ParticipationStatus::Accepted),
            start,
            start
        ));
    }
}
