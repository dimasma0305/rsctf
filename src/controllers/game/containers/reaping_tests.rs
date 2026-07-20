use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use super::reaping::{clear_destroyed_managed_container, resolve_managed_container_owner};

struct Harness {
    admin: sqlx::PgPool,
    pool: sqlx::PgPool,
    schema: String,
    stale: uuid::Uuid,
    replacement: uuid::Uuid,
}

impl Harness {
    async fn new() -> Self {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("rsctf_test_reaping_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "Containers" (id UUID PRIMARY KEY, container_id TEXT NOT NULL);
            CREATE TABLE "GameChallenges" (
                id INTEGER PRIMARY KEY,
                game_id INTEGER NOT NULL,
                shared_container_id UUID,
                test_container_id UUID
            );
            CREATE TABLE "KothTargets" (
                challenge_id INTEGER NOT NULL,
                container_id TEXT,
                host TEXT NOT NULL DEFAULT '',
                port INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE "GameInstances" (
                id INTEGER PRIMARY KEY,
                participation_id INTEGER NOT NULL,
                container_id UUID,
                is_loaded BOOLEAN NOT NULL,
                last_container_operation TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
            );
            CREATE TABLE "ExerciseInstances" (
                id INTEGER PRIMARY KEY,
                exercise_id INTEGER NOT NULL,
                user_id UUID NOT NULL,
                container_id UUID,
                is_loaded BOOLEAN NOT NULL,
                last_container_operation TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
            );
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let stale = uuid::Uuid::new_v4();
        let replacement = uuid::Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO "Containers" (id, container_id)
               VALUES ($1, 'runtime-stale'), ($2, 'runtime-replacement')"#,
        )
        .bind(stale)
        .bind(replacement)
        .execute(&pool)
        .await
        .unwrap();
        Self {
            admin,
            pool,
            schema,
            stale,
            replacement,
        }
    }

    async fn cleanup(self) {
        self.pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{}" CASCADE"#, self.schema))
            .execute(&self.admin)
            .await
            .unwrap();
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn stale_game_forward_id_never_detaches_the_replacement() {
    let harness = Harness::new().await;
    sqlx::query(
        r#"INSERT INTO "GameInstances"
              (id, participation_id, container_id, is_loaded)
            VALUES (11, 23, $1, TRUE)"#,
    )
    .bind(harness.replacement)
    .execute(&harness.pool)
    .await
    .unwrap();

    let owner = resolve_managed_container_owner(
        &harness.pool,
        harness.stale,
        "runtime-stale",
        Some(11),
        None,
    )
    .await
    .unwrap();
    assert!(owner.is_none());

    clear_destroyed_managed_container(
        &harness.pool,
        harness.stale,
        "runtime-stale",
        Some(11),
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let instance = sqlx::query_as::<_, (Option<uuid::Uuid>, bool)>(
        r#"SELECT container_id, is_loaded FROM "GameInstances" WHERE id = 11"#,
    )
    .fetch_one(&harness.pool)
    .await
    .unwrap();
    assert_eq!(instance, (Some(harness.replacement), true));
    let stale_exists: bool =
        sqlx::query_scalar(r#"SELECT EXISTS (SELECT 1 FROM "Containers" WHERE id = $1)"#)
            .bind(harness.stale)
            .fetch_one(&harness.pool)
            .await
            .unwrap();
    assert!(!stale_exists);
    harness.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn exercise_owner_uses_the_established_lock_and_clears_exactly() {
    let harness = Harness::new().await;
    let user_id = uuid::Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "ExerciseInstances"
              (id, exercise_id, user_id, container_id, is_loaded)
            VALUES (17, 31, $1, $2, TRUE)"#,
    )
    .bind(user_id)
    .bind(harness.stale)
    .execute(&harness.pool)
    .await
    .unwrap();

    let owner = resolve_managed_container_owner(
        &harness.pool,
        harness.stale,
        "runtime-stale",
        None,
        Some(17),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(owner.lock_key, format!("exercise-container:{user_id}:31"));
    assert_eq!(owner.exercise_instance_id, Some(17));

    clear_destroyed_managed_container(
        &harness.pool,
        harness.stale,
        "runtime-stale",
        None,
        owner.exercise_instance_id,
        None,
        None,
    )
    .await
    .unwrap();
    let instance = sqlx::query_as::<_, (Option<uuid::Uuid>, bool)>(
        r#"SELECT container_id, is_loaded FROM "ExerciseInstances" WHERE id = 17"#,
    )
    .fetch_one(&harness.pool)
    .await
    .unwrap();
    assert_eq!(instance, (None, false));
    harness.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn exercise_cleanup_does_not_detach_a_new_replacement() {
    let harness = Harness::new().await;
    let user_id = uuid::Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "ExerciseInstances"
              (id, exercise_id, user_id, container_id, is_loaded)
            VALUES (19, 37, $1, $2, TRUE)"#,
    )
    .bind(user_id)
    .bind(harness.replacement)
    .execute(&harness.pool)
    .await
    .unwrap();

    clear_destroyed_managed_container(
        &harness.pool,
        harness.stale,
        "runtime-stale",
        None,
        Some(19),
        None,
        None,
    )
    .await
    .unwrap();
    let instance = sqlx::query_as::<_, (Option<uuid::Uuid>, bool)>(
        r#"SELECT container_id, is_loaded FROM "ExerciseInstances" WHERE id = 19"#,
    )
    .fetch_one(&harness.pool)
    .await
    .unwrap();
    assert_eq!(instance, (Some(harness.replacement), true));
    harness.cleanup().await;
}
