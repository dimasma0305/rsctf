use std::str::FromStr;

use chrono::{Duration, TimeZone, Utc};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::ConnectOptions;

use super::super::SolversQuery;
use super::{bounded_solver_page, load_challenge_solver_page};

#[test]
fn solver_pages_have_a_small_default_and_hard_bounds() {
    assert_eq!(
        bounded_solver_page(&SolversQuery::default()).unwrap(),
        (0, 20)
    );
    assert_eq!(
        bounded_solver_page(&SolversQuery {
            count: Some(50_000),
            skip: Some(10_000),
        })
        .unwrap(),
        (10_000, 100)
    );
    assert!(bounded_solver_page(&SolversQuery {
        count: Some(20),
        skip: Some(10_001),
    })
    .is_err());
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn maximum_roster_solver_page_is_sql_bounded_and_freeze_aware() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let options = PgConnectOptions::from_str(&database_url)
        .expect("invalid PostgreSQL URL")
        .application_name("rsctf:solver-page:test")
        .disable_statement_logging();
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();

    sqlx::raw_sql(
        r#"
        CREATE TEMP TABLE "Games" (
          id INTEGER PRIMARY KEY, practice_mode BOOLEAN NOT NULL,
          start_time_utc TIMESTAMPTZ NOT NULL, end_time_utc TIMESTAMPTZ NOT NULL
        );
        CREATE TEMP TABLE "Teams" (
          id INTEGER PRIMARY KEY, name TEXT NOT NULL, avatar_hash TEXT NULL
        );
        CREATE TEMP TABLE "AspNetUsers" (
          id UUID PRIMARY KEY, user_name TEXT NULL
        );
        CREATE TEMP TABLE "Divisions" (
          id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, default_permissions INTEGER NOT NULL
        );
        CREATE TEMP TABLE "GameChallenges" (
          id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, is_enabled BOOLEAN NOT NULL,
          review_status SMALLINT NOT NULL, deadline_utc TIMESTAMPTZ NULL
        );
        CREATE TEMP TABLE "DivisionChallengeConfigs" (
          division_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL, permissions INTEGER NOT NULL
        );
        CREATE TEMP TABLE "Participations" (
          id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, team_id INTEGER NOT NULL,
          status SMALLINT NOT NULL, division_id INTEGER NULL
        );
        CREATE TEMP TABLE "Submissions" (
          id INTEGER PRIMARY KEY, participation_id INTEGER NOT NULL,
          challenge_id INTEGER NOT NULL, game_id INTEGER NOT NULL, team_id INTEGER NOT NULL,
          user_id UUID NULL, status SMALLINT NOT NULL, submit_time_utc TIMESTAMPTZ NOT NULL
        );
        CREATE TEMP TABLE "FirstSolves" (
          participation_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
          submission_id INTEGER NOT NULL, PRIMARY KEY (participation_id, challenge_id)
        );
        CREATE INDEX ON "FirstSolves" (challenge_id, participation_id);
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let start = Utc.with_ymd_and_hms(2026, 8, 26, 0, 0, 0).unwrap();
    sqlx::query(r#"INSERT INTO "Games" VALUES (1, FALSE, $1, $2)"#)
        .bind(start)
        .bind(start + Duration::hours(1))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "GameChallenges" VALUES (9, 1, TRUE, 0, NULL)"#)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"
        INSERT INTO "Teams" (id, name)
          SELECT n, 'Team-' || lpad(n::text, 4, '0') FROM generate_series(1, 500) n;
        INSERT INTO "Participations" (id, game_id, team_id, status, division_id)
          SELECT n, 1, n, 1, NULL FROM generate_series(1, 500) n;
        INSERT INTO "Submissions"
          (id, participation_id, challenge_id, game_id, team_id, user_id, status, submit_time_utc)
          SELECT n, n, 9, 1, n, NULL, 1, game.start_time_utc + n * INTERVAL '1 second'
            FROM generate_series(1, 500) n CROSS JOIN "Games" game WHERE game.id = 1;
        INSERT INTO "FirstSolves" (participation_id, challenge_id, submission_id)
          SELECT n, 9, n FROM generate_series(1, 500) n;
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let last_page = load_challenge_solver_page(&pool, 1, 9, None, 480, 20)
        .await
        .unwrap();
    assert_eq!(last_page.len(), 20);
    assert!(last_page.iter().all(|row| row.total == 500));
    assert_eq!(last_page[0].team_name, "Team-0481");
    assert_eq!(last_page[19].team_name, "Team-0500");

    let frozen =
        load_challenge_solver_page(&pool, 1, 9, Some(start + Duration::seconds(251)), 240, 20)
            .await
            .unwrap();
    assert_eq!(frozen.len(), 10);
    assert!(frozen.iter().all(|row| row.total == 250));
    assert_eq!(frozen[0].team_name, "Team-0241");
    assert_eq!(frozen[9].team_name, "Team-0250");

    pool.close().await;
}
