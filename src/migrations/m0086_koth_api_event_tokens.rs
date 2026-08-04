//! Give Leaderboard/API KotH one explicit, event-scoped player capability.
//!
//! Boot2Root marker capabilities remain in `KothTokens` and continue rotating
//! with every crown cycle. API arenas instead authenticate through this table;
//! a token changes only when its team deliberately rotates it or loses roster
//! eligibility.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "KothApiTeamTokens" (
    game_id INTEGER NOT NULL,
    challenge_id INTEGER NOT NULL,
    participation_id INTEGER NOT NULL,
    token TEXT NOT NULL,
    generation INTEGER NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    rotated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    last_used_at TIMESTAMPTZ NULL,
    PRIMARY KEY (game_id, challenge_id, participation_id),
    CONSTRAINT uq_koth_api_team_tokens_token UNIQUE (token),
    CONSTRAINT fk_koth_api_team_tokens_challenge
        FOREIGN KEY (game_id, challenge_id)
        REFERENCES "GameChallenges"(game_id, id)
        ON DELETE CASCADE,
    CONSTRAINT fk_koth_api_team_tokens_participation
        FOREIGN KEY (game_id, participation_id)
        REFERENCES "Participations"(game_id, id)
        ON DELETE CASCADE,
    CONSTRAINT ck_koth_api_team_tokens_shape CHECK (
        token ~ '^koth_[A-Za-z0-9_-]{8,128}$'
        AND generation >= 1
    )
);

-- Preserve the capability already shown to players when upgrading a running
-- event. DISTINCT ON chooses the newest previously activated crown-cycle row
-- for each team and API hill. This deliberately also covers the short reset
-- interval after a cycle leaves Active; a never-activated failed attempt is
-- excluded. Future resets use ON CONFLICT DO NOTHING and keep this value.
INSERT INTO "KothApiTeamTokens"
    (game_id, challenge_id, participation_id, token, created_at, rotated_at)
SELECT chosen.game_id, chosen.challenge_id, chosen.participation_id,
       chosen.token, chosen.submitted_at, chosen.submitted_at
  FROM (
    SELECT DISTINCT ON (
               cycle.game_id, token.challenge_id, token.participation_id
           )
           cycle.game_id, token.challenge_id, token.participation_id,
           token.token, token.submitted_at
      FROM "KothTokens" token
      JOIN "KothCrownCycles" cycle ON cycle.id = token.cycle_id
      JOIN "Games" game ON game.id = cycle.game_id
      JOIN "KothOfficialConfigs" config ON config.game_id = cycle.game_id
      JOIN LATERAL jsonb_array_elements(config.hills_snapshot) hill
        ON (hill->>'challengeId')::integer = token.challenge_id
       AND COALESCE(NULLIF(hill->>'claimSource', ''), 'Marker') = 'Api'
     WHERE token.challenge_id = cycle.challenge_id
       AND token.reset_attempt = cycle.reset_attempt
       AND cycle.activated_at IS NOT NULL
       AND clock_timestamp() < game.end_time_utc
     ORDER BY cycle.game_id, token.challenge_id, token.participation_id,
              cycle.cycle_number DESC, token.id DESC
  ) chosen
ON CONFLICT DO NOTHING;
"#;

const DOWN_SQL: &str = r#"
DROP TABLE IF EXISTS "KothApiTeamTokens";
"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(DOWN_SQL)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use sqlx::{Connection, PgConnection};

    use super::UP_SQL;

    #[test]
    fn api_tokens_are_event_scoped_but_explicitly_rotatable() {
        assert!(UP_SQL.contains("PRIMARY KEY (game_id, challenge_id, participation_id)"));
        assert!(UP_SQL.contains("CONSTRAINT uq_koth_api_team_tokens_token UNIQUE (token)"));
        assert!(UP_SQL.contains("generation INTEGER NOT NULL DEFAULT 1"));
        assert!(UP_SQL.contains("DISTINCT ON"));
        assert!(UP_SQL.contains("cycle.activated_at IS NOT NULL"));
        assert!(UP_SQL.contains("claimSource', ''), 'Marker') = 'Api'"));
        assert!(UP_SQL.contains("ON CONFLICT DO NOTHING"));
        assert!(!UP_SQL.contains("CREATE EXTENSION"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn migration_preserves_the_live_api_token_across_idempotent_replay() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let mut connection = PgConnection::connect(&database_url).await.unwrap();
        let schema = format!("rsctf_m0086_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query(&format!(r#"SET search_path TO "{schema}""#))
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "GameChallenges" (
              game_id INTEGER, id INTEGER, UNIQUE (game_id, id)
            );
            CREATE TABLE "Games" (
              id INTEGER PRIMARY KEY, end_time_utc TIMESTAMPTZ
            );
            CREATE TABLE "Participations" (
              game_id INTEGER, id INTEGER, UNIQUE (game_id, id)
            );
            CREATE TABLE "KothOfficialConfigs" (
              game_id INTEGER, hills_snapshot JSONB
            );
            CREATE TABLE "KothCrownCycles" (
              id BIGINT, game_id INTEGER, challenge_id INTEGER,
              cycle_number INTEGER, reset_attempt INTEGER,
              replacement_container_id TEXT, phase TEXT,
              activated_at TIMESTAMPTZ
            );
            CREATE TABLE "KothTokens" (
              id INTEGER, target_id INTEGER, participation_id INTEGER,
              token TEXT, submitted_at TIMESTAMPTZ, revoked_at TIMESTAMPTZ,
              cycle_id BIGINT, challenge_id INTEGER, reset_attempt INTEGER
            );
            INSERT INTO "GameChallenges" VALUES (7, 9);
            INSERT INTO "Games" VALUES
              (7, clock_timestamp() + interval '1 hour');
            INSERT INTO "Participations" VALUES (7, 11);
            INSERT INTO "KothOfficialConfigs" VALUES
              (7, '[{"challengeId":9,"claimSource":"Api"}]');
            INSERT INTO "KothCrownCycles" VALUES
              (41, 7, 9, 4, 0, 'runtime-a', 'DestroyPending',
               clock_timestamp() - interval '1 minute');
            INSERT INTO "KothTokens" VALUES
              (101, 3, 11, 'koth_current_token_a', clock_timestamp(), NULL,
               41, 9, 0);
            "#,
        )
        .execute(&mut connection)
        .await
        .unwrap();

        sqlx::raw_sql(UP_SQL)
            .execute(&mut connection)
            .await
            .unwrap();
        let first: String = sqlx::query_scalar(
            r#"SELECT token FROM "KothApiTeamTokens"
                WHERE game_id = 7 AND challenge_id = 9 AND participation_id = 11"#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(first, "koth_current_token_a");

        sqlx::raw_sql(
            r#"UPDATE "KothCrownCycles" SET phase = 'Completed' WHERE id = 41;
               INSERT INTO "KothCrownCycles" VALUES
                 (42, 7, 9, 5, 0, 'runtime-b', 'Active', clock_timestamp());
               INSERT INTO "KothTokens" VALUES
                 (102, 3, 11, 'koth_next_cycle_token', clock_timestamp(), NULL,
                  42, 9, 0);"#,
        )
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::raw_sql(UP_SQL)
            .execute(&mut connection)
            .await
            .unwrap();
        let preserved: (String, i32) = sqlx::query_as(
            r#"SELECT token, generation FROM "KothApiTeamTokens"
                WHERE game_id = 7 AND challenge_id = 9 AND participation_id = 11"#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(preserved, ("koth_current_token_a".to_string(), 1));

        sqlx::query("SET search_path TO public")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&mut connection)
            .await
            .unwrap();
    }
}
