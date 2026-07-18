use super::{
    data::snapshot_ids,
    deadline::{action as deadline_action, Action as DeadlineAction},
    record_receipt, rotate_capability_window, set_phase, CapabilityWindow, CrownPhase, CycleRow,
};
use crate::utils::enums::ParticipationStatus;
use serde_json::json;

#[test]
fn roster_snapshot_accepts_compact_and_object_forms() {
    assert_eq!(snapshot_ids(&json!([1, 2]), "participationId"), vec![1, 2]);
    assert_eq!(
        snapshot_ids(
            &json!([{"participationId": 4}, {"participationId": 7}]),
            "participationId"
        ),
        vec![4, 7]
    );
    assert_eq!(
        snapshot_ids(
            &json!([{"challengeId": 9}, "ignored", {"challengeId": 11}]),
            "challengeId"
        ),
        vec![9, 11]
    );
}

#[test]
fn event_deadline_adopts_unpublished_runtime_before_reclaiming_it() {
    assert_eq!(
        deadline_action(CrownPhase::CreatePending, false),
        DeadlineAction::AdoptReplacement
    );
    assert_eq!(
        deadline_action(CrownPhase::CreatePending, true),
        DeadlineAction::Reclaim
    );
    assert_eq!(
        deadline_action(CrownPhase::PublishPending, true),
        DeadlineAction::Reclaim
    );
    assert_eq!(
        deadline_action(CrownPhase::Active, true),
        DeadlineAction::Complete
    );
    assert_eq!(
        deadline_action(CrownPhase::Completed, true),
        DeadlineAction::Cleanup
    );
    assert_eq!(
        deadline_action(CrownPhase::Completed, false),
        DeadlineAction::Cleanup
    );
    assert_eq!(
        deadline_action(CrownPhase::Ended, true),
        DeadlineAction::Done
    );
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn phase_and_receipt_are_atomic_idempotent_and_fk_safe() {
    use sqlx::{Connection, PgConnection};

    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
    let mut connection = PgConnection::connect(&database_url).await.unwrap();
    sqlx::raw_sql(
        r#"
        CREATE TEMP TABLE "KothCrownCycles" (
          id BIGINT PRIMARY KEY,
          phase TEXT NOT NULL,
          updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
          last_error TEXT
        );
        CREATE TEMP TABLE "KothCycleAuditReceipts" (
          id BIGSERIAL PRIMARY KEY,
          cycle_id BIGINT NOT NULL REFERENCES "KothCrownCycles"(id),
          phase TEXT NOT NULL,
          attempt INTEGER NOT NULL,
          receipt JSONB NOT NULL,
          filesystem_diff JSONB,
          UNIQUE (cycle_id, phase, attempt)
        );
        INSERT INTO "KothCrownCycles" (id, phase) VALUES (41, 'FinalizePending');
        "#,
    )
    .execute(&mut connection)
    .await
    .unwrap();
    let cycle = CycleRow {
        id: 41,
        game_id: 7,
        challenge_id: 9,
        cycle_number: 1,
        phase: "FinalizePending".to_string(),
        planned_start_round: 1,
        old_container_id: None,
        replacement_container_id: None,
        replacement_host: None,
        replacement_port: None,
        expected_image: "registry.example/hill@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        reset_attempt: 1,
        readiness_attempt: 0,
    };

    for expected_transition in [true, false] {
        let mut transaction = connection.begin().await.unwrap();
        assert_eq!(
            set_phase(
                &mut transaction,
                cycle.id,
                CrownPhase::FinalizePending,
                CrownPhase::SnapshotPending,
            )
            .await
            .unwrap(),
            expected_transition
        );
        record_receipt(
            &mut transaction,
            &cycle,
            CrownPhase::FinalizePending,
            json!({"round": 1}),
            None,
        )
        .await
        .unwrap();
        transaction.commit().await.unwrap();
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>(r#"SELECT COUNT(*) FROM "KothCycleAuditReceipts""#)
            .fetch_one(&mut connection)
            .await
            .unwrap(),
        1
    );

    let mut missing = cycle.clone();
    missing.id = 99;
    record_receipt(
        &mut connection,
        &missing,
        CrownPhase::FinalizePending,
        json!({"recovered": true}),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(r#"SELECT COUNT(*) FROM "KothCycleAuditReceipts""#)
            .fetch_one(&mut connection)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn recovery_mints_one_immutable_window_per_reset_attempt() {
    use sqlx::{Connection, PgConnection};

    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
    let mut connection = PgConnection::connect(&database_url).await.unwrap();
    sqlx::raw_sql(
        r#"
        CREATE TEMP TABLE "Participations" (
          id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, status SMALLINT NOT NULL
        );
        CREATE TEMP TABLE "KothTokens" (
          id SERIAL PRIMARY KEY,
          target_id INTEGER NOT NULL,
          participation_id INTEGER NOT NULL,
          token TEXT NOT NULL,
          submitted_at TIMESTAMPTZ NOT NULL,
          round_number INTEGER NOT NULL,
          ad_round_id INTEGER NOT NULL,
          revoked_at TIMESTAMPTZ,
          cycle_id BIGINT NOT NULL,
          challenge_id INTEGER NOT NULL,
          reset_attempt INTEGER NOT NULL,
          UNIQUE (cycle_id, challenge_id, reset_attempt, participation_id)
        );
        "#,
    )
    .execute(&mut connection)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "Participations" (id, game_id, status)
           VALUES (11, 7, $1), (12, 7, $1)"#,
    )
    .bind(ParticipationStatus::Accepted as i16)
    .execute(&mut connection)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "KothTokens"
             (target_id, participation_id, token, submitted_at, round_number,
              ad_round_id, cycle_id, challenge_id, reset_attempt)
           VALUES (3, 11, 'attempt-0-a', clock_timestamp(), 5, 50, 41, 9, 0),
                  (3, 12, 'attempt-0-b', clock_timestamp(), 5, 50, 41, 9, 0)"#,
    )
    .execute(&mut connection)
    .await
    .unwrap();

    let roster = [11, 12];
    let fresh = ["attempt-1-a".to_string(), "attempt-1-b".to_string()];
    rotate_capability_window(
        &mut connection,
        CapabilityWindow {
            target_id: 3,
            game_id: 7,
            challenge_id: 9,
            cycle_id: 41,
            reset_attempt: 1,
            round_number: 6,
            ad_round_id: 60,
            roster: &roster,
            tokens: &fresh,
        },
    )
    .await
    .unwrap();

    let windows: Vec<(i32, String, bool)> = sqlx::query_as(
        r#"SELECT reset_attempt, token, revoked_at IS NULL
             FROM "KothTokens" ORDER BY reset_attempt, participation_id"#,
    )
    .fetch_all(&mut connection)
    .await
    .unwrap();
    assert_eq!(
        windows,
        vec![
            (0, "attempt-0-a".to_string(), false),
            (0, "attempt-0-b".to_string(), false),
            (1, "attempt-1-a".to_string(), true),
            (1, "attempt-1-b".to_string(), true),
        ]
    );

    let retry = [
        "retry-must-not-replace-a".to_string(),
        "retry-must-not-replace-b".to_string(),
    ];
    rotate_capability_window(
        &mut connection,
        CapabilityWindow {
            target_id: 3,
            game_id: 7,
            challenge_id: 9,
            cycle_id: 41,
            reset_attempt: 1,
            round_number: 6,
            ad_round_id: 60,
            roster: &roster,
            tokens: &retry,
        },
    )
    .await
    .unwrap();

    let active: Vec<String> = sqlx::query_scalar(
        r#"SELECT token FROM "KothTokens"
            WHERE revoked_at IS NULL ORDER BY participation_id"#,
    )
    .fetch_all(&mut connection)
    .await
    .unwrap();
    assert_eq!(active, fresh);
    let row_count: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM "KothTokens""#)
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(row_count, 4);
}
