use super::*;

use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use uuid::Uuid;

async fn remove_if_allowed(pool: &sqlx::PgPool, team_id: i32, user_id: Uuid) -> AppResult<()> {
    let mut roster = super::super::acquire_roster_mutation(pool, team_id).await?;
    ensure_roster_change_allowed(roster.transaction_mut(), team_id).await?;
    sqlx::query(r#"DELETE FROM "TeamMembers" WHERE team_id = $1 AND user_id = $2"#)
        .bind(team_id)
        .bind(user_id)
        .execute(&mut **roster.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    roster.release().await
}

async fn membership_exists(pool: &sqlx::PgPool, team_id: i32, user_id: Uuid) -> bool {
    sqlx::query_scalar(
        r#"SELECT EXISTS(
             SELECT 1 FROM "TeamMembers" WHERE team_id = $1 AND user_id = $2
           )"#,
    )
    .bind(team_id)
    .bind(user_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn public_roster_removal_obeys_scoring_and_active_lock_fences() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect test database");
    let schema = format!("rsctf_roster_policy_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin_pool)
        .await
        .expect("create isolated test schema");
    let options = PgConnectOptions::from_str(&database_url)
        .expect("parse test database URL")
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect_with(options)
        .await
        .expect("connect isolated test pool");
    sqlx::raw_sql(
        r#"
        CREATE TABLE "Teams" (
          id INTEGER PRIMARY KEY,
          locked BOOLEAN NOT NULL
        );
        CREATE TABLE "Games" (
          id INTEGER PRIMARY KEY,
          end_time_utc TIMESTAMPTZ NOT NULL,
          ad_scoring_start_round INTEGER,
          koth_scoring_start_round INTEGER
        );
        CREATE TABLE "Participations" (
          game_id INTEGER NOT NULL,
          team_id INTEGER NOT NULL,
          status SMALLINT NOT NULL
        );
        CREATE TABLE "AdRounds" (
          game_id INTEGER NOT NULL,
          finalized BOOLEAN NOT NULL
        );
        CREATE TABLE "TeamMembers" (
          team_id INTEGER NOT NULL,
          user_id UUID NOT NULL,
          PRIMARY KEY (team_id, user_id)
        );
        INSERT INTO "Teams" (id, locked) VALUES
          (10, FALSE), (11, FALSE), (12, TRUE), (13, FALSE),
          (14, FALSE), (15, FALSE), (16, FALSE), (17, FALSE);
        INSERT INTO "Games"
          (id, end_time_utc, ad_scoring_start_round, koth_scoring_start_round)
        VALUES
          (20, clock_timestamp() + interval '1 hour', 1, NULL),
          (21, clock_timestamp() + interval '1 hour', NULL, 1),
          (22, clock_timestamp() + interval '1 hour', NULL, NULL),
          (23, clock_timestamp() - interval '1 second', 1, NULL),
          (24, clock_timestamp() + interval '1 hour', 1, NULL),
          (25, clock_timestamp() - interval '1 second', 1, NULL);
        INSERT INTO "Participations" (game_id, team_id, status) VALUES
          (20, 10, 1), (21, 11, 1), (22, 12, 1), (23, 13, 1),
          (24, 14, 0), (24, 15, 2), (25, 16, 1), (24, 17, 3);
        INSERT INTO "AdRounds" (game_id, finalized) VALUES
          (23, TRUE), (25, FALSE);
        "#,
    )
    .execute(&pool)
    .await
    .expect("create roster policy fixture");

    let members = std::array::from_fn::<_, 8, _>(|_| Uuid::new_v4());
    for (team_id, user_id) in (10..=17).zip(members) {
        sqlx::query(r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES ($1, $2)"#)
            .bind(team_id)
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();
    }

    for (team_id, user_id) in [(10, members[0]), (11, members[1])] {
        let error = remove_if_allowed(&pool, team_id, user_id)
            .await
            .expect_err("official scoring allowed an unlocked roster to shrink");
        assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(
            error.to_string(),
            "Team membership cannot change after A&D/KotH epoch scoring has started"
        );
        assert!(membership_exists(&pool, team_id, user_id).await);
    }

    let suspended_error = remove_if_allowed(&pool, 17, members[7])
        .await
        .expect_err("suspension allowed an official scoring roster to change");
    assert_eq!(
        suspended_error.to_string(),
        "Team membership cannot change after A&D/KotH epoch scoring has started"
    );
    assert!(membership_exists(&pool, 17, members[7]).await);

    let locked_error = remove_if_allowed(&pool, 12, members[2])
        .await
        .expect_err("an active locked team allowed roster removal");
    assert_eq!(locked_error.status(), axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(locked_error.to_string(), "Team is locked by an active game");
    assert!(membership_exists(&pool, 12, members[2]).await);

    remove_if_allowed(&pool, 13, members[3])
        .await
        .expect("an ended game kept the historical roster frozen");
    assert!(!membership_exists(&pool, 13, members[3]).await);

    for (team_id, user_id) in [(14, members[4]), (15, members[5])] {
        remove_if_allowed(&pool, team_id, user_id)
            .await
            .expect("a non-accepted participation froze an unlocked roster");
        assert!(!membership_exists(&pool, team_id, user_id).await);
    }

    let error = remove_if_allowed(&pool, 16, members[6])
        .await
        .expect_err("ended game allowed roster mutation before closeout completed");
    assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(
        error.to_string(),
        "Team membership cannot change after A&D/KotH epoch scoring has started"
    );
    assert!(membership_exists(&pool, 16, members[6]).await);

    sqlx::query(r#"UPDATE "AdRounds" SET finalized = TRUE WHERE game_id = 25"#)
        .execute(&pool)
        .await
        .unwrap();
    remove_if_allowed(&pool, 16, members[6])
        .await
        .expect("a durably finalized ended game kept its roster frozen");
    assert!(!membership_exists(&pool, 16, members[6]).await);

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin_pool)
        .await
        .expect("drop isolated test schema");
}
