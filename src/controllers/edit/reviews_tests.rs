use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use super::reviews::{insert_game_manager_if_absent, INSERT_GAME_MANAGER_SQL};

#[test]
fn manager_grant_is_one_atomic_conflict_safe_insert() {
    assert!(INSERT_GAME_MANAGER_SQL.contains("INSERT INTO \"GameManagers\""));
    assert!(INSERT_GAME_MANAGER_SQL.contains("ON CONFLICT (game_id, user_id) DO NOTHING"));
    assert!(!INSERT_GAME_MANAGER_SQL.contains("SELECT"));
}

struct Fixture {
    admin: sqlx::PgPool,
    pool: sqlx::PgPool,
    schema: String,
}

impl Fixture {
    async fn create() -> Self {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("game_manager_grant_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"CREATE TABLE "Games" (id INTEGER PRIMARY KEY);
               CREATE TABLE "AspNetUsers" (id UUID PRIMARY KEY);
               CREATE TABLE "GameManagers" (
                 id SERIAL PRIMARY KEY,
                 game_id INTEGER NOT NULL,
                 user_id UUID NOT NULL,
                 CONSTRAINT fk_gamemanagers_game
                   FOREIGN KEY (game_id) REFERENCES "Games" (id) ON DELETE CASCADE,
                 CONSTRAINT fk_gamemanagers_user
                   FOREIGN KEY (user_id) REFERENCES "AspNetUsers" (id) ON DELETE CASCADE
               );
               CREATE UNIQUE INDEX ux_gamemanagers_game_user
                 ON "GameManagers" (game_id, user_id);"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        Self {
            admin,
            pool,
            schema,
        }
    }

    async fn seed_parents(&self, game_id: i32, user_id: uuid::Uuid) {
        sqlx::query(r#"INSERT INTO "Games" (id) VALUES ($1)"#)
            .bind(game_id)
            .execute(&self.pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "AspNetUsers" (id) VALUES ($1)"#)
            .bind(user_id)
            .execute(&self.pool)
            .await
            .unwrap();
    }

    async fn count(&self, game_id: i32, user_id: uuid::Uuid) -> i64 {
        sqlx::query_scalar(
            r#"SELECT count(*)::bigint FROM "GameManagers"
                WHERE game_id = $1 AND user_id = $2"#,
        )
        .bind(game_id)
        .bind(user_id)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn cleanup(self) {
        self.pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{}" CASCADE"#, self.schema))
            .execute(&self.admin)
            .await
            .unwrap();
        self.admin.close().await;
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn repeated_manager_grants_are_idempotent() {
    let fixture = Fixture::create().await;
    let user_id = uuid::Uuid::new_v4();
    fixture.seed_parents(7, user_id).await;

    assert!(insert_game_manager_if_absent(&fixture.pool, 7, user_id)
        .await
        .unwrap());
    assert!(!insert_game_manager_if_absent(&fixture.pool, 7, user_id)
        .await
        .unwrap());
    assert_eq!(fixture.count(7, user_id).await, 1);

    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn concurrent_manager_grants_create_exactly_one_membership() {
    let fixture = Fixture::create().await;
    let user_id = uuid::Uuid::new_v4();
    fixture.seed_parents(7, user_id).await;
    let left_pool = fixture.pool.clone();
    let right_pool = fixture.pool.clone();

    let (left, right) = tokio::join!(
        insert_game_manager_if_absent(&left_pool, 7, user_id),
        insert_game_manager_if_absent(&right_pool, 7, user_id),
    );
    assert_eq!(usize::from(left.unwrap()) + usize::from(right.unwrap()), 1);
    assert_eq!(fixture.count(7, user_id).await, 1);

    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn manager_grant_racing_game_delete_cascades_or_returns_not_found() {
    let fixture = Fixture::create().await;
    let user_id = uuid::Uuid::new_v4();
    fixture.seed_parents(7, user_id).await;

    // Hold an uncommitted grant. Its FK key-share lock makes the concurrent
    // parent delete wait; once the grant commits, the same delete cascades it.
    let mut grant = fixture.pool.begin().await.unwrap();
    sqlx::query(INSERT_GAME_MANAGER_SQL)
        .bind(7_i32)
        .bind(user_id)
        .execute(&mut *grant)
        .await
        .unwrap();
    let delete_pool = fixture.pool.clone();
    let mut delete = tokio::spawn(async move {
        sqlx::query(r#"DELETE FROM "Games" WHERE id = 7"#)
            .execute(&delete_pool)
            .await
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut delete)
            .await
            .is_err(),
        "parent deletion must wait for the in-flight FK insert"
    );
    grant.commit().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), &mut delete)
        .await
        .expect("parent delete completes after grant commit")
        .unwrap()
        .unwrap();
    assert_eq!(fixture.count(7, user_id).await, 0);

    let error = insert_game_manager_if_absent(&fixture.pool, 7, user_id)
        .await
        .unwrap_err();
    assert!(matches!(error, crate::utils::error::AppError::NotFound(_)));

    fixture.cleanup().await;
}
