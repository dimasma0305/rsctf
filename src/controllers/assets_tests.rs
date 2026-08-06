use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use uuid::Uuid;

use super::authorization::{
    participant_can_download_target, query_asset_gate, AssetGate, AssetTarget,
};

struct AssetAuthorizationHarness {
    admin: sqlx::PgPool,
    pool: sqlx::PgPool,
    schema: String,
}

impl AssetAuthorizationHarness {
    async fn new() -> Self {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("rsctf_asset_auth_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "Games" (
              id INTEGER PRIMARY KEY,
              hidden BOOLEAN NOT NULL,
              poster_hash TEXT
            );
            CREATE TABLE "GameChallenges" (
              id INTEGER PRIMARY KEY,
              game_id INTEGER NOT NULL,
              is_enabled BOOLEAN NOT NULL,
              review_status SMALLINT NOT NULL,
              attachment_id INTEGER
            );
            CREATE TABLE "Participations" (
              id INTEGER PRIMARY KEY,
              game_id INTEGER NOT NULL,
              team_id INTEGER NOT NULL,
              status SMALLINT NOT NULL,
              writeup_id INTEGER
            );
            CREATE TABLE "UserParticipations" (
              user_id UUID NOT NULL,
              game_id INTEGER NOT NULL,
              team_id INTEGER NOT NULL,
              participation_id INTEGER NOT NULL,
              PRIMARY KEY (user_id, game_id)
            );
            CREATE TABLE "AspNetUsers" (
              id UUID PRIMARY KEY,
              avatar_hash TEXT
            );
            CREATE TABLE "Teams" (
              id INTEGER PRIMARY KEY,
              avatar_hash TEXT
            );
            CREATE TABLE "Configs" (
              config_key TEXT PRIMARY KEY,
              value TEXT
            );
            CREATE TABLE "Files" (
              id INTEGER PRIMARY KEY,
              hash TEXT UNIQUE NOT NULL,
              file_size BIGINT NOT NULL,
              reference_count BIGINT NOT NULL
            );
            CREATE TABLE "Attachments" (
              id INTEGER PRIMARY KEY,
              local_file_id INTEGER
            );
            CREATE TABLE "FlagContexts" (
              id INTEGER PRIMARY KEY,
              attachment_id INTEGER
            );
            CREATE TABLE "GameInstances" (
              id INTEGER PRIMARY KEY,
              challenge_id INTEGER NOT NULL,
              participation_id INTEGER NOT NULL,
              flag_id INTEGER
            );
            "#,
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
async fn asset_gate_query_preserves_public_static_team_and_private_boundaries() {
    let harness = AssetAuthorizationHarness::new().await;
    let public_hash = "a".repeat(64);
    let static_hash = "b".repeat(64);
    let writeup_hash = "c".repeat(64);
    let orphan_hash = "d".repeat(64);

    sqlx::query(r#"INSERT INTO "AspNetUsers" (id, avatar_hash) VALUES ($1, $2)"#)
        .bind(Uuid::new_v4())
        .bind(&public_hash)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "Files" (id, hash, file_size, reference_count) VALUES
          (1, $1, 1048576, 1),
          (2, $2, 2048, 1),
          (3, $3, 4096, 1)"#,
    )
    .bind(&static_hash)
    .bind(&writeup_hash)
    .bind(&orphan_hash)
    .execute(&harness.pool)
    .await
    .unwrap();
    sqlx::raw_sql(
        r#"
        INSERT INTO "Attachments" (id, local_file_id) VALUES (10, 1);
        INSERT INTO "Games" (id, hidden) VALUES (20, TRUE), (21, TRUE);
        INSERT INTO "GameChallenges"
          (id, game_id, is_enabled, review_status, attachment_id)
        VALUES (30, 20, TRUE, 0, 10);
        INSERT INTO "Participations"
          (id, game_id, team_id, status, writeup_id)
        VALUES (40, 21, 50, 1, 2);
        "#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();

    assert_eq!(
        query_asset_gate(&harness.pool, &public_hash).await.unwrap(),
        AssetGate::Public { file_size: None }
    );
    assert_eq!(
        query_asset_gate(&harness.pool, &static_hash).await.unwrap(),
        AssetGate::Protected {
            file_size: Some(1_048_576),
            targets: vec![AssetTarget {
                game_id: 20,
                source_team: None,
                challenge_id: Some(30),
            }],
        }
    );
    assert_eq!(
        query_asset_gate(&harness.pool, &writeup_hash)
            .await
            .unwrap(),
        AssetGate::Protected {
            file_size: Some(2048),
            targets: vec![AssetTarget {
                game_id: 21,
                source_team: Some(50),
                challenge_id: None,
            }],
        }
    );
    assert_eq!(
        query_asset_gate(&harness.pool, &orphan_hash).await.unwrap(),
        AssetGate::Private {
            file_size: Some(4096),
        }
    );

    harness.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn accepted_participant_can_download_hidden_game_attachment() {
    let harness = AssetAuthorizationHarness::new().await;
    let user_id = Uuid::new_v4();
    sqlx::raw_sql(
        r#"
        INSERT INTO "Games" (id, hidden) VALUES (205, TRUE);
        INSERT INTO "GameChallenges" (id, game_id, is_enabled, review_status)
        VALUES (997, 205, TRUE, 0);
        INSERT INTO "Participations" (id, game_id, team_id, status)
        VALUES (17, 205, 9, 1);
        "#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "UserParticipations"
             (user_id, game_id, team_id, participation_id)
           VALUES ($1, 205, 9, 17)"#,
    )
    .bind(user_id)
    .execute(&harness.pool)
    .await
    .unwrap();

    let target = AssetTarget {
        game_id: 205,
        source_team: None,
        challenge_id: Some(997),
    };
    assert!(
        participant_can_download_target(&harness.pool, user_id, &target)
            .await
            .unwrap(),
        "hidden-game participation was incorrectly treated as public discovery"
    );

    sqlx::query(r#"UPDATE "Participations" SET status = 2 WHERE id = 17"#)
        .execute(&harness.pool)
        .await
        .unwrap();
    assert!(
        !participant_can_download_target(&harness.pool, user_id, &target)
            .await
            .unwrap(),
        "rejected participation still authorized the private attachment"
    );

    harness.cleanup().await;
}
