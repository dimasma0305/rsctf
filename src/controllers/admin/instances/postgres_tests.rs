use super::*;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn instance_projection_resolves_every_ownership_shape() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect test database");
    let schema = format!("admin_instances_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .expect("create isolated schema");
    let options = PgConnectOptions::from_str(&database_url)
        .expect("parse test database URL")
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .expect("connect isolated pool");

    sqlx::raw_sql(
        r#"
        CREATE TABLE "Containers" (
          id UUID PRIMARY KEY,
          image TEXT NOT NULL,
          container_id TEXT NOT NULL,
          started_at TIMESTAMPTZ NOT NULL,
          expect_stop_at TIMESTAMPTZ NOT NULL,
          is_proxy BOOLEAN NOT NULL,
          ip TEXT NOT NULL,
          port INTEGER NOT NULL,
          public_ip TEXT,
          public_port INTEGER,
          game_instance_id INTEGER,
          exercise_instance_id INTEGER,
          ad_team_service_id INTEGER
        );
        CREATE TABLE "GameInstances" (
          id INTEGER PRIMARY KEY,
          challenge_id INTEGER NOT NULL,
          participation_id INTEGER NOT NULL
        );
        CREATE TABLE "GameChallenges" (
          id INTEGER PRIMARY KEY,
          title TEXT NOT NULL,
          category SMALLINT NOT NULL,
          shared_container_id UUID,
          test_container_id UUID
        );
        CREATE TABLE "Participations" (
          id INTEGER PRIMARY KEY,
          team_id INTEGER NOT NULL
        );
        CREATE TABLE "Teams" (
          id INTEGER PRIMARY KEY,
          name TEXT NOT NULL,
          avatar_hash TEXT
        );
        CREATE TABLE "AdTeamServices" (
          id INTEGER PRIMARY KEY,
          participation_id INTEGER NOT NULL,
          challenge_id INTEGER NOT NULL,
          container_id TEXT
        );
        CREATE TABLE "ExerciseInstances" (
          id INTEGER PRIMARY KEY,
          exercise_id INTEGER NOT NULL,
          user_id UUID NOT NULL,
          container_id UUID
        );
        CREATE TABLE "ExerciseChallenges" (
          id INTEGER PRIMARY KEY,
          title TEXT NOT NULL,
          category SMALLINT NOT NULL
        );
        CREATE TABLE "AspNetUsers" (
          id UUID PRIMARY KEY,
          user_name TEXT,
          real_name TEXT NOT NULL
        );

        INSERT INTO "Teams" VALUES
            (7, 'red', 'red-avatar'),
            (8, 'late-page-team', NULL);
        INSERT INTO "Participations" VALUES (11, 7), (12, 8);
        INSERT INTO "GameChallenges"
            (id, title, category, shared_container_id, test_container_id)
        VALUES
            (20, 'per-team', 3, NULL, NULL),
            (21, 'the-hill', 0, '00000000-0000-0000-0000-000000000002', NULL),
            (22, 'admin-test', 0, NULL, '00000000-0000-0000-0000-000000000003'),
            (23, 'late-page-challenge', 3, NULL, NULL);
        INSERT INTO "GameInstances" VALUES (30, 20, 11), (31, 23, 12);
        INSERT INTO "AspNetUsers"
        VALUES ('00000000-0000-0000-0000-000000000099', 'alice', 'Alice');
        INSERT INTO "ExerciseChallenges" VALUES (40, 'practice-web', 3);
        INSERT INTO "ExerciseInstances"
        VALUES (
            41,
            40,
            '00000000-0000-0000-0000-000000000099',
            '00000000-0000-0000-0000-000000000004'
        );
        INSERT INTO "Containers"
            (id, image, container_id, started_at, expect_stop_at, is_proxy,
             ip, port, public_ip, public_port, game_instance_id,
             exercise_instance_id, ad_team_service_id)
        VALUES
            ('00000000-0000-0000-0000-000000000001', 'team-image', 'runtime-1',
             now(), now() + interval '1 hour', TRUE, '10.0.0.1', 8080,
             NULL, NULL, 30, NULL, NULL),
            ('00000000-0000-0000-0000-000000000002', 'hill-image', 'runtime-2',
             now(), now() + interval '1 hour', FALSE, '10.0.0.2', 8080,
             NULL, NULL, NULL, NULL, NULL),
            ('00000000-0000-0000-0000-000000000003', 'test-image', 'runtime-3',
             now(), now() + interval '1 hour', FALSE, '10.0.0.3', 8080,
             NULL, NULL, NULL, NULL, NULL),
            ('00000000-0000-0000-0000-000000000004', 'exercise-image', 'runtime-4',
             now(), now() + interval '1 hour', TRUE, '10.0.0.4', 8080,
             '203.0.113.4', 443, NULL, 41, NULL),
            ('00000000-0000-0000-0000-000000000005', 'orphan-image', 'runtime-5',
             now(), now() + interval '1 hour', FALSE, '10.0.0.5', 8080,
             NULL, NULL, NULL, NULL, NULL),
            ('00000000-0000-0000-0000-000000000006', 'late-image', 'runtime-6',
             now() + interval '1 hour', now() + interval '2 hours', FALSE,
             '10.0.0.6', 8080, NULL, NULL, 31, NULL, NULL);
        "#,
    )
    .execute(&pool)
    .await
    .expect("seed ownership shapes");

    let total = sqlx::query_scalar::<_, i64>(INSTANCE_COUNT_SQL.as_str())
        .bind(None::<i32>)
        .bind(None::<i32>)
        .fetch_one(&pool)
        .await
        .expect("count complete inventory");
    let page = sqlx::query_as::<_, ContainerInstanceRow>(INSTANCE_PAGE_SQL.as_str())
        .bind(None::<i32>)
        .bind(None::<i32>)
        .bind(2_i64)
        .bind(0_i64)
        .fetch_all(&pool)
        .await
        .expect("fetch bounded inventory page");
    assert_eq!(total, 6);
    assert_eq!(page.len(), 2);
    assert!(page.iter().all(|row| row.team_id != Some(8)));

    let rows = sqlx::query_as::<_, ContainerInstanceRow>(INSTANCE_PAGE_SQL.as_str())
        .bind(None::<i32>)
        .bind(None::<i32>)
        .bind(100_i64)
        .bind(0_i64)
        .fetch_all(&pool)
        .await
        .expect("project instances");
    let models = rows
        .into_iter()
        .map(project_instance)
        .collect::<AppResult<Vec<_>>>()
        .expect("decode projections");

    let team = &models[0];
    assert_eq!(team.owner_kind, ContainerOwnerKind::Team);
    assert_eq!(
        team.team.as_ref().map(|value| value.name.as_str()),
        Some("red")
    );
    assert_eq!(
        team.challenge.as_ref().map(|value| value.title.as_str()),
        Some("per-team")
    );
    assert!(team.is_proxy);

    let shared = &models[1];
    assert_eq!(shared.owner_kind, ContainerOwnerKind::Shared);
    assert!(shared.team.is_none());
    assert_eq!(
        shared.challenge.as_ref().map(|value| value.title.as_str()),
        Some("the-hill")
    );
    assert!(!shared.is_proxy);

    assert_eq!(models[2].owner_kind, ContainerOwnerKind::AdminTest);
    assert_eq!(models[3].owner_kind, ContainerOwnerKind::Exercise);
    assert_eq!(models[3].owner_name.as_deref(), Some("alice"));
    assert_eq!(models[3].ip, "203.0.113.4");
    assert_eq!(models[3].port, 443);
    assert_eq!(models[4].owner_kind, ContainerOwnerKind::Unassigned);
    assert_eq!(models[5].team.as_ref().map(|team| team.id), Some(8));

    let late_team_total = sqlx::query_scalar::<_, i64>(INSTANCE_COUNT_SQL.as_str())
        .bind(Some(8_i32))
        .bind(None::<i32>)
        .fetch_one(&pool)
        .await
        .expect("count filtered team inventory");
    let late_team_rows = sqlx::query_as::<_, ContainerInstanceRow>(INSTANCE_PAGE_SQL.as_str())
        .bind(Some(8_i32))
        .bind(None::<i32>)
        .bind(25_i64)
        .bind(0_i64)
        .fetch_all(&pool)
        .await
        .expect("fetch team outside the first unfiltered page");
    assert_eq!(late_team_total, 1);
    assert_eq!(late_team_rows.len(), 1);
    assert_eq!(late_team_rows[0].container_id, "runtime-6");

    let practice_total = sqlx::query_scalar::<_, i64>(INSTANCE_COUNT_SQL.as_str())
        .bind(None::<i32>)
        .bind(Some(40_i32))
        .fetch_one(&pool)
        .await
        .expect("count filtered challenge inventory");
    let practice_rows = sqlx::query_as::<_, ContainerInstanceRow>(INSTANCE_PAGE_SQL.as_str())
        .bind(None::<i32>)
        .bind(Some(40_i32))
        .bind(25_i64)
        .bind(0_i64)
        .fetch_all(&pool)
        .await
        .expect("fetch challenge outside the first unfiltered page");
    assert_eq!(practice_total, 1);
    assert_eq!(practice_rows.len(), 1);
    assert_eq!(practice_rows[0].container_id, "runtime-4");

    let no_combined_match = sqlx::query_scalar::<_, i64>(INSTANCE_COUNT_SQL.as_str())
        .bind(Some(8_i32))
        .bind(Some(40_i32))
        .fetch_one(&pool)
        .await
        .expect("count combined filters");
    assert_eq!(no_combined_match, 0);

    let team_options = sqlx::query_as::<_, ContainerInstanceFilterOptionRow>(
        INSTANCE_TEAM_FILTER_OPTIONS_SQL.as_str(),
    )
    .bind("late-page")
    .bind(30_i64)
    .fetch_all(&pool)
    .await
    .expect("discover team option outside the current page");
    assert_eq!(team_options.len(), 1);
    assert_eq!(team_options[0].id, 8);
    assert_eq!(team_options[0].total, 1);

    let challenge_options = sqlx::query_as::<_, ContainerInstanceFilterOptionRow>(
        INSTANCE_CHALLENGE_FILTER_OPTIONS_SQL.as_str(),
    )
    .bind("practice")
    .bind(30_i64)
    .fetch_all(&pool)
    .await
    .expect("discover challenge option outside the current page");
    assert_eq!(challenge_options.len(), 1);
    assert_eq!(challenge_options[0].id, 40);
    assert_eq!(challenge_options[0].total, 1);

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .expect("drop isolated schema");
    admin.close().await;
}
