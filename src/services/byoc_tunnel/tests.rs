use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use super::{load_team_participation_page, AuthorizationGenerations, TEAM_DISCONNECT_PAGE_SIZE};

#[test]
fn team_generation_fences_only_the_rotated_team_without_per_participation_entries() {
    let mut generations = AuthorizationGenerations::default();
    let original = generations.current(7, 70, 700);
    let generation = generations.teams.entry(7).or_default();
    *generation = generation.saturating_add(1);

    assert_ne!(generations.current(7, 70, 700), original);
    assert_eq!(generations.current(8, 70, 700).team, 0);
    assert!(generations.participations.is_empty());
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn historical_team_participations_are_read_in_strictly_bounded_pages() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect test database");
    let schema = format!("rsctf_byoc_team_page_{}", uuid::Uuid::new_v4().simple());
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
        r#"CREATE TABLE "Participations" (
               id INTEGER PRIMARY KEY,
               team_id INTEGER NOT NULL
           );
           INSERT INTO "Participations" (id, team_id)
           SELECT value, 7 FROM generate_series(1, 600) value;
           INSERT INTO "Participations" (id, team_id) VALUES (601, 8);"#,
    )
    .execute(&pool)
    .await
    .expect("seed participation history");

    let first = load_team_participation_page(&pool, 7, 0)
        .await
        .expect("load first page");
    let second = load_team_participation_page(&pool, 7, *first.last().unwrap())
        .await
        .expect("load second page");
    let third = load_team_participation_page(&pool, 7, *second.last().unwrap())
        .await
        .expect("load final page");
    let exhausted = load_team_participation_page(&pool, 7, *third.last().unwrap())
        .await
        .expect("observe exhausted history");

    assert_eq!(first.len(), TEAM_DISCONNECT_PAGE_SIZE as usize);
    assert_eq!(second.len(), TEAM_DISCONNECT_PAGE_SIZE as usize);
    assert_eq!(third.len(), 88);
    assert!(exhausted.is_empty());
    assert_eq!(first[0], 1);
    assert_eq!(third.last(), Some(&600));

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .expect("drop isolated schema");
    admin.close().await;
}
