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

#[tokio::test]
async fn profile_mutation_rejects_a_same_team_waiter_before_pool_checkout() {
    let team_id = 918_273;
    let key = format!("team-roster:{team_id}");
    let _leader = crate::utils::single_flight::coalesce(&key).await;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://postgres:postgres@127.0.0.1/rsctf")
        .expect("valid lazy PostgreSQL URL");

    let error = match acquire_profile_mutation(&pool, team_id).await {
        Ok(_) => panic!("same-team profile waiter was admitted"),
        Err(error) => error,
    };
    assert_eq!(error.status(), axum::http::StatusCode::SERVICE_UNAVAILABLE);
}
