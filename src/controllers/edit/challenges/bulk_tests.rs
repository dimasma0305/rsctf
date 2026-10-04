use super::*;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;

#[test]
fn rejects_duplicate_and_oversized_intents_before_reservation() {
    assert!(BULK_DELETE_STEP_BUDGET < std::time::Duration::from_secs(5 * 60));
    let mut duplicate = BulkChallengeMutationRequest {
        operation_id: Uuid::new_v4(),
        expected_revision: 1,
        action: BulkChallengeAction::Enable,
        challenge_ids: vec![9, 9],
    };
    assert_eq!(
        validate_request(&mut duplicate).unwrap_err().status(),
        axum::http::StatusCode::BAD_REQUEST
    );
    let mut oversized = BulkChallengeMutationRequest {
        operation_id: Uuid::new_v4(),
        expected_revision: 1,
        action: BulkChallengeAction::Delete,
        challenge_ids: (1..=101).collect(),
    };
    assert_eq!(
        validate_request(&mut oversized).unwrap_err().status(),
        axum::http::StatusCode::PAYLOAD_TOO_LARGE
    );
}

#[test]
fn bulk_admission_and_long_reconciliation_do_not_queue_http_handlers() {
    let source = include_str!("bulk.rs");
    let reserve = source
        .split_once("async fn reserve_operation(")
        .unwrap()
        .1
        .split_once("async fn abandon_operation(")
        .unwrap()
        .0;
    assert!(reserve.contains("pg_try_advisory_xact_lock"));
    assert!(!reserve.contains("SELECT pg_advisory_xact_lock"));

    let handler = source
        .split_once("pub async fn mutate_challenges_bulk(")
        .unwrap()
        .1;
    assert!(handler.contains("try_acquire_owned()"));
    let compact_handler = handler.split_whitespace().collect::<String>();
    let prepare = compact_handler
        .find("complete_desired_state(&st,game_id,&request,lease_token,false).await")
        .expect("the request performs only the short desired-state preparation");
    let reconcile = compact_handler
        .find("spawn_desired_state_job_with_permit(st.clone(),game_id,request.clone(),lease_token,permit,)")
        .expect("long reconciliation owns the nonqueued admission permit");
    assert!(prepare < reconcile);
}

#[test]
fn desired_state_reconciles_only_the_event_wide_effects_each_type_needs() {
    let static_attachment = DesiredRuntimeEffect {
        challenge_id: 1,
        challenge_type: ChallengeType::StaticAttachment as i16,
        ad_self_hosted: false,
    };
    assert!(!desired::effect_has_runtime(&static_attachment));
    assert!(!desired::effect_needs_vpn_reconciliation(
        &static_attachment
    ));

    let static_container = DesiredRuntimeEffect {
        challenge_id: 2,
        challenge_type: ChallengeType::StaticContainer as i16,
        ad_self_hosted: false,
    };
    assert!(desired::effect_has_runtime(&static_container));
    assert!(!desired::effect_needs_vpn_reconciliation(&static_container));

    let attack_defense = DesiredRuntimeEffect {
        challenge_id: 3,
        challenge_type: ChallengeType::AttackDefense as i16,
        ad_self_hosted: true,
    };
    assert!(desired::effect_has_runtime(&attack_defense));
    assert!(desired::effect_needs_vpn_reconciliation(&attack_defense));
}

#[test]
fn bulk_disable_clears_only_changed_koth_holders_set_wise() {
    let changed = [
        (11, ChallengeType::StaticContainer as i16, false),
        (12, ChallengeType::KingOfTheHill as i16, false),
        (13, ChallengeType::AttackDefense as i16, true),
        (14, ChallengeType::KingOfTheHill as i16, false),
    ];
    assert_eq!(
        desired::disabled_koth_challenge_ids(false, &changed),
        vec![12, 14]
    );
    assert!(desired::disabled_koth_challenge_ids(true, &changed).is_empty());
    assert!(desired::CLEAR_DISABLED_KOTH_TARGETS_SQL.contains("challenge_id = ANY($2)"));
    assert!(desired::CLEAR_DISABLED_KOTH_TARGETS_SQL.contains("holder_participation_id = NULL"));
    assert!(desired::CLEAR_DISABLED_KOTH_TARGETS_SQL.contains("held_since = NULL"));
}

#[tokio::test]
async fn delete_dispatch_has_no_unbounded_waiter_queue() {
    let first = BULK_DELETE_SLOTS.clone().try_acquire_owned().unwrap();
    let second = BULK_DELETE_SLOTS.clone().try_acquire_owned().unwrap();
    assert!(BULK_DELETE_SLOTS.clone().try_acquire_owned().is_err());
    assert_eq!(BULK_DELETE_SLOTS.available_permits(), 0);
    drop((first, second));
    assert_eq!(
        BULK_DELETE_SLOTS.available_permits(),
        BULK_DELETE_CONCURRENCY
    );
}

#[tokio::test]
async fn desired_state_dispatch_has_no_unbounded_waiter_queue() {
    let permits = (0..BULK_DESIRED_STATE_CONCURRENCY)
        .map(|_| {
            BULK_DESIRED_STATE_SLOTS
                .clone()
                .try_acquire_owned()
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert!(BULK_DESIRED_STATE_SLOTS
        .clone()
        .try_acquire_owned()
        .is_err());
    assert_eq!(BULK_DESIRED_STATE_SLOTS.available_permits(), 0);
    drop(permits);
    assert_eq!(
        BULK_DESIRED_STATE_SLOTS.available_permits(),
        BULK_DESIRED_STATE_CONCURRENCY
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn expired_desired_state_operation_has_one_replica_safe_lease_owner() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("bulk_claim_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let first_pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options.clone())
        .await
        .unwrap();
    let second_pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"CREATE TABLE "BulkChallengeMutationOperations" (
             game_id INTEGER NOT NULL,
             operation_id UUID NOT NULL,
             action SMALLINT NOT NULL,
             state SMALLINT NOT NULL,
             lease_token UUID NULL,
             lease_expires_at_utc TIMESTAMPTZ NOT NULL,
             PRIMARY KEY (game_id, operation_id)
           );"#,
    )
    .execute(&first_pool)
    .await
    .unwrap();
    sqlx::raw_sql(
        r#"CREATE TABLE "BulkChallengeDesiredStateSlots" (
             slot_id SMALLINT PRIMARY KEY,
             lease_token UUID NULL,
             expires_at_utc TIMESTAMPTZ NULL
           );
           INSERT INTO "BulkChallengeDesiredStateSlots" (slot_id)
           VALUES (0), (1), (2), (3);"#,
    )
    .execute(&first_pool)
    .await
    .unwrap();
    let operation_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "BulkChallengeMutationOperations"
             (game_id, operation_id, action, state, lease_expires_at_utc)
           VALUES (1, $1, 0, 0, clock_timestamp() - INTERVAL '1 second')"#,
    )
    .bind(operation_id)
    .execute(&first_pool)
    .await
    .unwrap();

    let first =
        claim_desired_state_operation(&first_pool, 1, operation_id, BulkChallengeAction::Enable);
    let second =
        claim_desired_state_operation(&second_pool, 1, operation_id, BulkChallengeAction::Enable);
    let (first, second) = tokio::join!(first, second);
    let owners = [first.unwrap(), second.unwrap()]
        .into_iter()
        .flatten()
        .count();
    assert_eq!(owners, 1);

    first_pool.close().await;
    second_pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
