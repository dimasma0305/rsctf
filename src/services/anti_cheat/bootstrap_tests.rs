use std::str::FromStr;

use chrono::{Duration, Utc};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use uuid::Uuid;

use super::*;

async fn pool() -> sqlx::PgPool {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("identity_bootstrap_{}", Uuid::new_v4().simple());
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
        CREATE TABLE "AspNetUsers" (
            id UUID PRIMARY KEY, user_name TEXT, ip TEXT NOT NULL,
            browser_fingerprint TEXT, last_signed_in_utc TIMESTAMPTZ NOT NULL,
            register_time_utc TIMESTAMPTZ NOT NULL,
            email_confirmed BOOLEAN NOT NULL, role SMALLINT NOT NULL
        );
        CREATE TABLE "IdentityObservations" (
            id BIGSERIAL PRIMARY KEY, user_id UUID NOT NULL, team_id INTEGER,
            game_id INTEGER, participation_id INTEGER, kind TEXT NOT NULL,
            value_hash BYTEA NOT NULL, subnet_group_hash BYTEA,
            broad_network_hash BYTEA, value_hint TEXT NOT NULL,
            source TEXT NOT NULL, observed_at_utc TIMESTAMPTZ NOT NULL
        );
        CREATE TABLE "IdentityObservationBootstrapState" (
            version SMALLINT PRIMARY KEY, key_identifier BYTEA NOT NULL,
            completed_at_utc TIMESTAMPTZ NOT NULL,
            observations_inserted BIGINT NOT NULL
        );
        CREATE TABLE "AntiCheatBlocks" (
            id SERIAL PRIMARY KEY, kind TEXT NOT NULL, conflicting_value TEXT,
            conflicting_value_hash BYTEA
        );
        CREATE TABLE "Logs" (
            id BIGSERIAL PRIMARY KEY, time_utc TIMESTAMPTZ NOT NULL,
            logger TEXT NOT NULL, remote_ip TEXT, user_name TEXT,
            message TEXT NOT NULL, status TEXT, browser_fingerprint TEXT
        );
        CREATE TABLE "TeamMembers" (
            team_id INTEGER NOT NULL, user_id UUID NOT NULL,
            PRIMARY KEY (team_id, user_id)
        );
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    pool
}

fn config(key: &str) -> AppConfig {
    let mut config = AppConfig::from_env();
    config.identity_hash_key = key.to_string();
    config
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn bootstrap_uses_only_matching_success_audits_and_is_key_bound() {
    let pool = pool().await;
    let accepted = Uuid::new_v4();
    let poisoned = Uuid::new_v4();
    let now = Utc::now();
    let signed_at = now - Duration::minutes(2);
    let fingerprint = "a".repeat(64);
    sqlx::query(
        r#"INSERT INTO "AspNetUsers"
             (id,user_name,ip,browser_fingerprint,last_signed_in_utc,
              register_time_utc,email_confirmed,role)
           VALUES
             ($1,'accepted','192.0.2.10',$3,$4,$5,TRUE,1),
             ($2,'poisoned','198.51.100.99',$3,$4,$5,TRUE,1)"#,
    )
    .bind(accepted)
    .bind(poisoned)
    .bind(&fingerprint)
    .bind(signed_at)
    .bind(signed_at - Duration::days(1))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "Logs"
             (time_utc,logger,remote_ip,user_name,message,status,browser_fingerprint)
           VALUES
             ($1,'AccountController','192.0.2.10','accepted','login','Success',$2),
             ($1,'AccountController','198.51.100.20','poisoned','login','Success',NULL)"#,
    )
    .bind(signed_at + Duration::seconds(1))
    .bind(&fingerprint)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "AntiCheatBlocks" (kind,conflicting_value)
           VALUES ('Ip','203.0.113.9'), ('Fingerprint',$1)"#,
    )
    .bind(&fingerprint)
    .execute(&pool)
    .await
    .unwrap();

    let app_config = config("bootstrap-test-key-0123456789abcdef");
    assert_eq!(
        bootstrap_legacy_identity_observations(&pool, &app_config)
            .await
            .unwrap(),
        2
    );
    let accepted_kinds: Vec<String> = sqlx::query_scalar(
        r#"SELECT kind FROM "IdentityObservations"
            WHERE user_id=$1 AND game_id IS NULL ORDER BY kind"#,
    )
    .bind(accepted)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(accepted_kinds, vec!["Fingerprint", "Ip"]);
    let fabricated_contexts: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM "IdentityObservations"
            WHERE source='Legacy'
              AND (team_id IS NOT NULL OR game_id IS NOT NULL
                   OR participation_id IS NOT NULL)"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(fabricated_contexts, 0);
    let poisoned_count: i64 =
        sqlx::query_scalar(r#"SELECT COUNT(*) FROM "IdentityObservations" WHERE user_id=$1"#)
            .bind(poisoned)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(poisoned_count, 0);
    let raw_accounts: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM "AspNetUsers" WHERE browser_fingerprint IS NOT NULL"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let raw_logs: i64 =
        sqlx::query_scalar(r#"SELECT COUNT(*) FROM "Logs" WHERE browser_fingerprint IS NOT NULL"#)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((raw_accounts, raw_logs), (0, 0));
    let block_values: Vec<(String, Option<Vec<u8>>)> = sqlx::query_as(
        r#"SELECT conflicting_value, conflicting_value_hash
             FROM "AntiCheatBlocks" ORDER BY id"#,
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(block_values[0].0, "203.0.113.x");
    assert_ne!(block_values[1].0, fingerprint[..12]);
    assert!(block_values
        .iter()
        .all(|row| row.1.as_ref().is_some_and(|hash| hash.len() == 32)));
    assert_eq!(
        bootstrap_legacy_identity_observations(&pool, &app_config)
            .await
            .unwrap(),
        0
    );
    assert!(bootstrap_legacy_identity_observations(
        &pool,
        &config("different-bootstrap-key-0123456789")
    )
    .await
    .is_err());
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn bootstrap_drains_pre_marker_team_member_writer_before_completion() {
    let pool = pool().await;
    let user_id = Uuid::new_v4();
    let mut legacy_writer = pool.begin().await.unwrap();
    sqlx::query(r#"INSERT INTO "TeamMembers" (team_id,user_id) VALUES (10,$1)"#)
        .bind(user_id)
        .execute(&mut *legacy_writer)
        .await
        .unwrap();

    let bootstrap_pool = pool.clone();
    let app_config = config("bootstrap-lock-test-key-0123456789");
    let mut bootstrap = tokio::spawn(async move {
        bootstrap_legacy_identity_observations(&bootstrap_pool, &app_config).await
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(75), &mut bootstrap)
            .await
            .is_err()
    );
    let marker_before_commit: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM "IdentityObservationBootstrapState" WHERE version=1"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(marker_before_commit, 0);

    legacy_writer.commit().await.unwrap();
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(5), bootstrap)
            .await
            .expect("bootstrap remained blocked after the legacy writer committed")
            .unwrap()
            .unwrap(),
        0
    );
    let (members, marker): (i64, i64) = sqlx::query_as(
        r#"SELECT
             (SELECT COUNT(*) FROM "TeamMembers"),
             (SELECT COUNT(*) FROM "IdentityObservationBootstrapState" WHERE version=1)"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((members, marker), (1, 1));
}
