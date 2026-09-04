use super::*;

#[test]
fn late_acceptance_is_the_only_new_scoring_roster_transition() {
    for current in [
        ParticipationStatus::Pending,
        ParticipationStatus::Rejected,
        ParticipationStatus::Unsubmitted,
    ] {
        assert!(is_late_roster_admission(
            true,
            current,
            ParticipationStatus::Accepted
        ));
    }
    assert!(!is_late_roster_admission(
        false,
        ParticipationStatus::Pending,
        ParticipationStatus::Accepted
    ));
    assert!(!is_late_roster_admission(
        true,
        ParticipationStatus::Accepted,
        ParticipationStatus::Rejected
    ));
    assert!(!is_late_roster_admission(
        true,
        ParticipationStatus::Suspended,
        ParticipationStatus::Accepted
    ));
}
