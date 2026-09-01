use std::sync::{atomic::Ordering, Arc};

use axum::extract::{Path, State};
use sea_orm::SqlxPostgresConnector;
use serde_json::Value;
use sqlx::postgres::PgPoolOptions;
use sqlx::types::Json;

use super::*;
use crate::app_state::{AppState, SharedState};
use crate::middlewares::privilege_authentication::CurrentUser;
use crate::models::internal::configs::AppConfig;
use crate::services::cache::InMemoryCache;
use crate::services::container::NoopContainerManager;
use crate::services::token::TokenService;
use crate::storage::LocalBlobStorage;
use crate::utils::enums::Role;

const SERVICE_COUNT: i32 = 8;

async fn operator_test_pool() -> sqlx::PgPool {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect operator-console test database");
    sqlx::raw_sql(
        r#"
        CREATE TEMP TABLE "Games" (
          id INTEGER PRIMARY KEY,
          ad_scoring_paused BOOLEAN NOT NULL,
          ad_scoring_paused_at TIMESTAMPTZ NULL,
          ad_control_revision BIGINT NOT NULL,
          start_time_utc TIMESTAMPTZ NOT NULL,
          end_time_utc TIMESTAMPTZ NOT NULL
        );
        CREATE TEMP TABLE "GameManagers" (
          id INTEGER PRIMARY KEY,
          game_id INTEGER NOT NULL,
          user_id UUID NOT NULL
        );
        CREATE TEMP TABLE "AdRounds" (
          id INTEGER PRIMARY KEY,
          game_id INTEGER NOT NULL,
          number INTEGER NOT NULL,
          start_time_utc TIMESTAMPTZ NOT NULL,
          end_time_utc TIMESTAMPTZ NOT NULL
        );
        CREATE TEMP TABLE "Participations" (
          id INTEGER PRIMARY KEY,
          game_id INTEGER NOT NULL,
          status SMALLINT NOT NULL
        );
        CREATE TEMP TABLE "GameChallenges" (
          id INTEGER PRIMARY KEY,
          game_id INTEGER NOT NULL,
          "Type" SMALLINT NOT NULL
        );
        CREATE TEMP TABLE "AdTeamServices" (
          id INTEGER PRIMARY KEY,
          game_id INTEGER NOT NULL,
          participation_id INTEGER NOT NULL,
          challenge_id INTEGER NOT NULL
        );
        CREATE TEMP TABLE "AdCheckResults" (
          id INTEGER PRIMARY KEY,
          team_service_id INTEGER NOT NULL,
          status SMALLINT NOT NULL,
          checked_at TIMESTAMPTZ NOT NULL
        );
        CREATE TEMP TABLE "AdFlags" (
          id INTEGER PRIMARY KEY,
          round_id INTEGER NOT NULL,
          team_service_id INTEGER NOT NULL,
          flag TEXT NOT NULL
        );
        CREATE TEMP TABLE "KothControlResults" (
          id INTEGER PRIMARY KEY,
          game_id INTEGER NOT NULL,
          challenge_id INTEGER NOT NULL,
          checked_at TIMESTAMPTZ NOT NULL,
          status SMALLINT NOT NULL,
          ad_round_id INTEGER NOT NULL,
          confirmed_participation_id INTEGER NULL,
          cycle_id BIGINT NULL,
          token_window_attempt INTEGER NOT NULL
        );
        "#,
    )
    .execute(&pool)
    .await
    .expect("create operator-console fixture tables");
    sqlx::raw_sql(crate::migrations::OPERATOR_LATEST_INDEX_SQL)
        .execute(&pool)
        .await
        .expect("install operator-console latest-row indexes");
    sqlx::raw_sql(crate::migrations::OPERATOR_LATEST_INDEX_SQL)
        .execute(&pool)
        .await
        .expect("operator-console latest-row indexes are idempotent");
    pool
}

async fn seed_live_projection(pool: &sqlx::PgPool) {
    sqlx::raw_sql(
        r#"INSERT INTO "Games" VALUES (
             1, FALSE, NULL, 0, '2026-08-27 08:00:00+00', '2026-08-27 10:00:00+00'
           );
           INSERT INTO "AdRounds" VALUES
             (17, 1, 41, '2026-08-27 08:00:00+00', '2026-08-27 08:00:05+00');"#,
    )
    .execute(pool)
    .await
    .expect("seed game and round");
    sqlx::query(r#"INSERT INTO "GameChallenges" VALUES (11, 1, $1)"#)
        .bind(ChallengeType::AttackDefense as i16)
        .execute(pool)
        .await
        .expect("seed A&D challenge");
    sqlx::query(
        r#"INSERT INTO "Participations"
           SELECT n, 1, $1 FROM generate_series(1, $2) n"#,
    )
    .bind(ParticipationStatus::Accepted as i16)
    .bind(SERVICE_COUNT)
    .execute(pool)
    .await
    .expect("seed accepted participations");
    sqlx::query(
        r#"INSERT INTO "AdTeamServices"
           SELECT n, 1, n, 11 FROM generate_series(1, $1) n"#,
    )
    .bind(SERVICE_COUNT)
    .execute(pool)
    .await
    .expect("seed accepted A&D services");
    sqlx::query(
        r#"INSERT INTO "AdFlags"
           SELECT n, 17, n, 'flag-' || n FROM generate_series(1, $1) n"#,
    )
    .bind(SERVICE_COUNT)
    .execute(pool)
    .await
    .expect("seed current flags");
}

async fn append_check_history(pool: &sqlx::PgPool, first: i32, last: i32) {
    sqlx::query(
        r#"INSERT INTO "AdCheckResults" (id, team_service_id, status, checked_at)
           SELECT service * 1000000 + tick,
                  service,
                  (tick % 4)::smallint,
                  '2026-08-27 00:00:00+00'::timestamptz
                    + make_interval(secs => tick::double precision / 1000.0)
             FROM generate_series(1, $1) service
            CROSS JOIN generate_series($2, $3) tick"#,
    )
    .bind(SERVICE_COUNT)
    .bind(first)
    .bind(last)
    .execute(pool)
    .await
    .expect("append checker history");
}

fn index_work(plan: &Value, index_name: &str) -> Vec<(f64, f64)> {
    fn count(value: Option<&Value>) -> f64 {
        value.and_then(Value::as_f64).unwrap_or(0.0)
    }

    let mut found = Vec::new();
    fn visit(value: &Value, index_name: &str, found: &mut Vec<(f64, f64)>) {
        match value {
            Value::Object(object) => {
                if object.get("Index Name").and_then(Value::as_str) == Some(index_name) {
                    let rows = count(object.get("Actual Rows"));
                    let loops = count(object.get("Actual Loops"));
                    found.push((rows, loops));
                }
                object
                    .values()
                    .for_each(|child| visit(child, index_name, found));
            }
            Value::Array(values) => values
                .iter()
                .for_each(|child| visit(child, index_name, found)),
            _ => {}
        }
    }
    visit(plan, index_name, &mut found);
    found
}

async fn explain_latest(pool: &sqlx::PgPool) -> Value {
    let explain = format!(
        "EXPLAIN (ANALYZE, BUFFERS, COSTS OFF, FORMAT JSON) {}",
        LATEST_AD_CHECKS_SQL
    );
    let service_ids: Vec<i32> = (1..=SERVICE_COUNT).collect();
    let Json(plan) = sqlx::query_scalar::<_, Json<Value>>(&explain)
        .bind(service_ids)
        .fetch_one(pool)
        .await
        .expect("explain latest-result lookup");
    plan
}

fn operator_state(pool: sqlx::PgPool) -> SharedState {
    AppState::new(
        SqlxPostgresConnector::from_sqlx_postgres_pool(pool),
        Arc::new(AppConfig::default()),
        Arc::new(InMemoryCache::new()),
        Arc::new(LocalBlobStorage::new(
            std::env::temp_dir().join("rsctf-operator-console-read-test"),
        )),
        TokenService::new("0123456789abcdef0123456789abcdef", 60),
        Arc::new(NoopContainerManager),
    )
}

fn ordinary_user(id: uuid::Uuid) -> CurrentUser {
    CurrentUser {
        id,
        role: Role::User,
        name: "operator-read-test".to_string(),
        security_stamp: "operator-read-test-stamp".to_string(),
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn latest_verdict_work_and_live_query_count_stay_bounded_as_history_grows() {
    let pool = operator_test_pool().await;
    seed_live_projection(&pool).await;
    append_check_history(&pool, 1, 5_000).await;
    sqlx::raw_sql(r#"VACUUM (ANALYZE) "AdCheckResults""#)
        .execute(&pool)
        .await
        .expect("analyze initial checker history");

    let service_ids: Vec<i32> = (1..=SERVICE_COUNT).collect();
    let first = latest_ad_checks(&pool, &service_ids)
        .await
        .expect("load initial latest checks");
    assert_eq!(first.len(), SERVICE_COUNT as usize);
    assert!(first.values().all(|row| row.last_check_id.is_some()));
    let first_plan = explain_latest(&pool).await;
    let first_work = index_work(&first_plan, "ix_adcheckresults_service_latest");
    assert_eq!(first_work, vec![(1.0, SERVICE_COUNT as f64)]);

    append_check_history(&pool, 5_001, 25_000).await;
    sqlx::raw_sql(r#"VACUUM (ANALYZE) "AdCheckResults""#)
        .execute(&pool)
        .await
        .expect("analyze long checker history");
    let latest = latest_ad_checks(&pool, &service_ids)
        .await
        .expect("load latest checks after history growth");
    assert!(latest
        .values()
        .all(|row| row.last_check_id.is_some_and(|id| id % 1_000_000 == 25_000)));
    let long_plan = explain_latest(&pool).await;
    let long_work = index_work(&long_plan, "ix_adcheckresults_service_latest");
    assert_eq!(
        long_work, first_work,
        "history growth changed latest-row work"
    );

    AD_LIVE_QUERY_EXECUTIONS.store(0, Ordering::Relaxed);
    let (game, cells) = load_ad_live_projection(&pool, 1)
        .await
        .expect("load compact live projection");
    assert_eq!(game.current_round, Some(41));
    assert_eq!(cells.len(), SERVICE_COUNT as usize);
    assert_eq!(
        AD_LIVE_QUERY_EXECUTIONS.load(Ordering::Relaxed),
        AD_LIVE_STATE_QUERY_COUNT
    );
    assert!(cells.iter().all(|cell| cell.current_flag.is_some()));

    let manager_id = uuid::Uuid::new_v4();
    let outsider_id = uuid::Uuid::new_v4();
    let state = operator_state(pool.clone());
    let denied = ad_engine_metadata(State(state.clone()), ordinary_user(outsider_id), Path(1))
        .await
        .expect_err("a non-manager cannot discover configured engines");
    assert_eq!(denied.status(), axum::http::StatusCode::FORBIDDEN);
    let denied_live = ad_live_state(State(state.clone()), ordinary_user(outsider_id), Path(1))
        .await
        .expect_err("a non-manager cannot read live service evidence");
    assert_eq!(denied_live.status(), axum::http::StatusCode::FORBIDDEN);
    sqlx::query(r#"INSERT INTO "GameManagers" VALUES (1, 1, $1)"#)
        .bind(manager_id)
        .execute(&pool)
        .await
        .expect("grant game-manager access");
    let permitted = ad_engine_metadata(State(state.clone()), ordinary_user(manager_id), Path(1))
        .await
        .expect("the game manager may read engine metadata");
    assert!(permitted.data.has_attack_defense);
    assert!(!permitted.data.has_koth);
    let permitted_live = ad_live_state(State(state), ordinary_user(manager_id), Path(1))
        .await
        .expect("the game manager may read live service evidence");
    assert_eq!(permitted_live.data.services.len(), SERVICE_COUNT as usize);
}
