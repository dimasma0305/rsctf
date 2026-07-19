use std::str::FromStr;

use chrono::{Duration, Utc};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use uuid::Uuid;

use super::*;

async fn test_pool() -> (sqlx::PgPool, sqlx::PgPool, String) {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("rsctf_byoc_fence_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"
        CREATE TABLE "AspNetUsers" (id UUID PRIMARY KEY, role SMALLINT NOT NULL);
        CREATE TABLE "Teams" (
          id INTEGER PRIMARY KEY, captain_id UUID NOT NULL,
          invite_token TEXT NOT NULL, deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
        );
        CREATE TABLE "TeamMembers" (team_id INTEGER NOT NULL, user_id UUID NOT NULL);
        CREATE TABLE "Participations" (
          id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
          team_id INTEGER NOT NULL, status SMALLINT NOT NULL
        );
        CREATE TABLE "Games" (
          id INTEGER PRIMARY KEY, private_key TEXT NOT NULL,
          start_time_utc TIMESTAMPTZ NOT NULL, end_time_utc TIMESTAMPTZ NOT NULL
        );
        CREATE TABLE "GameChallenges" (
          id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, title TEXT NOT NULL,
          container_image TEXT, build_status SMALLINT NOT NULL,
          build_image_digest TEXT, expose_port INTEGER, "Type" SMALLINT NOT NULL,
          ad_self_hosted BOOLEAN NOT NULL, is_enabled BOOLEAN NOT NULL,
          review_status SMALLINT NOT NULL
        );
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let captain = Uuid::new_v4();
    sqlx::query(r#"INSERT INTO "AspNetUsers" VALUES ($1, 1)"#)
        .bind(captain)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "Teams" VALUES (7, $1, 'team-secret', FALSE)"#)
        .bind(captain)
        .execute(&pool)
        .await
        .unwrap();
    let start = Utc::now() - Duration::hours(1);
    let end = Utc::now() + Duration::hours(1);
    sqlx::query(r#"INSERT INTO "Games" VALUES (3, 'game-secret', $1, $2)"#)
        .bind(start)
        .bind(end)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "Participations" VALUES (11, 3, 7, 1)"#)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "GameChallenges"
           VALUES (13, 3, 'service', 'service:latest', 1,
                   'sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                   31337, 4, TRUE, TRUE, 0)"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    (admin, pool, schema)
}

async fn cleanup(admin: sqlx::PgPool, pool: sqlx::PgPool, schema: String) {
    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}

fn token(team_secret: &str) -> String {
    super::super::byoc::byoc_token("adbyocimage:", "game-secret", team_secret, 11, 13)
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn roster_revocation_waits_until_capability_fence_releases() {
    let (admin, pool, schema) = test_pool().await;
    let old_token = token("team-secret");
    let authorization = authorize_byoc_capability(&pool, 3, 11, 13, "adbyocimage:", &old_token)
        .await
        .unwrap()
        .expect("live capability");

    let mut revocation = tokio::spawn({
        let pool = pool.clone();
        async move {
            let mut roster =
                crate::utils::single_flight::PgAdvisoryLock::acquire(&pool, "team-roster:7")
                    .await
                    .unwrap();
            sqlx::query(r#"UPDATE "Teams" SET invite_token = 'rotated' WHERE id = 7"#)
                .execute(&mut **roster.transaction_mut())
                .await
                .unwrap();
            roster.release().await.unwrap();
        }
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(75), &mut revocation)
            .await
            .is_err(),
        "revocation returned while an old capability fence was live"
    );
    authorization.release().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), revocation)
        .await
        .expect("revocation remained blocked")
        .expect("revocation task failed");
    assert!(
        authorize_byoc_capability(&pool, 3, 11, 13, "adbyocimage:", &old_token)
            .await
            .unwrap()
            .is_none(),
        "the pre-rotation capability became valid again"
    );
    cleanup(admin, pool, schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn challenge_revocation_waits_for_the_atomic_admission_snapshot() {
    let (admin, pool, schema) = test_pool().await;
    let old_token = token("team-secret");
    let authorization = authorize_byoc_capability(&pool, 3, 11, 13, "adbyocimage:", &old_token)
        .await
        .unwrap()
        .expect("live capability");

    let mut revocation = tokio::spawn({
        let pool = pool.clone();
        async move {
            sqlx::query(r#"UPDATE "GameChallenges" SET is_enabled = FALSE WHERE id = 13"#)
                .execute(&pool)
                .await
                .unwrap();
        }
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(75), &mut revocation)
            .await
            .is_err(),
        "challenge revocation passed a live admission row fence"
    );
    authorization.release().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), revocation)
        .await
        .expect("challenge revocation remained blocked")
        .expect("challenge revocation task failed");
    assert!(
        authorize_byoc_capability(&pool, 3, 11, 13, "adbyocimage:", &old_token)
            .await
            .unwrap()
            .is_none(),
        "a disabled challenge retained its old image grant"
    );
    cleanup(admin, pool, schema).await;
}
