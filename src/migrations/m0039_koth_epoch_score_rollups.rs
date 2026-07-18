//! Durable, versioned KotH epoch, team, and hill score rollups.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                ALTER TABLE "Games"
                  ADD COLUMN IF NOT EXISTS koth_scoring_formula_version
                    SMALLINT NOT NULL DEFAULT 1;
                ALTER TABLE "Games"
                  DROP CONSTRAINT IF EXISTS ck_games_koth_scoring_formula_version;
                ALTER TABLE "Games"
                  ADD CONSTRAINT ck_games_koth_scoring_formula_version
                    CHECK (koth_scoring_formula_version >= 1);

                CREATE TABLE IF NOT EXISTS "KothEpochRollups" (
                  game_id INTEGER NOT NULL REFERENCES "Games"(id) ON DELETE CASCADE,
                  formula_version SMALLINT NOT NULL,
                  epoch INTEGER NOT NULL,
                  start_round INTEGER NOT NULL,
                  end_round INTEGER NOT NULL,
                  round_count INTEGER NOT NULL,
                  epoch_weight DOUBLE PRECISION NOT NULL,
                  finalized_round INTEGER NOT NULL,
                  evidence_finalized_at TIMESTAMPTZ NOT NULL,
                  scorable_ticks BIGINT NOT NULL,
                  eligible_windows BIGINT NOT NULL,
                  cumulative_scorable_ticks BIGINT NOT NULL,
                  cumulative_eligible_windows BIGINT NOT NULL,
                  created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                  PRIMARY KEY (game_id, formula_version, epoch),
                  CONSTRAINT ck_koth_epoch_rollups_identity CHECK (
                    formula_version >= 1 AND epoch >= 1
                  ),
                  CONSTRAINT ck_koth_epoch_rollups_rounds CHECK (
                    start_round >= 1 AND end_round >= start_round
                    AND round_count >= 1 AND finalized_round >= end_round
                  ),
                  CONSTRAINT ck_koth_epoch_rollups_weight CHECK (
                    epoch_weight >= 0.0 AND epoch_weight <= 1.0
                  ),
                  CONSTRAINT ck_koth_epoch_rollups_counts CHECK (
                    scorable_ticks >= 0 AND eligible_windows >= 0
                    AND cumulative_scorable_ticks >= scorable_ticks
                    AND cumulative_eligible_windows >= eligible_windows
                  )
                );

                CREATE TABLE IF NOT EXISTS "KothEpochTeamRollups" (
                  game_id INTEGER NOT NULL,
                  formula_version SMALLINT NOT NULL,
                  epoch INTEGER NOT NULL,
                  participation_id INTEGER NOT NULL
                    REFERENCES "Participations"(id) ON DELETE CASCADE,
                  points DOUBLE PRECISION NOT NULL,
                  epoch_weight DOUBLE PRECISION NOT NULL,
                  acquisition_rate DOUBLE PRECISION NOT NULL,
                  control_rate DOUBLE PRECISION NOT NULL,
                  sla_rate DOUBLE PRECISION NOT NULL,
                  acquisition_windows BIGINT NOT NULL,
                  controlled_ticks BIGINT NOT NULL,
                  responsible_ticks BIGINT NOT NULL,
                  healthy_responsible_ticks BIGINT NOT NULL,
                  cumulative_points_numerator DOUBLE PRECISION NOT NULL,
                  cumulative_epoch_weight DOUBLE PRECISION NOT NULL,
                  cumulative_acquisition_numerator DOUBLE PRECISION NOT NULL,
                  cumulative_control_numerator DOUBLE PRECISION NOT NULL,
                  cumulative_sla_numerator DOUBLE PRECISION NOT NULL,
                  cumulative_rate_weight DOUBLE PRECISION NOT NULL,
                  cumulative_acquisition_windows BIGINT NOT NULL,
                  cumulative_controlled_ticks BIGINT NOT NULL,
                  cumulative_responsible_ticks BIGINT NOT NULL,
                  cumulative_healthy_responsible_ticks BIGINT NOT NULL,
                  PRIMARY KEY (game_id, formula_version, epoch, participation_id),
                  FOREIGN KEY (game_id, formula_version, epoch)
                    REFERENCES "KothEpochRollups"(game_id, formula_version, epoch)
                    ON DELETE CASCADE,
                  CONSTRAINT ck_koth_epoch_team_rollups_rates CHECK (
                    points BETWEEN 0.0 AND 100.0
                    AND epoch_weight BETWEEN 0.0 AND 1.0
                    AND acquisition_rate BETWEEN 0.0 AND 1.0
                    AND control_rate BETWEEN 0.0 AND 1.0
                    AND sla_rate BETWEEN 0.0 AND 1.0
                  ),
                  CONSTRAINT ck_koth_epoch_team_rollups_cumulative CHECK (
                    cumulative_points_numerator >= 0.0
                    AND cumulative_epoch_weight >= 0.0
                    AND cumulative_points_numerator
                          <= 100.0 * cumulative_epoch_weight + 0.000000001
                    AND cumulative_acquisition_numerator >= 0.0
                    AND cumulative_control_numerator >= 0.0
                    AND cumulative_sla_numerator >= 0.0
                    AND cumulative_rate_weight >= 0.0
                    AND cumulative_acquisition_numerator
                          <= cumulative_rate_weight + 0.000000001
                    AND cumulative_control_numerator
                          <= cumulative_rate_weight + 0.000000001
                    AND cumulative_sla_numerator
                          <= cumulative_rate_weight + 0.000000001
                  ),
                  CONSTRAINT ck_koth_epoch_team_rollups_counts CHECK (
                    acquisition_windows >= 0 AND controlled_ticks >= 0
                    AND responsible_ticks >= 0 AND healthy_responsible_ticks >= 0
                    AND healthy_responsible_ticks <= responsible_ticks
                    AND cumulative_acquisition_windows >= acquisition_windows
                    AND cumulative_controlled_ticks >= controlled_ticks
                    AND cumulative_responsible_ticks >= responsible_ticks
                    AND cumulative_healthy_responsible_ticks >= healthy_responsible_ticks
                    AND cumulative_healthy_responsible_ticks
                          <= cumulative_responsible_ticks
                  )
                );
                CREATE INDEX IF NOT EXISTS ix_koth_epoch_team_rollups_latest
                  ON "KothEpochTeamRollups"
                    (game_id, formula_version, participation_id, epoch DESC);

                CREATE TABLE IF NOT EXISTS "KothEpochHillRollups" (
                  game_id INTEGER NOT NULL,
                  formula_version SMALLINT NOT NULL,
                  epoch INTEGER NOT NULL,
                  participation_id INTEGER NOT NULL
                    REFERENCES "Participations"(id) ON DELETE CASCADE,
                  challenge_id INTEGER NOT NULL
                    REFERENCES "GameChallenges"(id) ON DELETE CASCADE,
                  service_weight DOUBLE PRECISION NOT NULL,
                  evidence_fraction DOUBLE PRECISION NOT NULL,
                  epoch_fraction DOUBLE PRECISION NOT NULL,
                  local_points DOUBLE PRECISION NOT NULL,
                  acquisition_rate DOUBLE PRECISION NOT NULL,
                  control_rate DOUBLE PRECISION NOT NULL,
                  sla_rate DOUBLE PRECISION NOT NULL,
                  acquisition_windows BIGINT NOT NULL,
                  controlled_ticks BIGINT NOT NULL,
                  responsible_ticks BIGINT NOT NULL,
                  healthy_responsible_ticks BIGINT NOT NULL,
                  cumulative_points_numerator DOUBLE PRECISION NOT NULL,
                  cumulative_score_weight DOUBLE PRECISION NOT NULL,
                  cumulative_acquisition_numerator DOUBLE PRECISION NOT NULL,
                  cumulative_control_numerator DOUBLE PRECISION NOT NULL,
                  cumulative_sla_numerator DOUBLE PRECISION NOT NULL,
                  cumulative_rate_weight DOUBLE PRECISION NOT NULL,
                  cumulative_acquisition_windows BIGINT NOT NULL,
                  cumulative_controlled_ticks BIGINT NOT NULL,
                  cumulative_responsible_ticks BIGINT NOT NULL,
                  cumulative_healthy_responsible_ticks BIGINT NOT NULL,
                  PRIMARY KEY (
                    game_id, formula_version, epoch, participation_id, challenge_id
                  ),
                  FOREIGN KEY (game_id, formula_version, epoch)
                    REFERENCES "KothEpochRollups"(game_id, formula_version, epoch)
                    ON DELETE CASCADE,
                  CONSTRAINT ck_koth_epoch_hill_rollups_fractions CHECK (
                    service_weight BETWEEN 0.8 AND 1.2
                    AND evidence_fraction BETWEEN 0.0 AND 1.0
                    AND epoch_fraction BETWEEN 0.0 AND 1.0
                    AND local_points BETWEEN 0.0 AND 100.0
                    AND acquisition_rate BETWEEN 0.0 AND 1.0
                    AND control_rate BETWEEN 0.0 AND 1.0
                    AND sla_rate BETWEEN 0.0 AND 1.0
                  ),
                  CONSTRAINT ck_koth_epoch_hill_rollups_cumulative CHECK (
                    cumulative_points_numerator >= 0.0
                    AND cumulative_score_weight >= 0.0
                    AND cumulative_points_numerator
                          <= 100.0 * cumulative_score_weight + 0.000000001
                    AND cumulative_acquisition_numerator >= 0.0
                    AND cumulative_control_numerator >= 0.0
                    AND cumulative_sla_numerator >= 0.0
                    AND cumulative_rate_weight >= 0.0
                    AND cumulative_acquisition_numerator
                          <= cumulative_rate_weight + 0.000000001
                    AND cumulative_control_numerator
                          <= cumulative_rate_weight + 0.000000001
                    AND cumulative_sla_numerator
                          <= cumulative_rate_weight + 0.000000001
                  ),
                  CONSTRAINT ck_koth_epoch_hill_rollups_counts CHECK (
                    acquisition_windows >= 0 AND controlled_ticks >= 0
                    AND responsible_ticks >= 0 AND healthy_responsible_ticks >= 0
                    AND healthy_responsible_ticks <= responsible_ticks
                    AND cumulative_acquisition_windows >= acquisition_windows
                    AND cumulative_controlled_ticks >= controlled_ticks
                    AND cumulative_responsible_ticks >= responsible_ticks
                    AND cumulative_healthy_responsible_ticks >= healthy_responsible_ticks
                    AND cumulative_healthy_responsible_ticks
                          <= cumulative_responsible_ticks
                  )
                );
                CREATE INDEX IF NOT EXISTS ix_koth_epoch_hill_rollups_latest
                  ON "KothEpochHillRollups"
                    (game_id, formula_version, participation_id, challenge_id, epoch DESC);
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                DROP TABLE IF EXISTS "KothEpochHillRollups";
                DROP TABLE IF EXISTS "KothEpochTeamRollups";
                DROP TABLE IF EXISTS "KothEpochRollups";
                ALTER TABLE "Games"
                  DROP CONSTRAINT IF EXISTS ck_games_koth_scoring_formula_version,
                  DROP COLUMN IF EXISTS koth_scoring_formula_version;
                "#,
            )
            .await?;
        Ok(())
    }
}
