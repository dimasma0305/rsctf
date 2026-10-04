//! Real-PostgreSQL check that a monitor action on a sealed event is visible on
//! the next report read instead of after the sealed cache lifetime.

use super::super::ai_chats::tests::fixture;
use super::*;
use crate::utils::enums::ParticipationStatus;

#[tokio::test]
#[ignore = "requires RSCTF_TEST_DATABASE_URL"]
async fn a_monitor_status_change_evicts_the_sealed_report() {
    let f = fixture().await;
    sqlx::query(
        r#"UPDATE "SuspicionReconciliationState"
              SET evidence_closed_at_utc = clock_timestamp(),
                  sealed_at_utc = clock_timestamp()
            WHERE game_id = $1"#,
    )
    .bind(f.open_game)
    .execute(&f.pool)
    .await
    .unwrap();
    let keys = ReportKeys::for_game(f.open_game);
    serve_report(&f.st, f.open_game, &HeaderMap::new())
        .await
        .unwrap();
    let cached = f.st.cache.get(&keys.report).await.expect("report cached");
    assert!(sealed_bundle(&cached));

    // The monitor suspends Beta from the report, then reads it back.
    let (participation_id,): (i32,) = sqlx::query_as(
        r#"SELECT participation_id FROM "UserParticipations"
            WHERE user_id = $1 AND game_id = $2"#,
    )
    .bind(f.bob.id)
    .bind(f.open_game)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    crate::controllers::admin::update_participation(
        State(f.st.clone()),
        f.admin.clone(),
        Path(participation_id),
        axum::Json(crate::controllers::admin::ParticipationEditModel {
            status: Some(ParticipationStatus::Suspended),
            division_id: None,
        }),
    )
    .await
    .unwrap();
    assert!(f.st.cache.get(&keys.report).await.is_none());
    assert!(f.st.cache.get_authoritative(&keys.epoch).await.is_some());
    f.teardown().await;
}
