use std::{
    str::FromStr,
    time::{Duration, Instant},
};

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use uuid::Uuid;

use super::{
    monitor_history::{
        load_event_page, load_events_legacy, load_submission_page, load_submissions_legacy,
        MONITOR_EVENTS_SEARCH_SQL, MONITOR_EVENTS_SQL, MONITOR_SUBMISSIONS_SEARCH_SQL,
        MONITOR_SUBMISSIONS_SQL,
    },
    EventQuery, SubmissionQuery,
};

const TEST_INDEX_SQL: &str = r#"
CREATE EXTENSION IF NOT EXISTS pg_trgm;
CREATE INDEX ix_gameevents_monitor_page
    ON "GameEvents" (game_id, publish_time_utc DESC, id DESC);
CREATE INDEX ix_gameevents_game_user
    ON "GameEvents" (game_id, user_id) WHERE user_id IS NOT NULL;
CREATE INDEX ix_gameevents_game_team_type
    ON "GameEvents" (game_id, team_id, "Type");
CREATE INDEX ix_submissions_game_time
    ON "Submissions" (game_id, submit_time_utc DESC, id DESC);
CREATE INDEX ix_submissions_monitor_status_page
    ON "Submissions" (game_id, status, submit_time_utc DESC, id DESC);
CREATE INDEX ix_submissions_game_team
    ON "Submissions" (game_id, team_id);
CREATE INDEX ix_submissions_game_user
    ON "Submissions" (game_id, user_id) WHERE user_id IS NOT NULL;
CREATE INDEX ix_submissions_game_challenge
    ON "Submissions" (game_id, challenge_id);
CREATE INDEX ix_teams_monitor_name_trgm
    ON "Teams" USING GIN (LOWER(name) gin_trgm_ops);
CREATE INDEX ix_users_monitor_name_trgm
    ON "AspNetUsers" USING GIN (LOWER(user_name) gin_trgm_ops)
    WHERE user_name IS NOT NULL;
CREATE INDEX ix_challenges_monitor_title_trgm
    ON "GameChallenges" USING GIN (LOWER(title) gin_trgm_ops);
CREATE INDEX ix_submissions_monitor_answer_trgm
    ON "Submissions" USING GIST (LOWER(answer) gist_trgm_ops(siglen=64));
CREATE INDEX ix_gameevents_monitor_values_trgm
    ON "GameEvents" USING GIST (LOWER(values::text) gist_trgm_ops(siglen=64));
"#;

async fn statement_calls(pool: &sqlx::PgPool, marker: &str) -> Option<i64> {
    let installed: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'pg_stat_statements')",
    )
    .fetch_one(pool)
    .await
    .ok()?;
    if !installed {
        return None;
    }
    let pattern = format!("%{marker}%");
    sqlx::query_scalar::<_, Option<i64>>(
        "SELECT SUM(calls)::bigint FROM pg_stat_statements WHERE query LIKE $1",
    )
    .bind(pattern)
    .fetch_one(pool)
    .await
    .ok()
    .flatten()
    .or(Some(0))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn large_monitor_history_is_bounded_indexed_and_one_query_per_page() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect test database");
    sqlx::query("CREATE EXTENSION IF NOT EXISTS pg_trgm WITH SCHEMA public")
        .execute(&admin)
        .await
        .expect("install trusted trigram extension in shared public schema");
    let schema = format!("monitor_history_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .expect("create isolated schema");
    let search_path = format!("{schema},public");
    let options = PgConnectOptions::from_str(&database_url)
        .expect("parse database URL")
        .options([("search_path", search_path.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(20)
        .acquire_timeout(Duration::from_secs(5))
        .connect_with(options)
        .await
        .expect("connect isolated schema");

    sqlx::raw_sql(
        r#"
        CREATE TABLE "Teams" (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
        CREATE TABLE "AspNetUsers" (id UUID PRIMARY KEY, user_name TEXT NULL);
        CREATE TABLE "GameChallenges" (
            id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, title TEXT NOT NULL
        );
        CREATE TABLE "GameEvents" (
            id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, "Type" SMALLINT NOT NULL,
            values JSONB NOT NULL, publish_time_utc TIMESTAMPTZ NOT NULL,
            user_id UUID NULL, team_id INTEGER NOT NULL, feed_cursor BIGINT NOT NULL
        );
        CREATE TABLE "Submissions" (
            id INTEGER PRIMARY KEY, answer TEXT NOT NULL, status SMALLINT NOT NULL,
            submit_time_utc TIMESTAMPTZ NOT NULL, user_id UUID NULL,
            team_id INTEGER NOT NULL, game_id INTEGER NOT NULL,
            challenge_id INTEGER NOT NULL
        );
        "#,
    )
    .execute(&pool)
    .await
    .expect("create monitor tables");

    let first_user = Uuid::new_v4();
    let second_user = Uuid::new_v4();
    sqlx::query(r#"INSERT INTO "Teams" (id, name) VALUES (1, '100%_Crew'), (2, 'Other Team')"#)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "AspNetUsers" (id, user_name) VALUES ($1, 'Alice Monitor'), ($2, 'Bob')"#,
    )
    .bind(first_user)
    .bind(second_user)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "GameChallenges" (id, game_id, title)
           VALUES (1, 7, 'Needle Challenge'), (2, 8, 'Other Challenge')"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        r#"INSERT INTO "GameEvents"
               (id, game_id, "Type", values, publish_time_utc, user_id, team_id, feed_cursor)
           SELECT n, 7, (n % 7)::smallint,
                  jsonb_build_array('game-seven-' || n::text),
                  clock_timestamp() - n * interval '1 millisecond',
                  CASE WHEN n % 2 = 0 THEN $1 ELSE $2 END,
                  CASE WHEN n % 2 = 0 THEN 1 ELSE 2 END, n::bigint
             FROM generate_series(1, 30000) n
           UNION ALL
           SELECT 100000 + n, 8, (n % 7)::smallint,
                  jsonb_build_array('game-eight-' || n::text),
                  clock_timestamp() - n * interval '1 millisecond', $2, 2,
                  (100000 + n)::bigint
             FROM generate_series(1, 10000) n"#,
    )
    .bind(first_user)
    .bind(second_user)
    .execute(&pool)
    .await
    .expect("seed large event history");
    sqlx::query(
        r#"INSERT INTO "Submissions"
               (id, answer, status, submit_time_utc, user_id, team_id, game_id, challenge_id)
           SELECT n, 'game-seven-' || n::text, (n % 4)::smallint,
                  clock_timestamp() - n * interval '1 millisecond',
                  CASE WHEN n % 2 = 0 THEN $1 ELSE $2 END,
                  CASE WHEN n % 2 = 0 THEN 1 ELSE 2 END, 7, 1
             FROM generate_series(1, 50000) n
           UNION ALL
           SELECT 100000 + n, 'game-eight-' || n::text, (n % 4)::smallint,
                  clock_timestamp() - n * interval '1 millisecond', $2, 2, 8, 2
             FROM generate_series(1, 20000) n"#,
    )
    .bind(first_user)
    .bind(second_user)
    .execute(&pool)
    .await
    .expect("seed large submission history");
    sqlx::raw_sql(TEST_INDEX_SQL)
        .execute(&pool)
        .await
        .expect("create monitor indexes");
    sqlx::query("ANALYZE")
        .execute(&pool)
        .await
        .expect("analyze monitor fixtures");

    let event_calls_before = statement_calls(&pool, r#""GameEvents" event"#).await;
    let events = load_event_page(
        &pool,
        7,
        &EventQuery {
            count: Some(0),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(events.len(), 100, "count=0 must use the bounded default");
    assert!(events.iter().all(|row| {
        row.values
            .as_array()
            .and_then(|values| values.first())
            .and_then(|value| value.as_str())
            .is_some_and(|value| value.starts_with("game-seven-"))
    }));
    if let (Some(before), Some(after)) = (
        event_calls_before,
        statement_calls(&pool, r#""GameEvents" event"#).await,
    ) {
        assert_eq!(
            after - before,
            1,
            "one event page must issue one feed query"
        );
    }

    let submission_calls_before = statement_calls(&pool, r#""Submissions" submission"#).await;
    let submissions = load_submission_page(
        &pool,
        7,
        &SubmissionQuery {
            count: Some(10_000),
            ..Default::default()
        },
        None,
    )
    .await
    .unwrap();
    assert_eq!(submissions.len(), 100, "oversized count must clamp to 100");
    assert!(submissions
        .iter()
        .all(|row| row.answer.starts_with("game-seven-")));
    if let (Some(before), Some(after)) = (
        submission_calls_before,
        statement_calls(&pool, r#""Submissions" submission"#).await,
    ) {
        assert_eq!(
            after - before,
            1,
            "one submission page must issue one feed query"
        );
    }

    let legacy_events = load_events_legacy(
        &pool,
        7,
        &EventQuery {
            count: Some(0),
            skip: Some(29_999),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        legacy_events.len(),
        30_000,
        "legacy events count=0 must return complete retained history and ignore skip"
    );
    drop(legacy_events);

    let legacy_submissions = load_submissions_legacy(
        &pool,
        7,
        &SubmissionQuery {
            count: Some(0),
            skip: Some(49_999),
            ..Default::default()
        },
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        legacy_submissions.len(),
        50_000,
        "legacy submissions count=0 must return complete retained history and ignore skip"
    );
    drop(legacy_submissions);

    let literal_wildcard = load_event_page(
        &pool,
        7,
        &EventQuery {
            count: Some(100),
            search: Some("%_".repeat(300)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(literal_wildcard.is_empty(), "wildcards must remain literal");

    let named_team = load_event_page(
        &pool,
        7,
        &EventQuery {
            count: Some(100),
            search: Some("  100%_CREW  ".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(named_team.len(), 100);
    assert!(named_team
        .iter()
        .all(|row| row.team.as_deref() == Some("100%_Crew")));

    let concurrent_started = Instant::now();
    let mut concurrent = Vec::new();
    for page in 0..16_u64 {
        let pool = pool.clone();
        concurrent.push(tokio::spawn(async move {
            tokio::time::timeout(
                Duration::from_secs(5),
                load_submission_page(
                    &pool,
                    7,
                    &SubmissionQuery {
                        count: Some(100),
                        skip: Some(page * 100),
                        search: Some("game-seven".into()),
                        ..Default::default()
                    },
                    None,
                ),
            )
            .await
        }));
    }
    for page in concurrent {
        assert_eq!(page.await.unwrap().unwrap().unwrap().len(), 100);
    }
    assert!(
        concurrent_started.elapsed() < Duration::from_secs(10),
        "bounded concurrent monitor pages exceeded the regression budget"
    );

    for (sql, binds, expected_index) in [
        (MONITOR_EVENTS_SQL, false, "ix_gameevents_monitor_page"),
        (MONITOR_SUBMISSIONS_SQL, true, "ix_submissions_game_time"),
    ] {
        let explain = format!("EXPLAIN (FORMAT JSON) {sql}");
        let plan: serde_json::Value = if binds {
            sqlx::query_scalar(&explain)
                .bind(7_i32)
                .bind(Option::<i16>::None)
                .bind(0_i64)
                .bind(100_i64)
                .fetch_one(&pool)
                .await
                .unwrap()
        } else {
            sqlx::query_scalar(&explain)
                .bind(7_i32)
                .bind(false)
                .bind(0_i64)
                .bind(100_i64)
                .fetch_one(&pool)
                .await
                .unwrap()
        };
        assert!(
            plan.to_string().contains(expected_index),
            "monitor plan did not use {expected_index}: {plan}"
        );
    }

    for (sql, status_filter, pattern, expected_index) in [
        (
            MONITOR_EVENTS_SEARCH_SQL,
            false,
            "%game-seven-12345%",
            "ix_gameevents_monitor_values_trgm",
        ),
        (
            MONITOR_SUBMISSIONS_SEARCH_SQL,
            true,
            "%game-seven-12345%",
            "ix_submissions_monitor_answer_trgm",
        ),
    ] {
        let explain = format!("EXPLAIN (FORMAT JSON) {sql}");
        let plan: serde_json::Value = if status_filter {
            sqlx::query_scalar(&explain)
                .bind(7_i32)
                .bind(Option::<i16>::None)
                .bind(pattern)
                .bind(0_i64)
                .bind(100_i64)
                .bind(100_i64)
                .fetch_one(&pool)
                .await
                .unwrap()
        } else {
            sqlx::query_scalar(&explain)
                .bind(7_i32)
                .bind(false)
                .bind(pattern)
                .bind(0_i64)
                .bind(100_i64)
                .bind(100_i64)
                .fetch_one(&pool)
                .await
                .unwrap()
        };
        assert!(
            plan.to_string().contains(expected_index),
            "monitor search plan did not use {expected_index}: {plan}"
        );
    }

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .expect("drop isolated schema");
    admin.close().await;
}
