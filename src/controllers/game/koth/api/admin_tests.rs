use super::*;

use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::ConnectOptions as _;

const GAME_ID: i32 = 7;
const CHALLENGE_ID: i32 = 9;

#[test]
fn mutation_results_are_never_cacheable() {
    let response = private_no_store(AdminKothObserverModel {
        challenge_id: CHALLENGE_ID,
        revision: 1,
        claim_source: "Api".to_string(),
        configured: true,
        secret_hint: None,
        objective_count: None,
        objective_ids: None,
        objective_schema_hash: None,
        created_at: None,
        rotated_at: None,
        last_used_at: None,
        last_observation_at: None,
        context_path: "/context".to_string(),
        observation_path: "/observations".to_string(),
        operation_id: Some(Uuid::new_v4()),
        secret: None,
    });

    assert_eq!(
        response.headers().get(header::CACHE_CONTROL),
        Some(&HeaderValue::from_static("private, no-store"))
    );
    assert_eq!(
        response.headers().get(header::PRAGMA),
        Some(&HeaderValue::from_static("no-cache"))
    );
}

struct RotationHarness {
    admin: sqlx::PgPool,
    first: sqlx::PgPool,
    second: sqlx::PgPool,
    schema: String,
    actor: Uuid,
    other_actor: Uuid,
}

impl RotationHarness {
    async fn new() -> Self {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let schema = format!("koth_observer_rotation_{}", Uuid::new_v4().simple());
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("connect disposable PostgreSQL");
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .expect("create isolated observer schema");
        let options = |application_name: &str| {
            PgConnectOptions::from_str(&database_url)
                .expect("parse disposable PostgreSQL URL")
                .application_name(application_name)
                .disable_statement_logging()
                .options([("search_path", schema.as_str())])
        };
        let first = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(options("rsctf:test:koth-observer:first"))
            .await
            .expect("connect first replica pool");
        let second = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(options("rsctf:test:koth-observer:second"))
            .await
            .expect("connect second replica pool");
        sqlx::raw_sql(
            r#"
            CREATE TABLE "AspNetUsers" (id UUID PRIMARY KEY);
            CREATE TABLE "GameChallenges" (
              id INTEGER PRIMARY KEY,
              game_id INTEGER NOT NULL,
              "Type" SMALLINT NOT NULL,
              UNIQUE (game_id, id)
            );
            CREATE TABLE "KothOfficialConfigs" (
              game_id INTEGER PRIMARY KEY,
              hills_snapshot JSONB NOT NULL
            );
            CREATE TABLE "KothApiObservers" (
              challenge_id INTEGER PRIMARY KEY,
              game_id INTEGER NOT NULL,
              hmac_secret TEXT NOT NULL,
              secret_hint TEXT NOT NULL,
              created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
              rotated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
              last_used_at TIMESTAMPTZ NULL,
              UNIQUE (game_id, challenge_id)
            );
            CREATE TABLE "KothApiArenaSchemes" (
              challenge_id INTEGER PRIMARY KEY,
              game_id INTEGER NOT NULL,
              objective_count SMALLINT,
              objective_ids TEXT[],
              objective_schema_hash BYTEA
            );
            CREATE TABLE "KothTargets" (
              id INTEGER PRIMARY KEY,
              game_id INTEGER NOT NULL,
              challenge_id INTEGER NOT NULL,
              UNIQUE (game_id, challenge_id)
            );
            CREATE TABLE "KothApiSnapshots" (
              target_id INTEGER PRIMARY KEY,
              accepted_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
            );
            CREATE TABLE "KothApiRequestReplays" (
              request_hash BYTEA PRIMARY KEY,
              challenge_id INTEGER NOT NULL
            );
            CREATE TABLE "ObserverClearAudit" (
              input_kind TEXT PRIMARY KEY,
              deleted_rows INTEGER NOT NULL DEFAULT 0
            );
            CREATE FUNCTION observer_clear_audit() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN
              INSERT INTO "ObserverClearAudit" (input_kind, deleted_rows)
              VALUES (TG_ARGV[0], 1)
              ON CONFLICT (input_kind) DO UPDATE
                SET deleted_rows = "ObserverClearAudit".deleted_rows + 1;
              RETURN OLD;
            END $$;
            CREATE TRIGGER observer_snapshot_clear
              AFTER DELETE ON "KothApiSnapshots"
              FOR EACH ROW EXECUTE FUNCTION observer_clear_audit('snapshot');
            CREATE TRIGGER observer_replay_clear
              AFTER DELETE ON "KothApiRequestReplays"
              FOR EACH ROW EXECUTE FUNCTION observer_clear_audit('replay');
            "#,
        )
        .execute(&first)
        .await
        .expect("create observer fixture tables");
        let legacy_challenge_id = CHALLENGE_ID + 1;
        sqlx::query(r#"INSERT INTO "GameChallenges" VALUES ($1, $2, $3)"#)
            .bind(legacy_challenge_id)
            .bind(GAME_ID)
            .bind(ChallengeType::KingOfTheHill as i16)
            .execute(&first)
            .await
            .expect("seed pre-migration KotH challenge");
        sqlx::query(
            r#"INSERT INTO "KothApiObservers"
                 (challenge_id, game_id, hmac_secret, secret_hint)
               VALUES ($1, $2, 'legacy-fixture', '…fixture')"#,
        )
        .bind(legacy_challenge_id)
        .bind(GAME_ID)
        .execute(&first)
        .await
        .expect("seed pre-migration observer");
        sqlx::raw_sql(crate::migrations::KOTH_OBSERVER_ROTATION_SQL)
            .execute(&first)
            .await
            .expect("apply observer operation migration");
        sqlx::raw_sql(crate::migrations::KOTH_OBSERVER_ROTATION_SQL)
            .execute(&first)
            .await
            .expect("observer operation migration is reentrant");
        let backfilled_revision: i64 = sqlx::query_scalar(
            r#"SELECT revision FROM "KothApiObserverRevisions"
                WHERE challenge_id = $1 AND game_id = $2"#,
        )
        .bind(legacy_challenge_id)
        .bind(GAME_ID)
        .fetch_one(&first)
        .await
        .expect("read pre-migration observer revision");
        assert_eq!(backfilled_revision, 1);
        sqlx::query(r#"DELETE FROM "KothApiObservers" WHERE challenge_id = $1"#)
            .bind(legacy_challenge_id)
            .execute(&first)
            .await
            .expect("remove pre-migration observer fixture");
        sqlx::query(r#"DELETE FROM "GameChallenges" WHERE id = $1"#)
            .bind(legacy_challenge_id)
            .execute(&first)
            .await
            .expect("remove pre-migration challenge fixture");

        let actor = Uuid::new_v4();
        let other_actor = Uuid::new_v4();
        sqlx::query(r#"INSERT INTO "AspNetUsers" (id) VALUES ($1), ($2)"#)
            .bind(actor)
            .bind(other_actor)
            .execute(&first)
            .await
            .expect("seed observer actors");
        sqlx::query(r#"INSERT INTO "GameChallenges" VALUES ($1, $2, $3)"#)
            .bind(CHALLENGE_ID)
            .bind(GAME_ID)
            .bind(ChallengeType::KingOfTheHill as i16)
            .execute(&first)
            .await
            .expect("seed KotH challenge");
        sqlx::query(r#"INSERT INTO "KothTargets" VALUES (3, $1, $2)"#)
            .bind(GAME_ID)
            .bind(CHALLENGE_ID)
            .execute(&first)
            .await
            .expect("seed KotH target");
        sqlx::query(
            r#"INSERT INTO "KothApiObservers"
                 (challenge_id, game_id, hmac_secret, secret_hint)
               VALUES ($1, $2, 'cutover-fixture', '…fixture')"#,
        )
        .bind(CHALLENGE_ID)
        .bind(GAME_ID)
        .execute(&first)
        .await
        .expect("seed post-migration observer without a revision row");

        Self {
            admin,
            first,
            second,
            schema,
            actor,
            other_actor,
        }
    }

    async fn seed_referee_input(&self, marker: u8) {
        sqlx::query(
            r#"INSERT INTO "KothApiSnapshots" (target_id) VALUES (3)
               ON CONFLICT (target_id) DO UPDATE SET accepted_at = clock_timestamp()"#,
        )
        .execute(&self.first)
        .await
        .expect("seed observer snapshot");
        sqlx::query(
            r#"INSERT INTO "KothApiRequestReplays" (request_hash, challenge_id)
               VALUES ($1, $2)"#,
        )
        .bind(vec![marker; 32])
        .bind(CHALLENGE_ID)
        .execute(&self.first)
        .await
        .expect("seed observer replay");
    }

    async fn reset_clear_audit(&self) {
        sqlx::query(r#"DELETE FROM "ObserverClearAudit""#)
            .execute(&self.first)
            .await
            .expect("reset clear audit");
    }

    async fn assert_one_clear(&self) {
        let rows = sqlx::query_as::<_, (String, i32)>(
            r#"SELECT input_kind, deleted_rows
                 FROM "ObserverClearAudit" ORDER BY input_kind"#,
        )
        .fetch_all(&self.first)
        .await
        .expect("read clear audit");
        assert_eq!(
            rows,
            vec![("replay".to_string(), 1), ("snapshot".to_string(), 1)]
        );
    }

    async fn cleanup(self) {
        self.first.close().await;
        self.second.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{}" CASCADE"#, self.schema))
            .execute(&self.admin)
            .await
            .expect("drop isolated observer schema");
        self.admin.close().await;
    }
}

async fn mutate_with_game_lock(
    pool: &sqlx::PgPool,
    actor: Uuid,
    kind: ObserverOperationKind,
    request: ObserverMutationRequest,
) -> AppResult<ObserverMutationOutcome> {
    let key = crate::services::ad_engine::game_lock_key(GAME_ID);
    let mut lock = crate::utils::single_flight::PgAdvisoryLock::acquire(pool, &key)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let result = mutate_observer_locked(
        lock.transaction_mut(),
        GAME_ID,
        CHALLENGE_ID,
        actor,
        kind,
        &request,
    )
    .await;
    match result {
        Ok(outcome) => {
            lock.release()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            Ok(outcome)
        }
        Err(error) => {
            lock.rollback()
                .await
                .map_err(|rollback| AppError::internal(rollback.to_string()))?;
            Err(error)
        }
    }
}

async fn recover_with_game_lock(
    pool: &sqlx::PgPool,
    actor: Uuid,
    operation_id: Uuid,
) -> AppResult<ObserverMutationOutcome> {
    let key = crate::services::ad_engine::game_lock_key(GAME_ID);
    let mut lock = crate::utils::single_flight::PgAdvisoryLock::acquire(pool, &key)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let result = recover_observer_locked(
        lock.transaction_mut(),
        GAME_ID,
        CHALLENGE_ID,
        actor,
        operation_id,
    )
    .await;
    match result {
        Ok(outcome) => {
            lock.release()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            Ok(outcome)
        }
        Err(error) => {
            lock.rollback()
                .await
                .map_err(|rollback| AppError::internal(rollback.to_string()))?;
            Err(error)
        }
    }
}

fn request(operation_id: Uuid, expected_revision: i64) -> ObserverMutationRequest {
    ObserverMutationRequest {
        operation_id,
        expected_revision,
    }
}

fn one_success(
    first: AppResult<ObserverMutationOutcome>,
    second: AppResult<ObserverMutationOutcome>,
) -> ObserverMutationOutcome {
    match (first, second) {
        (Ok(outcome), Err(error)) | (Err(error), Ok(outcome)) => {
            assert_eq!(error.status(), axum::http::StatusCode::CONFLICT);
            outcome
        }
        _ => panic!("exactly one competing observer mutation must succeed"),
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn rotation_is_recoverable_exactly_once_across_replicas_and_observation_races() {
    let harness = RotationHarness::new().await;

    harness.seed_referee_input(1).await;
    let operation_id = Uuid::new_v4();
    let (first, duplicate) = tokio::join!(
        mutate_with_game_lock(
            &harness.first,
            harness.actor,
            ObserverOperationKind::Rotate,
            request(operation_id, 0),
        ),
        mutate_with_game_lock(
            &harness.second,
            harness.actor,
            ObserverOperationKind::Rotate,
            request(operation_id, 0),
        ),
    );
    let first = first.expect("first exact operation result");
    let duplicate = duplicate.expect("duplicate exact operation result");
    assert!(first.model.secret.is_some());
    assert!(first.model.secret.as_deref() == duplicate.model.secret.as_deref());
    assert_eq!(first.model.revision, 1);
    assert_eq!(duplicate.model.revision, 1);
    assert_ne!(first.fresh, duplicate.fresh);
    harness.assert_one_clear().await;
    let operation_audit: (i64, i32) = sqlx::query_as(
        r#"SELECT result_revision, disclosure_count
             FROM "KothApiObserverOperations" WHERE operation_id = $1"#,
    )
    .bind(operation_id)
    .fetch_one(&harness.first)
    .await
    .expect("read exact operation audit");
    assert_eq!(operation_audit, (1, 2));

    let rebound = match mutate_with_game_lock(
        &harness.second,
        harness.actor,
        ObserverOperationKind::Revoke,
        request(operation_id, 0),
    )
    .await
    {
        Ok(_) => panic!("one operation identity accepted different mutation input"),
        Err(error) => error,
    };
    assert_eq!(rebound.status(), axum::http::StatusCode::CONFLICT);

    let recovered = recover_with_game_lock(&harness.second, harness.actor, operation_id)
        .await
        .expect("recover lost rotation response");
    assert!(recovered.model.secret.as_deref() == first.model.secret.as_deref());
    let disclosure_count: i32 = sqlx::query_scalar(
        r#"SELECT disclosure_count FROM "KothApiObserverOperations"
            WHERE operation_id = $1"#,
    )
    .bind(operation_id)
    .fetch_one(&harness.first)
    .await
    .expect("read recovery disclosure count");
    assert_eq!(disclosure_count, 3);
    let unauthorized =
        match recover_with_game_lock(&harness.second, harness.other_actor, operation_id).await {
            Ok(_) => panic!("another operator recovered a one-time result"),
            Err(error) => error,
        };
    assert_eq!(unauthorized.status(), axum::http::StatusCode::NOT_FOUND);

    harness.reset_clear_audit().await;
    harness.seed_referee_input(2).await;
    let (competing_a, competing_b) = tokio::join!(
        mutate_with_game_lock(
            &harness.first,
            harness.actor,
            ObserverOperationKind::Rotate,
            request(Uuid::new_v4(), 1),
        ),
        mutate_with_game_lock(
            &harness.second,
            harness.other_actor,
            ObserverOperationKind::Rotate,
            request(Uuid::new_v4(), 1),
        ),
    );
    let winner = one_success(competing_a, competing_b);
    assert_eq!(winner.model.revision, 2);
    harness.assert_one_clear().await;
    let current_secret: String = sqlx::query_scalar(
        r#"SELECT hmac_secret FROM "KothApiObservers"
            WHERE game_id = $1 AND challenge_id = $2"#,
    )
    .bind(GAME_ID)
    .bind(CHALLENGE_ID)
    .fetch_one(&harness.first)
    .await
    .expect("read current observer credential");
    assert!(winner.model.secret.as_deref() == Some(current_secret.as_str()));

    harness.reset_clear_audit().await;
    let mut observation = harness
        .first
        .begin()
        .await
        .expect("begin active observation");
    let observed_secret: String = sqlx::query_scalar(
        r#"SELECT hmac_secret FROM "KothApiObservers"
            WHERE game_id = $1 AND challenge_id = $2
            FOR UPDATE"#,
    )
    .bind(GAME_ID)
    .bind(CHALLENGE_ID)
    .fetch_one(&mut *observation)
    .await
    .expect("lock active observer credential");
    sqlx::query(
        r#"INSERT INTO "KothApiSnapshots" (target_id) VALUES (3)
           ON CONFLICT (target_id) DO UPDATE SET accepted_at = clock_timestamp()"#,
    )
    .execute(&mut *observation)
    .await
    .expect("stage active observation snapshot");
    sqlx::query(
        r#"INSERT INTO "KothApiRequestReplays" (request_hash, challenge_id)
           VALUES ($1, $2)"#,
    )
    .bind(vec![3_u8; 32])
    .bind(CHALLENGE_ID)
    .execute(&mut *observation)
    .await
    .expect("stage active observation replay");
    let second_pool = harness.second.clone();
    let actor = harness.actor;
    let mut rotating = tokio::spawn(async move {
        mutate_with_game_lock(
            &second_pool,
            actor,
            ObserverOperationKind::Rotate,
            request(Uuid::new_v4(), 2),
        )
        .await
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(75), &mut rotating)
            .await
            .is_err(),
        "rotation crossed an active observer row lock"
    );
    observation
        .commit()
        .await
        .expect("commit active observation before rotation");
    let post_observation = rotating
        .await
        .expect("rotation task joined")
        .expect("rotation resumed after observation");
    assert_eq!(post_observation.model.revision, 3);
    assert!(post_observation.model.secret.as_deref() != Some(observed_secret.as_str()));
    harness.assert_one_clear().await;

    harness.reset_clear_audit().await;
    harness.seed_referee_input(4).await;
    let (rotate, revoke) = tokio::join!(
        mutate_with_game_lock(
            &harness.first,
            harness.actor,
            ObserverOperationKind::Rotate,
            request(Uuid::new_v4(), 3),
        ),
        mutate_with_game_lock(
            &harness.second,
            harness.actor,
            ObserverOperationKind::Revoke,
            request(Uuid::new_v4(), 3),
        ),
    );
    let race_winner = one_success(rotate, revoke);
    assert_eq!(race_winner.model.revision, 4);
    let configured: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
             SELECT 1 FROM "KothApiObservers"
              WHERE game_id = $1 AND challenge_id = $2
           )"#,
    )
    .bind(GAME_ID)
    .bind(CHALLENGE_ID)
    .fetch_one(&harness.first)
    .await
    .expect("read post-race observer state");
    assert_eq!(
        configured,
        matches!(race_winner.kind, ObserverOperationKind::Rotate)
    );
    if configured {
        let stored: String = sqlx::query_scalar(
            r#"SELECT hmac_secret FROM "KothApiObservers"
                WHERE game_id = $1 AND challenge_id = $2"#,
        )
        .bind(GAME_ID)
        .bind(CHALLENGE_ID)
        .fetch_one(&harness.first)
        .await
        .expect("read post-race observer credential");
        assert!(race_winner.model.secret.as_deref() == Some(stored.as_str()));
    } else {
        assert!(race_winner.model.secret.is_none());
    }
    harness.assert_one_clear().await;

    let final_revision: i64 = sqlx::query_scalar(
        r#"SELECT revision FROM "KothApiObserverRevisions"
            WHERE game_id = $1 AND challenge_id = $2"#,
    )
    .bind(GAME_ID)
    .bind(CHALLENGE_ID)
    .fetch_one(&harness.first)
    .await
    .expect("read final observer revision");
    assert_eq!(final_revision, 4);

    harness.cleanup().await;
}
