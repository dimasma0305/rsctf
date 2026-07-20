use super::*;
use sea_orm::{ConnectOptions, Database};

fn failure(service_id: i32) -> FlagDeliveryOutcome {
    FlagDeliveryOutcome::failed(
        service_id,
        FlagDeliveryKind::Managed,
        Some(format!("container-{service_id}")),
        3,
        "container exec failed",
    )
}

#[test]
fn shared_policy_fits_default_attempts_and_rejects_an_oversized_pair() {
    let default = FlagDeliveryPolicy::from_values(None, None, None).unwrap();
    assert_eq!(default.attempts(), 3);
    assert_eq!(default.attempt_timeout(), std::time::Duration::from_secs(2));
    assert_eq!(
        default.worst_case_attempt_window(),
        std::time::Duration::from_millis(6_150)
    );
    assert_eq!(
        default.publication_reserve(),
        std::time::Duration::from_secs(7)
    );
    assert!(FlagDeliveryPolicy::from_values(None, Some("1"), Some("6")).is_ok());
    assert!(FlagDeliveryPolicy::from_values(None, Some("1"), Some("7")).is_err());
    assert!(FlagDeliveryPolicy::from_values(Some("64"), Some("3"), Some("3")).is_err());
    assert!(FlagDeliveryPolicy::from_values(Some("0"), None, None).is_err());
    assert!(FlagDeliveryPolicy::from_values(None, Some("not-a-number"), None).is_err());
}

#[test]
fn duplicate_service_outcomes_are_rejected_instead_of_double_counted() {
    let outcomes = vec![failure(7), failure(7)];
    assert!(matches!(
        validate_outcomes(&outcomes),
        Err(AppError::Conflict(_))
    ));
}

#[test]
fn outcome_shape_rejects_false_success_and_external_container_identity() {
    let mut outcome = FlagDeliveryOutcome::succeeded(
        1,
        FlagDeliveryKind::External,
        Some("not-valid-for-external".into()),
        1,
    );
    assert!(validate_outcomes(&[outcome.clone()]).is_err());
    outcome.container_id = None;
    outcome.attempts = 0;
    assert!(validate_outcomes(&[outcome]).is_err());
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn publication_is_idempotent_and_counts_each_failed_service_once() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
    let mut options = ConnectOptions::new(database_url);
    options.max_connections(1).min_connections(1);
    let db = Database::connect(options).await.unwrap();
    let pool = db.get_postgres_connection_pool();
    sqlx::raw_sql(
        r#"
            CREATE TEMP TABLE "Games" (
              id INTEGER PRIMARY KEY, end_time_utc TIMESTAMPTZ NOT NULL
            );
            CREATE TEMP TABLE "AdRounds" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              start_time_utc TIMESTAMPTZ NOT NULL, end_time_utc TIMESTAMPTZ NOT NULL,
              finalized BOOLEAN NOT NULL, flags_published_at TIMESTAMPTZ,
              flag_delivery_failures INTEGER NOT NULL
            );
            CREATE TEMP TABLE "GameChallenges" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              ad_self_hosted BOOLEAN NOT NULL
            );
            CREATE TEMP TABLE "AdTeamServices" (
              id INTEGER PRIMARY KEY, challenge_id INTEGER NOT NULL,
              game_id INTEGER NOT NULL, container_id TEXT
            );
            CREATE TEMP TABLE "AdFlags" (
              round_id INTEGER NOT NULL, team_service_id INTEGER NOT NULL,
              PRIMARY KEY (round_id, team_service_id)
            );
            CREATE TEMP TABLE "AdCheckResults" (
              round_id INTEGER NOT NULL, team_service_id INTEGER NOT NULL,
              status SMALLINT NOT NULL, message TEXT, checked_at TIMESTAMPTZ NOT NULL,
              sla_credit DOUBLE PRECISION, flag_verified BOOLEAN NOT NULL,
              PRIMARY KEY (round_id, team_service_id)
            );
            CREATE TEMP TABLE "AdFlagDeliveryResults" (
              round_id INTEGER NOT NULL, team_service_id INTEGER NOT NULL,
              delivery_kind TEXT NOT NULL, container_id TEXT, delivered BOOLEAN NOT NULL,
              attempts SMALLINT NOT NULL, failure_reason TEXT,
              completed_at TIMESTAMPTZ NOT NULL,
              PRIMARY KEY (round_id, team_service_id)
            );
            INSERT INTO "GameChallenges" VALUES (4, 7, FALSE);
            INSERT INTO "AdTeamServices" VALUES
              (11, 4, 7, 'container-11'), (12, 4, 7, 'container-12'),
              (13, 4, 7, 'container-13'), (14, 4, 7, 'container-14'),
              (15, 4, 7, 'container-15');
            INSERT INTO "AdFlags" VALUES
              (9, 11), (9, 12), (9, 13), (9, 14), (9, 15);
            INSERT INTO "AdCheckResults" VALUES
              (9, 11, 3, 'pending', clock_timestamp(), NULL, FALSE),
              (9, 12, 3, 'pending', clock_timestamp(), NULL, FALSE),
              (9, 13, 3, 'pending', clock_timestamp(), NULL, FALSE),
              (9, 14, 3, 'pending', clock_timestamp(), NULL, FALSE),
              (9, 15, 3, 'pending', clock_timestamp(), NULL, FALSE);
            "#,
    )
    .execute(pool)
    .await
    .unwrap();
    let now: chrono::DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "Games" VALUES (7, $1)"#)
        .bind(now + chrono::Duration::minutes(5))
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "AdRounds" VALUES (9, 7, $1, $2, FALSE, NULL, 0)"#)
        .bind(now - chrono::Duration::seconds(1))
        .bind(now + chrono::Duration::minutes(1))
        .execute(pool)
        .await
        .unwrap();

    // Service 12 changes identity while its batch is in flight. Its stale
    // success must become an attempted participant failure without rolling
    // back service 13's valid receipt.
    sqlx::query(r#"UPDATE "AdTeamServices" SET container_id = 'replacement-12' WHERE id = 12"#)
        .execute(pool)
        .await
        .unwrap();
    let outcomes = vec![
        FlagDeliveryOutcome::failed(
            11,
            FlagDeliveryKind::Managed,
            Some("container-11".into()),
            3,
            "repair and push both failed",
        ),
        FlagDeliveryOutcome::succeeded(
            12,
            FlagDeliveryKind::Managed,
            Some("container-12".into()),
            1,
        ),
        FlagDeliveryOutcome::succeeded(
            13,
            FlagDeliveryKind::Managed,
            Some("container-13".into()),
            1,
        ),
    ];
    let receipts = record_flag_delivery_outcome_batch(&db, 7, 9, &outcomes)
        .await
        .unwrap();
    assert_eq!(
        receipts
            .iter()
            .map(|receipt| receipt.team_service_id)
            .collect::<Vec<_>>(),
        vec![13]
    );

    // Service 13 changes after its successful durable receipt but before the
    // first settlement. Settlement itself must seal Offline evidence; no
    // later publication replay may be required for correctness.
    sqlx::query(r#"UPDATE "AdTeamServices" SET container_id = 'replacement-13' WHERE id = 13"#)
        .execute(pool)
        .await
        .unwrap();
    let first = settle_flag_delivery_outcomes(&db, 7, 9, &[14])
        .await
        .unwrap();
    let replay = settle_flag_delivery_outcomes(&db, 7, 9, &[14])
        .await
        .unwrap();
    assert_eq!(first.failure_count, 4);
    assert_eq!(replay.failure_count, 4);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            r#"SELECT COUNT(*) FROM "AdFlagDeliveryResults" WHERE round_id = 9"#,
        )
        .fetch_one(pool)
        .await
        .unwrap(),
        5
    );
    let failed_check: (i16, Option<String>, Option<f64>, bool) = sqlx::query_as(
        r#"SELECT status, message, sla_credit, flag_verified
                 FROM "AdCheckResults" WHERE round_id = 9 AND team_service_id = 11"#,
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(failed_check.0, AdCheckStatus::Offline as i16);
    assert_eq!(
        failed_check.1.as_deref(),
        Some("repair and push both failed")
    );
    assert_eq!(failed_check.2, Some(0.0));
    assert!(!failed_check.3);
    let in_flight_churn: (i16, Option<String>, Option<f64>, bool, i16) = sqlx::query_as(
        r#"SELECT check_result.status, check_result.message, check_result.sla_credit,
                      delivery.delivered, delivery.attempts
                 FROM "AdCheckResults" check_result
                 JOIN "AdFlagDeliveryResults" delivery
                   ON delivery.round_id = check_result.round_id
                  AND delivery.team_service_id = check_result.team_service_id
                WHERE check_result.round_id = 9 AND check_result.team_service_id = 12"#,
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(in_flight_churn.0, AdCheckStatus::Offline as i16);
    assert_eq!(
        in_flight_churn.1.as_deref(),
        Some(CHANGED_DURING_DELIVERY_REASON)
    );
    assert_eq!(in_flight_churn.2, Some(0.0));
    assert!(!in_flight_churn.3);
    assert!(in_flight_churn.4 > 0);

    let replaced_check: (i16, Option<String>, Option<f64>) = sqlx::query_as(
        r#"SELECT status, message, sla_credit FROM "AdCheckResults"
                WHERE round_id = 9 AND team_service_id = 13"#,
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(replaced_check.0, AdCheckStatus::Offline as i16);
    assert_eq!(
        replaced_check.1.as_deref(),
        Some(REPLACED_AFTER_PUBLICATION_REASON)
    );
    assert_eq!(replaced_check.2, Some(0.0));
    let incomplete_attempt: (i16, Option<f64>) = sqlx::query_as(
        r#"SELECT status, sla_credit FROM "AdCheckResults"
                WHERE round_id = 9 AND team_service_id = 14"#,
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(incomplete_attempt.0, AdCheckStatus::Offline as i16);
    assert_eq!(incomplete_attempt.1, Some(0.0));
    let platform_check: (i16, Option<f64>) = sqlx::query_as(
        r#"SELECT status, sla_credit FROM "AdCheckResults"
                WHERE round_id = 9 AND team_service_id = 15"#,
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(platform_check.0, AdCheckStatus::InternalError as i16);
    assert_eq!(platform_check.1, Some(0.0));

    let platform_void_services: Vec<i32> = sqlx::query_scalar(
        r#"SELECT COALESCE(array_agg(team_service_id ORDER BY team_service_id), '{}')
                 FROM "AdFlagDeliveryResults"
                WHERE round_id = 9 AND delivered = FALSE AND attempts = 0"#,
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(platform_void_services, vec![15]);
}
