use std::str::FromStr;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use super::seed_division_configs;

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn committed_default_revoke_wins_over_a_waiting_seed() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("division_seed_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"CREATE TABLE "Divisions" (
             id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
             default_permissions INTEGER NOT NULL
           );
           CREATE TABLE "DivisionChallengeConfigs" (
             division_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
             permissions INTEGER NOT NULL,
             PRIMARY KEY (division_id, challenge_id)
           );
           INSERT INTO "Divisions" VALUES (1, 9, 7);"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let mut revoke = pool.begin().await.unwrap();
    sqlx::query(r#"UPDATE "Divisions" SET default_permissions = 0 WHERE id = 1"#)
        .execute(&mut *revoke)
        .await
        .unwrap();

    let seed_pool = pool.clone();
    let mut seed = tokio::spawn(async move {
        let mut transaction = seed_pool.begin().await.unwrap();
        seed_division_configs(&mut transaction, 9, 4).await.unwrap();
        transaction.commit().await.unwrap();
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut seed)
            .await
            .is_err(),
        "seed did not wait for the parent division policy writer"
    );
    revoke.commit().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), seed)
        .await
        .expect("seed remained blocked after the revoke committed")
        .unwrap();

    let permissions: i32 = sqlx::query_scalar(
        r#"SELECT permissions FROM "DivisionChallengeConfigs"
            WHERE division_id = 1 AND challenge_id = 4"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(permissions, 0, "seed restored the stale VIEW permission");

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
