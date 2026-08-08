//! Adopt finalized-wave, leader-relative scoring for Leaderboard KotH.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP_SQL: &str = r#"
-- The formula is constant at runtime. Refuse to cross this one-time policy
-- boundary while an API hill is accepting official evidence.
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
      'cannot adopt finalized-wave Leaderboard scoring while an API KotH hill is live'
      USING HINT = 'End the affected event and deploy at a scoring boundary.';
  END IF;
END
$$;

-- Snapshots are replaceable current-round input. No accepted score receipt is
-- removed here; transient input and its frozen objective schema are rebuilt by
-- the first post-deployment referee submission.
DELETE FROM "KothApiSnapshots";
DELETE FROM "KothApiArenaSchemes";

CREATE TABLE "KothApiSnapshotWaves" (
    target_id INTEGER NOT NULL
        REFERENCES "KothApiSnapshots"(target_id) ON DELETE CASCADE,
    wave_id TEXT NOT NULL,
    ended_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (target_id, wave_id),
    CONSTRAINT ck_koth_api_snapshot_waves_id CHECK (
        OCTET_LENGTH(wave_id) BETWEEN 1 AND 128
        AND wave_id ~ '^[A-Za-z0-9][A-Za-z0-9._:-]*$'
    )
);

ALTER TABLE "KothApiSnapshotScores"
  DROP CONSTRAINT "KothApiSnapshotScores_pkey",
  ADD COLUMN wave_id TEXT NOT NULL,
  ADD COLUMN is_crown BOOLEAN NOT NULL,
  ADD CONSTRAINT "KothApiSnapshotScores_pkey"
    PRIMARY KEY (target_id, wave_id, participation_id),
  ADD CONSTRAINT fk_koth_api_snapshot_scores_wave
    FOREIGN KEY (target_id, wave_id)
    REFERENCES "KothApiSnapshotWaves"(target_id, wave_id)
    ON DELETE CASCADE;

CREATE UNIQUE INDEX uq_koth_api_snapshot_wave_crown
  ON "KothApiSnapshotScores"(target_id, wave_id)
  WHERE is_crown;

ALTER TABLE "KothApiScoreResults"
  DROP CONSTRAINT ck_koth_api_score_results_leaderboard;

-- Reinterpret the retained raw channels with the new completion gate before
-- comparing teams. Partial activity was useful progress telemetry under the
-- previous implementation, but it is not a completed wave here.
UPDATE "KothApiScoreResults"
   SET activity_rate = CASE
         WHEN activity_earned = activity_possible THEN 1.0 ELSE 0.0
       END,
       objective_rate = CASE
         WHEN activity_earned = activity_possible AND objective_rate > 0.0
         THEN objective_rate
         ELSE 0.0
       END,
       core_rate = CASE
         WHEN activity_earned = activity_possible AND objective_rate > 0.0
         THEN objective_rate
         ELSE 0.0
       END;

-- Pre-wave rows each represent exactly one historical scoring opportunity.
-- Canonicalize their budgets so the epoch query can weight new multi-wave
-- round summaries and historical single-wave summaries with the same rule.
UPDATE "KothApiScoreResults"
   SET activity_earned = ROUND(activity_rate * 1000000)::bigint,
       activity_possible = 1000000,
       objective_earned = ROUND(
         objective_rate * objective_count::bigint * 1000000
       )::bigint,
       objective_possible = objective_count::bigint * 1000000;

WITH ranked AS (
    SELECT game_id, challenge_id, ad_round_id, participation_id, core_rate,
           MAX(core_rate) OVER (
             PARTITION BY game_id, challenge_id, ad_round_id
           ) AS best_rate,
           ROW_NUMBER() OVER (
             PARTITION BY game_id, challenge_id, ad_round_id
             ORDER BY core_rate DESC, participation_id
           ) AS leader_rank
      FROM "KothApiScoreResults"
)
UPDATE "KothApiScoreResults" score
   SET performance_rate = CASE
         WHEN ranked.core_rate <= 0.0 OR ranked.best_rate <= 0.0 THEN 0.0
         ELSE POWER(LEAST(1.0, ranked.core_rate / ranked.best_rate), 0.75)
       END,
       lead_credit = CASE
         WHEN ranked.best_rate > 0.0 AND ranked.leader_rank = 1 THEN 1.0
         ELSE 0.0
       END
  FROM ranked
 WHERE score.game_id = ranked.game_id
   AND score.challenge_id = ranked.challenge_id
   AND score.ad_round_id = ranked.ad_round_id
   AND score.participation_id = ranked.participation_id;

ALTER TABLE "KothApiScoreResults"
  ADD CONSTRAINT ck_koth_api_score_results_leaderboard CHECK (
    core_rate BETWEEN 0.0 AND 1.0
    AND performance_rate BETWEEN 0.0 AND 1.0
    AND lead_credit BETWEEN 0.0 AND 1.0
  ),
  ADD CONSTRAINT ck_koth_api_score_results_wave_budgets CHECK (
    activity_possible % 1000000 = 0
    AND objective_possible = objective_count::bigint * activity_possible
  );

CREATE UNIQUE INDEX uq_koth_api_score_results_crown
  ON "KothApiScoreResults"(game_id, challenge_id, ad_round_id)
  WHERE lead_credit = 1.0;

-- Materialized projections are formula-derived. Rebuild API games from their
-- immutable per-round evidence instead of mixing the former formula with the
-- finalized-wave formula.
DELETE FROM "KothEpochRollups" rollup
 WHERE EXISTS (
       SELECT 1
         FROM "KothOfficialConfigs" config,
              LATERAL jsonb_array_elements(config.hills_snapshot) hill
        WHERE config.game_id = rollup.game_id
          AND COALESCE(NULLIF(hill->>'claimSource', ''), 'Marker') = 'Api'
 );
"#;

const DOWN_SQL: &str = r#"
DELETE FROM "KothApiSnapshots";

DROP INDEX IF EXISTS uq_koth_api_score_results_crown;
ALTER TABLE "KothApiScoreResults"
  DROP CONSTRAINT ck_koth_api_score_results_leaderboard,
  DROP CONSTRAINT ck_koth_api_score_results_wave_budgets;

WITH tick_stats AS (
  SELECT game_id, challenge_id, ad_round_id,
         MAX(core_rate) AS highest_core,
         COUNT(*) FILTER (WHERE core_rate > 0.0) AS positive_teams
    FROM "KothApiScoreResults"
   GROUP BY game_id, challenge_id, ad_round_id
), leader_counts AS (
  SELECT score.game_id, score.challenge_id, score.ad_round_id,
         stats.highest_core, stats.positive_teams,
         COUNT(*) FILTER (WHERE score.core_rate = stats.highest_core) AS tied_leaders
    FROM "KothApiScoreResults" score
    JOIN tick_stats stats USING (game_id, challenge_id, ad_round_id)
   GROUP BY score.game_id, score.challenge_id, score.ad_round_id,
            stats.highest_core, stats.positive_teams
)
UPDATE "KothApiScoreResults" score
   SET performance_rate = score.core_rate,
       lead_credit = CASE
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
  ADD CONSTRAINT ck_koth_api_score_results_leaderboard CHECK (
    core_rate BETWEEN 0.0 AND 1.0
    AND performance_rate = core_rate
    AND lead_credit BETWEEN 0.0 AND 1.0
  );

DROP INDEX IF EXISTS uq_koth_api_snapshot_wave_crown;
ALTER TABLE "KothApiSnapshotScores"
  DROP CONSTRAINT fk_koth_api_snapshot_scores_wave,
  DROP CONSTRAINT "KothApiSnapshotScores_pkey",
  DROP COLUMN is_crown,
  DROP COLUMN wave_id,
  ADD CONSTRAINT "KothApiSnapshotScores_pkey"
    PRIMARY KEY (target_id, participation_id);
DROP TABLE "KothApiSnapshotWaves";
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
    fn wave_contract_is_bounded_unique_and_formula_constant() {
        assert!(UP_SQL.contains("CREATE TABLE \"KothApiSnapshotWaves\""));
        assert!(UP_SQL.contains("OCTET_LENGTH(wave_id) BETWEEN 1 AND 128"));
        assert!(UP_SQL.contains("uq_koth_api_snapshot_wave_crown"));
        assert!(UP_SQL.contains("POWER(LEAST(1.0, ranked.core_rate / ranked.best_rate), 0.75)"));
        assert!(UP_SQL.contains("WHEN activity_earned = activity_possible"));
        assert!(UP_SQL.contains("lead_credit BETWEEN 0.0 AND 1.0"));
        assert!(UP_SQL.contains("activity_possible % 1000000 = 0"));
        assert!(UP_SQL.contains("cannot adopt finalized-wave Leaderboard scoring"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn upgrades_the_real_schema_to_bounded_wave_evidence() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("rsctf_m0088_{}", uuid::Uuid::new_v4().simple());
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
        let db = SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone());

        Migrator::up(&db, Some(87)).await.unwrap();
        let before: (bool, i64) = sqlx::query_as(
            r#"SELECT to_regclass('"KothApiSnapshotWaves"') IS NOT NULL,
                      COUNT(*) FILTER (WHERE column_name IN ('wave_id', 'is_crown'))
                 FROM information_schema.columns
                WHERE table_schema = current_schema()
                  AND table_name = 'KothApiSnapshotScores'"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(before, (false, 0));

        // The fixture intentionally omits parent rows so it can isolate the
        // formula conversion. PostgreSQL's replica role disables only the FK
        // triggers for this insert; all check constraints remain active.
        let mut seed_connection = pool.acquire().await.unwrap();
        sqlx::query("SET session_replication_role = replica")
            .execute(&mut *seed_connection)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO "Participations"
                 (id, status, token, game_id, team_id, suspicion_score)
               VALUES
                 (11, 0, 'fixture-11', 7, 11, 0),
                 (12, 0, 'fixture-12', 7, 12, 0),
                 (13, 0, 'fixture-13', 7, 13, 0)"#,
        )
        .execute(&mut *seed_connection)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "KothControlResults"
                 (game_id, challenge_id, ad_round_id, marker_observed, status,
                  checked_at, token_window_attempt, is_scorable, void_reason)
               VALUES (7, 9, 51, false, 0, NOW(), 0, false, 'migration fixture')"#,
        )
        .execute(&mut *seed_connection)
        .await
        .unwrap();
        sqlx::raw_sql(
            r#"INSERT INTO "KothApiScoreResults" VALUES
                 (7,9,51,11,1,1,8,10,1,1.0,0.8,0.7,0.7,0.0),
                 (7,9,51,12,1,2,10,10,1,0.5,1.0,0.7,0.7,0.0),
                 (7,9,51,13,1,1,8,10,1,1.0,0.8,0.7,0.7,0.0)"#,
        )
        .execute(&mut *seed_connection)
        .await
        .unwrap();
        sqlx::query("SET session_replication_role = origin")
            .execute(&mut *seed_connection)
            .await
            .unwrap();
        drop(seed_connection);

        Migrator::up(&db, Some(1)).await.unwrap();
        let after: (bool, i64, i64, i64) = sqlx::query_as(
            r#"SELECT
                 to_regclass('"KothApiSnapshotWaves"') IS NOT NULL,
                 (SELECT COUNT(*) FROM information_schema.columns
                   WHERE table_schema = current_schema()
                     AND table_name = 'KothApiSnapshotScores'
                     AND column_name IN ('wave_id', 'is_crown')),
                 (SELECT COUNT(*) FROM information_schema.table_constraints
                   WHERE table_schema = current_schema()
                     AND table_name = 'KothApiScoreResults'
                     AND constraint_name = 'ck_koth_api_score_results_wave_budgets'),
                 (SELECT COUNT(*) FROM pg_indexes
                   WHERE schemaname = current_schema()
                     AND indexname IN (
                       'uq_koth_api_snapshot_wave_crown',
                       'uq_koth_api_score_results_crown'
                     ))"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(after, (true, 2, 1, 2));
        let converted = sqlx::query_as::<_, (i32, i64, i64, f64, f64, f64)>(
            r#"SELECT participation_id, activity_earned, activity_possible,
                      core_rate, performance_rate, lead_credit
                 FROM "KothApiScoreResults"
                ORDER BY participation_id"#,
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            converted,
            vec![
                (11, 1_000_000, 1_000_000, 0.8, 1.0, 1.0),
                (12, 0, 1_000_000, 0.0, 0.0, 0.0),
                (13, 1_000_000, 1_000_000, 0.8, 1.0, 0.0),
            ]
        );

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
