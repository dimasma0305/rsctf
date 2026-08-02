//! Adopt the constant Leaderboard KotH contract and remove obsolete formula keys.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP_SQL: &str = r#"
-- This is an atomic scoring-policy boundary, not an in-event version switch.
-- Refuse deployment while an affected hill is accepting official evidence.
DO $$
BEGIN
  IF EXISTS (
    SELECT 1
      FROM "Games" game
      JOIN "KothOfficialConfigs" config ON config.game_id = game.id,
           LATERAL jsonb_array_elements(config.hills_snapshot) hill
     WHERE config.scoring_start_round IS NOT NULL
       AND COALESCE(NULLIF(hill->>'claimSource', ''), 'Marker') = 'Api'
       AND clock_timestamp() >= game.start_time_utc
       AND clock_timestamp() < game.end_time_utc
  ) THEN
    RAISE EXCEPTION
      'cannot adopt constant Leaderboard scoring while an API KotH hill is live'
      USING HINT = 'End the affected event and deploy at a scoring boundary.';
  END IF;
END
$$;

-- Referee snapshots are replaceable current-tick input. Clearing them is safe at
-- the enforced event boundary and prevents an old count-only objective scheme
-- from being accepted under the identity-bound contract.
DELETE FROM "KothApiSnapshots";
DELETE FROM "KothApiArenaSchemes";

ALTER TABLE "KothApiArenaSchemes"
  ADD COLUMN objective_ids TEXT[] NOT NULL,
  ADD COLUMN objective_schema_hash BYTEA NOT NULL,
  ADD CONSTRAINT uq_koth_api_arena_schemes_schema
    UNIQUE (game_id, challenge_id, objective_schema_hash),
  ADD CONSTRAINT ck_koth_api_arena_schemes_identity CHECK (
    cardinality(objective_ids) = objective_count
    AND array_position(objective_ids, '') IS NULL
    AND OCTET_LENGTH(objective_schema_hash) = 32
  );

ALTER TABLE "KothApiSnapshots"
  ADD COLUMN objective_schema_hash BYTEA NOT NULL,
  ADD CONSTRAINT fk_koth_api_snapshots_objective_schema
    FOREIGN KEY (game_id, challenge_id, objective_schema_hash)
    REFERENCES "KothApiArenaSchemes"
      (game_id, challenge_id, objective_schema_hash)
    ON DELETE CASCADE,
  ADD CONSTRAINT ck_koth_api_snapshots_objective_schema
    CHECK (OCTET_LENGTH(objective_schema_hash) = 32);

-- Invalid attempts are retained by challenge telemetry and competition rules;
-- they are no longer a points multiplier. Existing immutable tick cores remain
-- valid normalized performance evidence. Settled rollups are deliberately not
-- rewritten, so past awards do not move during this deployment.
ALTER TABLE "KothApiSnapshotScores"
  DROP CONSTRAINT IF EXISTS ck_koth_api_snapshot_scores_integrity,
  DROP COLUMN valid_actions,
  DROP COLUMN total_actions;

ALTER TABLE "KothApiScoreResults"
  DROP CONSTRAINT IF EXISTS ck_koth_api_score_results_integrity,
  DROP CONSTRAINT IF EXISTS ck_koth_api_score_results_derived,
  DROP COLUMN valid_actions,
  DROP COLUMN total_actions,
  DROP COLUMN integrity_rate;
ALTER TABLE "KothApiScoreResults"
  RENAME COLUMN score_rate TO performance_rate;

UPDATE "KothApiScoreResults"
   SET performance_rate = core_rate;

ALTER TABLE "KothApiScoreResults"
  ADD COLUMN lead_credit DOUBLE PRECISION NOT NULL DEFAULT 0.0;

WITH tick_stats AS (
  SELECT game_id, challenge_id, ad_round_id,
         MAX(core_rate) AS highest_core,
         COUNT(*) FILTER (WHERE core_rate > 0.0) AS positive_teams
    FROM "KothApiScoreResults"
   GROUP BY game_id, challenge_id, ad_round_id
), leader_counts AS (
  SELECT score.game_id, score.challenge_id, score.ad_round_id,
         stats.highest_core, stats.positive_teams,
         COUNT(*) FILTER (WHERE score.core_rate = stats.highest_core)
           AS tied_leaders
    FROM "KothApiScoreResults" score
    JOIN tick_stats stats USING (game_id, challenge_id, ad_round_id)
   GROUP BY score.game_id, score.challenge_id, score.ad_round_id,
            stats.highest_core, stats.positive_teams
)
UPDATE "KothApiScoreResults" score
   SET lead_credit = CASE
       WHEN leaders.positive_teams >= 2
        AND score.core_rate = leaders.highest_core
       THEN 1.0 / leaders.tied_leaders::double precision
       ELSE 0.0
   END
  FROM leader_counts leaders
 WHERE score.game_id = leaders.game_id
   AND score.challenge_id = leaders.challenge_id
   AND score.ad_round_id = leaders.ad_round_id;

ALTER TABLE "KothApiScoreResults"
  ALTER COLUMN lead_credit DROP DEFAULT,
  ADD CONSTRAINT ck_koth_api_score_results_leaderboard CHECK (
    core_rate BETWEEN 0.0 AND 1.0
    AND performance_rate = core_rate
    AND lead_credit BETWEEN 0.0 AND 1.0
  );

-- m0058 kept fixed-value columns only as a rolling-upgrade compatibility shim.
-- The runtime has been versionless since then; contract the schema now.
ALTER TABLE "Games"
  DROP CONSTRAINT IF EXISTS ck_games_koth_scoring_formula_version,
  DROP COLUMN IF EXISTS koth_scoring_formula_version;

ALTER TABLE "KothCrownCycles"
  DROP CONSTRAINT IF EXISTS fk_koth_crown_cycles_config_compat,
  DROP CONSTRAINT IF EXISTS ux_koth_crown_cycles_identity_compat,
  DROP CONSTRAINT IF EXISTS ck_koth_crown_cycles_identity;
ALTER TABLE "KothOfficialConfigs"
  DROP CONSTRAINT IF EXISTS ux_koth_official_configs_game_formula,
  DROP CONSTRAINT IF EXISTS ck_koth_official_configs_version;
ALTER TABLE "KothCrownCycles"
  DROP COLUMN IF EXISTS formula_version,
  ADD CONSTRAINT ck_koth_crown_cycles_identity
    CHECK (cycle_number >= 1 AND epoch >= 1);
ALTER TABLE "KothOfficialConfigs"
  DROP COLUMN IF EXISTS formula_version;

ALTER TABLE "KothEpochTeamRollups"
  DROP CONSTRAINT IF EXISTS
    "KothEpochTeamRollups_game_id_formula_version_epoch_fkey",
  DROP CONSTRAINT IF EXISTS ux_koth_epoch_team_rollups_compat,
  DROP CONSTRAINT IF EXISTS ck_koth_epoch_team_rollups_constant_formula;
ALTER TABLE "KothEpochHillRollups"
  DROP CONSTRAINT IF EXISTS
    "KothEpochHillRollups_game_id_formula_version_epoch_fkey",
  DROP CONSTRAINT IF EXISTS ux_koth_epoch_hill_rollups_compat,
  DROP CONSTRAINT IF EXISTS ck_koth_epoch_hill_rollups_constant_formula;
ALTER TABLE "KothEpochRollups"
  DROP CONSTRAINT IF EXISTS ux_koth_epoch_rollups_compat,
  DROP CONSTRAINT IF EXISTS ck_koth_epoch_rollups_identity;

ALTER TABLE "KothEpochTeamRollups" DROP COLUMN IF EXISTS formula_version;
ALTER TABLE "KothEpochHillRollups" DROP COLUMN IF EXISTS formula_version;
ALTER TABLE "KothEpochRollups"
  DROP COLUMN IF EXISTS formula_version,
  ADD CONSTRAINT ck_koth_epoch_rollups_identity CHECK (epoch >= 1);

-- The Jeopardy board now starts from the bounded canonical projection instead
-- of scanning submission history. Its primary key starts with participation_id,
-- so add the reverse lookup used by game/challenge-scoped reads.
CREATE INDEX IF NOT EXISTS ix_firstsolves_challenge
  ON "FirstSolves"(challenge_id, participation_id);
"#;

const DOWN_SQL: &str = r#"
DROP INDEX IF EXISTS ix_firstsolves_challenge;

ALTER TABLE "Games"
  ADD COLUMN koth_scoring_formula_version SMALLINT NOT NULL DEFAULT 2,
  ADD CONSTRAINT ck_games_koth_scoring_formula_version
    CHECK (koth_scoring_formula_version = 2);

ALTER TABLE "KothOfficialConfigs"
  ADD COLUMN formula_version SMALLINT NOT NULL DEFAULT 2,
  ADD CONSTRAINT ux_koth_official_configs_game_formula
    UNIQUE (game_id, formula_version),
  ADD CONSTRAINT ck_koth_official_configs_version CHECK (formula_version = 2);
ALTER TABLE "KothCrownCycles"
  DROP CONSTRAINT IF EXISTS ck_koth_crown_cycles_identity,
  ADD COLUMN formula_version SMALLINT NOT NULL DEFAULT 2,
  ADD CONSTRAINT ux_koth_crown_cycles_identity_compat
    UNIQUE (game_id, challenge_id, formula_version, cycle_number),
  ADD CONSTRAINT fk_koth_crown_cycles_config_compat
    FOREIGN KEY (game_id, formula_version)
    REFERENCES "KothOfficialConfigs"(game_id, formula_version)
    ON DELETE CASCADE,
  ADD CONSTRAINT ck_koth_crown_cycles_identity
    CHECK (formula_version = 2 AND cycle_number >= 1 AND epoch >= 1);

ALTER TABLE "KothEpochRollups"
  DROP CONSTRAINT IF EXISTS ck_koth_epoch_rollups_identity,
  ADD COLUMN formula_version SMALLINT NOT NULL DEFAULT 2,
  ADD CONSTRAINT ux_koth_epoch_rollups_compat
    UNIQUE (game_id, formula_version, epoch),
  ADD CONSTRAINT ck_koth_epoch_rollups_identity
    CHECK (formula_version = 2 AND epoch >= 1);
ALTER TABLE "KothEpochTeamRollups"
  ADD COLUMN formula_version SMALLINT NOT NULL DEFAULT 2,
  ADD CONSTRAINT ux_koth_epoch_team_rollups_compat
    UNIQUE (game_id, formula_version, epoch, participation_id),
  ADD CONSTRAINT "KothEpochTeamRollups_game_id_formula_version_epoch_fkey"
    FOREIGN KEY (game_id, formula_version, epoch)
    REFERENCES "KothEpochRollups"(game_id, formula_version, epoch)
    ON DELETE CASCADE,
  ADD CONSTRAINT ck_koth_epoch_team_rollups_constant_formula
    CHECK (formula_version = 2);
ALTER TABLE "KothEpochHillRollups"
  ADD COLUMN formula_version SMALLINT NOT NULL DEFAULT 2,
  ADD CONSTRAINT ux_koth_epoch_hill_rollups_compat
    UNIQUE (game_id, formula_version, epoch, participation_id, challenge_id),
  ADD CONSTRAINT "KothEpochHillRollups_game_id_formula_version_epoch_fkey"
    FOREIGN KEY (game_id, formula_version, epoch)
    REFERENCES "KothEpochRollups"(game_id, formula_version, epoch)
    ON DELETE CASCADE,
  ADD CONSTRAINT ck_koth_epoch_hill_rollups_constant_formula
    CHECK (formula_version = 2);

ALTER TABLE "KothApiScoreResults"
  DROP CONSTRAINT IF EXISTS ck_koth_api_score_results_leaderboard,
  DROP COLUMN lead_credit;
ALTER TABLE "KothApiScoreResults"
  RENAME COLUMN performance_rate TO score_rate;
ALTER TABLE "KothApiScoreResults"
  ADD COLUMN valid_actions BIGINT NOT NULL DEFAULT 0,
  ADD COLUMN total_actions BIGINT NOT NULL DEFAULT 1,
  ADD COLUMN integrity_rate DOUBLE PRECISION NOT NULL DEFAULT 0.0,
  ADD CONSTRAINT ck_koth_api_score_results_integrity CHECK (
    valid_actions >= 0 AND total_actions > 0
    AND valid_actions <= total_actions
    AND integrity_rate BETWEEN 0.0 AND 1.0
  ),
  ADD CONSTRAINT ck_koth_api_score_results_derived CHECK (
    core_rate BETWEEN 0.0 AND 1.0
    AND score_rate BETWEEN 0.0 AND core_rate
  );
ALTER TABLE "KothApiSnapshotScores"
  ADD COLUMN valid_actions BIGINT NOT NULL DEFAULT 0,
  ADD COLUMN total_actions BIGINT NOT NULL DEFAULT 1,
  ADD CONSTRAINT ck_koth_api_snapshot_scores_integrity CHECK (
    valid_actions >= 0 AND total_actions > 0 AND valid_actions <= total_actions
  );

ALTER TABLE "KothApiSnapshots"
  DROP CONSTRAINT IF EXISTS fk_koth_api_snapshots_objective_schema,
  DROP CONSTRAINT IF EXISTS ck_koth_api_snapshots_objective_schema,
  DROP COLUMN objective_schema_hash;
ALTER TABLE "KothApiArenaSchemes"
  DROP CONSTRAINT IF EXISTS uq_koth_api_arena_schemes_schema,
  DROP CONSTRAINT IF EXISTS ck_koth_api_arena_schemes_identity,
  DROP COLUMN objective_ids,
  DROP COLUMN objective_schema_hash;
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
    fn migration_removes_selectors_and_binds_leaderboard_evidence() {
        assert!(UP_SQL.contains("DROP COLUMN IF EXISTS koth_scoring_formula_version"));
        assert!(UP_SQL.contains("DROP COLUMN IF EXISTS formula_version"));
        assert!(UP_SQL.contains("objective_ids TEXT[] NOT NULL"));
        assert!(UP_SQL.contains("objective_schema_hash BYTEA NOT NULL"));
        assert!(UP_SQL.contains("RENAME COLUMN score_rate TO performance_rate"));
        assert!(UP_SQL.contains("ADD COLUMN lead_credit"));
        assert!(UP_SQL.contains("ix_firstsolves_challenge"));
        assert!(!UP_SQL.contains("ADD COLUMN formula_version SMALLINT"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn upgrades_arena_schema_to_one_versionless_leaderboard_contract() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("rsctf_m0085_{}", uuid::Uuid::new_v4().simple());
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

        Migrator::up(&db, Some(84)).await.unwrap();
        let legacy_selectors: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*)
                 FROM information_schema.columns
                WHERE table_schema = current_schema()
                  AND (
                    (table_name = 'Games'
                     AND column_name = 'koth_scoring_formula_version')
                    OR (table_name IN (
                          'KothOfficialConfigs', 'KothCrownCycles',
                          'KothEpochRollups', 'KothEpochTeamRollups',
                          'KothEpochHillRollups'
                        ) AND column_name = 'formula_version')
                  )"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(legacy_selectors, 6);

        Migrator::up(&db, Some(1)).await.unwrap();
        let shape = sqlx::query_as::<_, (i64, i64, i64, i64)>(
            r#"SELECT
                 COUNT(*) FILTER (WHERE column_name IN (
                   'objective_ids', 'objective_schema_hash'
                 )),
                 COUNT(*) FILTER (WHERE column_name = 'performance_rate'),
                 COUNT(*) FILTER (WHERE column_name = 'lead_credit'),
                 COUNT(*) FILTER (WHERE column_name IN (
                   'valid_actions', 'total_actions', 'integrity_rate',
                   'score_rate', 'formula_version',
                   'koth_scoring_formula_version'
                 ))
               FROM information_schema.columns
              WHERE table_schema = current_schema()
                AND table_name IN (
                  'Games', 'KothOfficialConfigs', 'KothCrownCycles',
                  'KothEpochRollups', 'KothEpochTeamRollups',
                  'KothEpochHillRollups', 'KothApiArenaSchemes',
                  'KothApiSnapshots', 'KothApiSnapshotScores',
                  'KothApiScoreResults'
                )"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(shape, (3, 1, 1, 0));

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
