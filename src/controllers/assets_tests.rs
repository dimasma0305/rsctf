use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use uuid::Uuid;

use super::{participant_can_download_target, AssetTarget};

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
              hidden BOOLEAN NOT NULL
            );
            CREATE TABLE "GameChallenges" (
              id INTEGER PRIMARY KEY,
              game_id INTEGER NOT NULL,
              is_enabled BOOLEAN NOT NULL,
              review_status SMALLINT NOT NULL
            );
            CREATE TABLE "Participations" (
              id INTEGER PRIMARY KEY,
              game_id INTEGER NOT NULL,
              team_id INTEGER NOT NULL,
              status SMALLINT NOT NULL
            );
            CREATE TABLE "UserParticipations" (
              user_id UUID NOT NULL,
              game_id INTEGER NOT NULL,
              team_id INTEGER NOT NULL,
              participation_id INTEGER NOT NULL,
              PRIMARY KEY (user_id, game_id)
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
