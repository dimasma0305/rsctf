use super::*;

fn participation_row(status: i16) -> ParticipationRow {
    ParticipationRow {
        id: 1,
        status,
        token: "token".to_string(),
        writeup_id: None,
        game_id: 2,
        team_id: 3,
        division_id: None,
        suspicion_score: 0,
    }
}

#[test]
fn participation_rows_decode_only_known_statuses() {
    for expected in [
        crate::utils::enums::ParticipationStatus::Pending,
        crate::utils::enums::ParticipationStatus::Accepted,
        crate::utils::enums::ParticipationStatus::Rejected,
        crate::utils::enums::ParticipationStatus::Suspended,
        crate::utils::enums::ParticipationStatus::Unsubmitted,
    ] {
        let model = participation::Model::try_from(participation_row(expected as i16))
            .expect("known participation status");
        assert_eq!(model.status, expected);
    }
    assert!(participation::Model::try_from(participation_row(i16::MAX)).is_err());
}
