//! Replace single-holder API observations with normalized, per-team arena evidence.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP_SQL: &str = r#"
-- API snapshots are ephemeral input, so the old single-holder observation has
-- no historical value. Durable scored evidence lives in KothApiScoreResults.
DROP TABLE IF EXISTS "KothApiObservations";

-- The objective scheme is event/challenge state, not observer-credential
-- state. It must survive referee secret rotation and revocation.
CREATE TABLE IF NOT EXISTS "KothApiArenaSchemes" (
    challenge_id INTEGER PRIMARY KEY,
    game_id INTEGER NOT NULL,
    objective_count SMALLINT NOT NULL,
    frozen_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT fk_koth_api_arena_schemes_challenge
        FOREIGN KEY (game_id, challenge_id)
        REFERENCES "GameChallenges"(game_id, id)
        ON DELETE CASCADE,
    CONSTRAINT uq_koth_api_arena_schemes_game_challenge
        UNIQUE (game_id, challenge_id),
    CONSTRAINT ck_koth_api_arena_schemes_objective_count
        CHECK (objective_count BETWEEN 1 AND 16)
);

CREATE TABLE IF NOT EXISTS "KothApiSnapshots" (
    target_id INTEGER PRIMARY KEY,
    game_id INTEGER NOT NULL,
    challenge_id INTEGER NOT NULL,
    cycle_id BIGINT NOT NULL,
    reset_attempt INTEGER NOT NULL,
    container_id TEXT NOT NULL,
    ad_round_id INTEGER NOT NULL,
    context_hash CHAR(64) NOT NULL,
    snapshot_hash BYTEA NOT NULL,
    request_timestamp_ms BIGINT NOT NULL,
    accepted_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT fk_koth_api_snapshots_challenge
        FOREIGN KEY (game_id, challenge_id)
        REFERENCES "GameChallenges"(game_id, id)
        ON DELETE CASCADE,
    CONSTRAINT fk_koth_api_snapshots_target
        FOREIGN KEY (target_id, challenge_id)
        REFERENCES "KothTargets"(id, challenge_id)
        ON DELETE CASCADE,
    CONSTRAINT fk_koth_api_snapshots_cycle
        FOREIGN KEY (cycle_id, challenge_id)
        REFERENCES "KothCrownCycles"(id, challenge_id)
        ON DELETE CASCADE,
    CONSTRAINT fk_koth_api_snapshots_round
        FOREIGN KEY (game_id, ad_round_id)
        REFERENCES "AdRounds"(game_id, id)
        ON DELETE CASCADE,
    CONSTRAINT ck_koth_api_snapshots_attempt
        CHECK (reset_attempt >= 0),
    CONSTRAINT ck_koth_api_snapshots_container
        CHECK (BTRIM(container_id) <> ''),
    CONSTRAINT ck_koth_api_snapshots_context
        CHECK (context_hash ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_koth_api_snapshots_hash
        CHECK (OCTET_LENGTH(snapshot_hash) = 32)
);

CREATE INDEX IF NOT EXISTS ix_koth_api_snapshots_context
    ON "KothApiSnapshots"(cycle_id, reset_attempt, ad_round_id, target_id);

CREATE TABLE IF NOT EXISTS "KothApiSnapshotScores" (
    target_id INTEGER NOT NULL
        REFERENCES "KothApiSnapshots"(target_id)
        ON DELETE CASCADE,
    participation_id INTEGER NOT NULL
        REFERENCES "Participations"(id)
        ON DELETE CASCADE,
    activity_earned BIGINT NOT NULL,
    activity_possible BIGINT NOT NULL,
    objective_earned BIGINT NOT NULL,
    objective_possible BIGINT NOT NULL,
    valid_actions BIGINT NOT NULL,
    total_actions BIGINT NOT NULL,
    objective_count SMALLINT NOT NULL,
    PRIMARY KEY (target_id, participation_id),
    CONSTRAINT ck_koth_api_snapshot_scores_activity
        CHECK (
            activity_earned >= 0
            AND activity_possible > 0
            AND activity_earned <= activity_possible
        ),
    CONSTRAINT ck_koth_api_snapshot_scores_objective
        CHECK (
            objective_earned >= 0
            AND objective_possible > 0
            AND objective_earned <= objective_possible
            AND objective_count BETWEEN 1 AND 16
        ),
    CONSTRAINT ck_koth_api_snapshot_scores_integrity
        CHECK (
            valid_actions >= 0
            AND total_actions > 0
            AND valid_actions <= total_actions
        )
);

CREATE TABLE IF NOT EXISTS "KothApiScoreResults" (
    game_id INTEGER NOT NULL,
    challenge_id INTEGER NOT NULL,
    ad_round_id INTEGER NOT NULL,
    participation_id INTEGER NOT NULL,
    activity_earned BIGINT NOT NULL,
    activity_possible BIGINT NOT NULL,
    objective_earned BIGINT NOT NULL,
    objective_possible BIGINT NOT NULL,
    valid_actions BIGINT NOT NULL,
    total_actions BIGINT NOT NULL,
    objective_count SMALLINT NOT NULL,
    activity_rate DOUBLE PRECISION NOT NULL,
    objective_rate DOUBLE PRECISION NOT NULL,
    integrity_rate DOUBLE PRECISION NOT NULL,
    core_rate DOUBLE PRECISION NOT NULL,
    score_rate DOUBLE PRECISION NOT NULL,
    PRIMARY KEY (game_id, challenge_id, ad_round_id, participation_id),
    CONSTRAINT fk_koth_api_score_results_control
        FOREIGN KEY (game_id, challenge_id, ad_round_id)
        REFERENCES "KothControlResults"(game_id, challenge_id, ad_round_id)
        ON DELETE CASCADE,
    CONSTRAINT fk_koth_api_score_results_participation
        FOREIGN KEY (game_id, participation_id)
        REFERENCES "Participations"(game_id, id)
        ON DELETE CASCADE,
    CONSTRAINT ck_koth_api_score_results_activity
        CHECK (
            activity_earned >= 0
            AND activity_possible > 0
            AND activity_earned <= activity_possible
            AND activity_rate BETWEEN 0.0 AND 1.0
        ),
    CONSTRAINT ck_koth_api_score_results_objective
        CHECK (
            objective_earned >= 0
            AND objective_possible > 0
            AND objective_earned <= objective_possible
            AND objective_count BETWEEN 1 AND 16
            AND objective_rate BETWEEN 0.0 AND 1.0
        ),
    CONSTRAINT ck_koth_api_score_results_integrity
        CHECK (
            valid_actions >= 0
            AND total_actions > 0
            AND valid_actions <= total_actions
            AND integrity_rate BETWEEN 0.0 AND 1.0
        ),
    CONSTRAINT ck_koth_api_score_results_derived
        CHECK (
            core_rate BETWEEN 0.0 AND 1.0
            AND score_rate BETWEEN 0.0 AND core_rate
        )
);

CREATE INDEX IF NOT EXISTS ix_koth_api_score_results_round
    ON "KothApiScoreResults"(game_id, ad_round_id, challenge_id);
CREATE INDEX IF NOT EXISTS ix_koth_api_score_results_team
    ON "KothApiScoreResults"(game_id, participation_id, challenge_id, ad_round_id);

-- The former API transport produced exclusive-holder evidence. It cannot be
-- interpreted under the arena formula, so fail closed instead of silently
-- converting historical holder ticks into zeros or new-style performance.
WITH api_hills AS (
    SELECT config.game_id, (hill->>'challengeId')::integer AS challenge_id
      FROM "KothOfficialConfigs" config,
           LATERAL jsonb_array_elements(config.hills_snapshot) hill
     WHERE COALESCE(NULLIF(hill->>'claimSource', ''), 'Marker') = 'Api'
)
UPDATE "KothControlResults" result
   SET controlling_participation_id = NULL,
       responsible_participation_id = NULL,
       token_id = NULL,
       token_window_round = NULL,
       provisional_participation_id = NULL,
       confirmed_participation_id = NULL,
       marker_observed = FALSE,
       confirmation_streak = 0,
       is_scorable = FALSE,
       void_reason = 'pre-arena API evidence is incompatible with normalized arena scoring'
  FROM api_hills
 WHERE result.game_id = api_hills.game_id
   AND result.challenge_id = api_hills.challenge_id;

DELETE FROM "KothEpochRollups" rollup
 WHERE EXISTS (
       SELECT 1
         FROM "KothOfficialConfigs" config,
              LATERAL jsonb_array_elements(config.hills_snapshot) hill
        WHERE config.game_id = rollup.game_id
          AND COALESCE(NULLIF(hill->>'claimSource', ''), 'Marker') = 'Api'
 );

UPDATE "KothTargets" target
   SET holder_participation_id = NULL,
       held_since = NULL
 WHERE EXISTS (
       SELECT 1
         FROM "KothOfficialConfigs" config,
              LATERAL jsonb_array_elements(config.hills_snapshot) hill
        WHERE config.game_id = target.game_id
          AND (hill->>'challengeId')::integer = target.challenge_id
          AND COALESCE(NULLIF(hill->>'claimSource', ''), 'Marker') = 'Api'
 );

DELETE FROM "KothClaimStates" claim
 WHERE EXISTS (
       SELECT 1
         FROM "KothTargets" target
         JOIN "KothOfficialConfigs" config ON config.game_id = target.game_id,
              LATERAL jsonb_array_elements(config.hills_snapshot) hill
        WHERE target.id = claim.target_id
          AND (hill->>'challengeId')::integer = target.challenge_id
          AND COALESCE(NULLIF(hill->>'claimSource', ''), 'Marker') = 'Api'
 );

DELETE FROM "KothCycleCooldowns" cooldown
 WHERE EXISTS (
       SELECT 1
         FROM "KothCrownCycles" cycle
         JOIN "KothOfficialConfigs" config ON config.game_id = cycle.game_id,
              LATERAL jsonb_array_elements(config.hills_snapshot) hill
        WHERE cycle.id = cooldown.cycle_id
          AND (hill->>'challengeId')::integer = cycle.challenge_id
          AND COALESCE(NULLIF(hill->>'claimSource', ''), 'Marker') = 'Api'
 );

DELETE FROM "KothAcquisitions" acquisition
 WHERE EXISTS (
       SELECT 1
         FROM "KothOfficialConfigs" config,
              LATERAL jsonb_array_elements(config.hills_snapshot) hill
        WHERE config.game_id = acquisition.game_id
          AND (hill->>'challengeId')::integer = acquisition.challenge_id
          AND COALESCE(NULLIF(hill->>'claimSource', ''), 'Marker') = 'Api'
 );

UPDATE "KothCrownCycles" cycle
   SET provisional_participation_id = NULL,
       confirmed_participation_id = NULL,
       confirmation_progress = 0
 WHERE EXISTS (
       SELECT 1
         FROM "KothOfficialConfigs" config,
              LATERAL jsonb_array_elements(config.hills_snapshot) hill
        WHERE config.game_id = cycle.game_id
          AND (hill->>'challengeId')::integer = cycle.challenge_id
          AND COALESCE(NULLIF(hill->>'claimSource', ''), 'Marker') = 'Api'
 );
"#;

const DOWN_SQL: &str = r#"
DROP TABLE IF EXISTS "KothApiScoreResults";
DROP TABLE IF EXISTS "KothApiSnapshotScores";
DROP TABLE IF EXISTS "KothApiSnapshots";
DROP TABLE IF EXISTS "KothApiArenaSchemes";
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
    use std::str::FromStr;

    use sea_orm_migration::sea_orm::SqlxPostgresConnector;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::UP_SQL;
    use crate::migrations::{Migrator, MigratorTrait};

    #[test]
    fn arena_evidence_is_round_bound_dense_and_bounded() {
        assert!(UP_SQL.contains("ad_round_id INTEGER NOT NULL"));
        assert!(UP_SQL.contains("OCTET_LENGTH(snapshot_hash) = 32"));
        assert!(UP_SQL.contains("activity_earned <= activity_possible"));
        assert!(UP_SQL.contains("objective_count BETWEEN 1 AND 16"));
        assert!(UP_SQL.contains("CREATE TABLE IF NOT EXISTS \"KothApiArenaSchemes\""));
        assert!(UP_SQL.contains("ck_koth_api_arena_schemes_objective_count"));
        assert!(UP_SQL.contains("uq_koth_api_arena_schemes_game_challenge"));
        assert!(UP_SQL.contains("valid_actions <= total_actions"));
        assert!(UP_SQL.contains("score_rate BETWEEN 0.0 AND core_rate"));
        assert!(UP_SQL.contains("REFERENCES \"KothControlResults\""));
        assert!(UP_SQL.contains("pre-arena API evidence is incompatible"));
        assert!(UP_SQL.contains("DELETE FROM \"KothEpochRollups\""));
        assert!(UP_SQL.contains("DELETE FROM \"KothCycleCooldowns\""));
        assert!(UP_SQL.contains("DELETE FROM \"KothAcquisitions\""));
        assert!(
            UP_SQL.contains("PRIMARY KEY (game_id, challenge_id, ad_round_id, participation_id)")
        );
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn upgrades_the_real_m0083_schema_without_retaining_holder_observations() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("rsctf_m0084_{}", uuid::Uuid::new_v4().simple());
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
        let db = SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone());
        let user_tables: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*) FROM information_schema.tables
                WHERE table_schema = current_schema()
                  AND table_name <> 'seaql_migrations'"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(user_tables, 0, "migration regression database is not empty");

        Migrator::up(&db, Some(83)).await.unwrap();
        let old_table: bool =
            sqlx::query_scalar(r#"SELECT to_regclass('"KothApiObservations"') IS NOT NULL"#)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(old_table);

        Migrator::up(&db, Some(1)).await.unwrap();
        let relations = sqlx::query_as::<_, (bool, bool, bool, bool, bool)>(
            r#"SELECT
                 to_regclass('"KothApiObservations"') IS NULL,
                 to_regclass('"KothApiArenaSchemes"') IS NOT NULL,
                 to_regclass('"KothApiSnapshots"') IS NOT NULL,
                 to_regclass('"KothApiSnapshotScores"') IS NOT NULL,
                 to_regclass('"KothApiScoreResults"') IS NOT NULL"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(relations, (true, true, true, true, true));

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
