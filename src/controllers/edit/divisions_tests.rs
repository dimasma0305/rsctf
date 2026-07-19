use super::*;

use std::str::FromStr;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

fn config(challenge_id: i32, permissions: Option<i32>) -> DivisionChallengeConfigInput {
    DivisionChallengeConfigInput {
        challenge_id,
        permissions,
    }
}

#[test]
fn scoring_boundary_rejects_only_real_division_policy_changes() {
    let current = std::collections::BTreeMap::from([(10, 7), (20, GamePermission::ALL)]);
    let same_reordered = [config(20, None), config(10, Some(7))];
    let same_with_duplicate = [config(10, Some(2)), config(10, Some(7)), config(20, None)];

    assert!(ensure_scored_division_policy_unchanged(true, 9, &current, None, None).is_ok());
    assert!(ensure_scored_division_policy_unchanged(true, 9, &current, Some(9), None).is_ok());
    assert!(ensure_scored_division_policy_unchanged(
        true,
        9,
        &current,
        None,
        Some(&same_reordered),
    )
    .is_ok());
    assert!(ensure_scored_division_policy_unchanged(
        true,
        9,
        &current,
        None,
        Some(&same_with_duplicate),
    )
    .is_ok());

    assert!(ensure_scored_division_policy_unchanged(true, 9, &current, Some(8), None).is_err());
    assert!(ensure_scored_division_policy_unchanged(
        true,
        9,
        &current,
        None,
        Some(&[config(10, Some(8)), config(20, None)]),
    )
    .is_err());
    assert!(ensure_scored_division_policy_unchanged(
        true,
        9,
        &current,
        None,
        Some(&[config(10, Some(7))]),
    )
    .is_err());
    assert!(
        ensure_scored_division_policy_unchanged(false, 9, &current, Some(8), Some(&[]),).is_ok()
    );
}

struct DivisionPolicyFixture {
    admin_pool: sqlx::PgPool,
    pool: sqlx::PgPool,
    schema: String,
    game_id: i32,
    division_id: i32,
}

impl DivisionPolicyFixture {
    async fn create() -> Self {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("connect test database");
        let schema = format!("rsctf_division_policy_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin_pool)
            .await
            .expect("create isolated test schema");
        let options = PgConnectOptions::from_str(&database_url)
            .expect("parse test database URL")
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .expect("connect isolated test pool");
        sqlx::raw_sql(
            r#"
            CREATE TABLE "Games" (
              id INTEGER PRIMARY KEY,
              ad_scoring_start_round INTEGER,
              koth_scoring_start_round INTEGER
            );
            CREATE TABLE "Divisions" (
              id INTEGER PRIMARY KEY,
              game_id INTEGER NOT NULL REFERENCES "Games"(id),
              default_permissions INTEGER NOT NULL
            );
            CREATE TABLE "DivisionChallengeConfigs" (
              division_id INTEGER NOT NULL REFERENCES "Divisions"(id),
              challenge_id INTEGER NOT NULL,
              permissions INTEGER NOT NULL,
              PRIMARY KEY (division_id, challenge_id)
            );
            "#,
        )
        .execute(&pool)
        .await
        .expect("create policy fixture tables");
        let seed = (uuid::Uuid::new_v4().as_u128() % 100_000_000) as i32 + 1_000;
        let game_id = seed;
        let division_id = seed + 1;
        sqlx::query(r#"INSERT INTO "Games" VALUES ($1, NULL, NULL)"#)
            .bind(game_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "Divisions" VALUES ($1, $2, 7)"#)
            .bind(division_id)
            .bind(game_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "DivisionChallengeConfigs" VALUES ($1, 10, 7)"#)
            .bind(division_id)
            .execute(&pool)
            .await
            .unwrap();
        Self {
            admin_pool,
            pool,
            schema,
            game_id,
            division_id,
        }
    }

    async fn destroy(self) {
        self.pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{}" CASCADE"#, self.schema))
            .execute(&self.admin_pool)
            .await
            .expect("drop isolated test schema");
    }
}

/// The first official round and a division edit can arrive on different
/// replicas. Whichever owns the shared PostgreSQL fence first must determine
/// the outcome; a stale editor may never write after the boundary commits.
#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn scoring_boundary_wins_against_a_concurrent_policy_edit() {
    let fixture = DivisionPolicyFixture::create().await;
    let key = crate::services::ad_engine::game_lock_key(fixture.game_id);
    let mut boundary = crate::utils::single_flight::PgAdvisoryLock::acquire(&fixture.pool, &key)
        .await
        .unwrap();
    let (attempting_tx, attempting_rx) = tokio::sync::oneshot::channel();
    let editor_pool = fixture.pool.clone();
    let editor_key = key.clone();
    let game_id = fixture.game_id;
    let division_id = fixture.division_id;
    let mut editor = tokio::spawn(async move {
        attempting_tx.send(()).unwrap();
        let mut lock =
            crate::utils::single_flight::PgAdvisoryLock::acquire(&editor_pool, &editor_key)
                .await
                .unwrap();
        let result = guard_division_policy_update(
            lock.transaction_mut(),
            game_id,
            division_id,
            Some(8),
            None,
        )
        .await;
        lock.release().await.unwrap();
        result
    });
    attempting_rx.await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut editor)
            .await
            .is_err(),
        "an editor on another replica crossed the scoring boundary fence"
    );
    sqlx::query(r#"UPDATE "Games" SET koth_scoring_start_round = 1 WHERE id = $1"#)
        .bind(fixture.game_id)
        .execute(&mut **boundary.transaction_mut())
        .await
        .unwrap();
    boundary.release().await.unwrap();

    let error = tokio::time::timeout(Duration::from_secs(2), editor)
        .await
        .expect("editor remained blocked after boundary commit")
        .expect("editor task failed")
        .expect_err("stale policy edit succeeded after KotH scoring started");
    assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
    let permissions: i32 =
        sqlx::query_scalar(r#"SELECT default_permissions FROM "Divisions" WHERE id = $1"#)
            .bind(fixture.division_id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(permissions, 7);
    fixture.destroy().await;
}

/// An edit that already owns the fence is allowed to commit while the game is
/// still a mutable template; the official boundary must wait and then start
/// from that fully committed policy rather than observing a partial update.
#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn pre_scoring_policy_edit_wins_before_a_concurrent_boundary() {
    let fixture = DivisionPolicyFixture::create().await;
    let key = crate::services::ad_engine::game_lock_key(fixture.game_id);
    let mut editor = crate::utils::single_flight::PgAdvisoryLock::acquire(&fixture.pool, &key)
        .await
        .unwrap();
    guard_division_policy_update(
        editor.transaction_mut(),
        fixture.game_id,
        fixture.division_id,
        Some(8),
        None,
    )
    .await
    .expect("pre-scoring policy update rejected");
    sqlx::query(r#"UPDATE "Divisions" SET default_permissions = 8 WHERE id = $1"#)
        .bind(fixture.division_id)
        .execute(&mut **editor.transaction_mut())
        .await
        .unwrap();

    let (attempting_tx, attempting_rx) = tokio::sync::oneshot::channel();
    let boundary_pool = fixture.pool.clone();
    let boundary_key = key.clone();
    let game_id = fixture.game_id;
    let mut boundary = tokio::spawn(async move {
        attempting_tx.send(()).unwrap();
        let mut lock =
            crate::utils::single_flight::PgAdvisoryLock::acquire(&boundary_pool, &boundary_key)
                .await
                .unwrap();
        sqlx::query(r#"UPDATE "Games" SET ad_scoring_start_round = 1 WHERE id = $1"#)
            .bind(game_id)
            .execute(&mut **lock.transaction_mut())
            .await
            .unwrap();
        lock.release().await.unwrap();
    });
    attempting_rx.await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut boundary)
            .await
            .is_err(),
        "the scoring boundary crossed an in-flight policy edit"
    );
    editor.release().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), boundary)
        .await
        .expect("boundary remained blocked after policy commit")
        .expect("boundary task failed");

    let state: (i32, Option<i32>) = sqlx::query_as(
        r#"SELECT division.default_permissions, game.ad_scoring_start_round
             FROM "Divisions" division
             JOIN "Games" game ON game.id = division.game_id
            WHERE division.id = $1"#,
    )
    .bind(fixture.division_id)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(state, (8, Some(1)));
    fixture.destroy().await;
}
