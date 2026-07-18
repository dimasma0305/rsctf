//! Adds bounded cumulative per-service score detail to official A&D rollups.

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
                ALTER TABLE "AdEpochServiceRollups"
                  ADD COLUMN IF NOT EXISTS local_points DOUBLE PRECISION NOT NULL DEFAULT 0.0,
                  ADD COLUMN IF NOT EXISTS offense_rate DOUBLE PRECISION NOT NULL DEFAULT 0.0,
                  ADD COLUMN IF NOT EXISTS defense_rate DOUBLE PRECISION NOT NULL DEFAULT 0.0,
                  ADD COLUMN IF NOT EXISTS sla_rate DOUBLE PRECISION NOT NULL DEFAULT 0.0,
                  ADD COLUMN IF NOT EXISTS cumulative_points_numerator
                    DOUBLE PRECISION NOT NULL DEFAULT 0.0,
                  ADD COLUMN IF NOT EXISTS cumulative_epoch_weight
                    DOUBLE PRECISION NOT NULL DEFAULT 0.0,
                  ADD COLUMN IF NOT EXISTS cumulative_offense_numerator
                    DOUBLE PRECISION NOT NULL DEFAULT 0.0,
                  ADD COLUMN IF NOT EXISTS cumulative_defense_numerator
                    DOUBLE PRECISION NOT NULL DEFAULT 0.0,
                  ADD COLUMN IF NOT EXISTS cumulative_sla_numerator
                    DOUBLE PRECISION NOT NULL DEFAULT 0.0,
                  ADD COLUMN IF NOT EXISTS cumulative_capture_count BIGINT NOT NULL DEFAULT 0;

                -- Recompute rather than copying the team score: the per-service
                -- score is nonlinear and team rows normalize across services.
                WITH rates AS (
                  SELECT service.game_id, service.epoch,
                         service.participation_id, service.challenge_id,
                         header.epoch_weight, service.service_weight,
                         LEAST(1.0, GREATEST(0.0,
                           CASE WHEN service.opportunity_count > 0 THEN
                             service.capture_count::float8
                               / service.opportunity_count::float8
                             + 0.25 * service.rarity_sum
                               / service.opportunity_count::float8
                           ELSE 0.0 END
                         )) AS offense_rate,
                         LEAST(1.0, GREATEST(0.0,
                           CASE WHEN service.defense_opportunity_count > 0 THEN
                             service.protected_opportunity_count::float8
                               / service.defense_opportunity_count::float8
                           ELSE 0.0 END
                         )) AS defense_rate,
                         LEAST(1.0, GREATEST(0.0,
                           CASE WHEN service.sla_tick_count > 0 THEN
                             service.sla_credit_sum
                               / service.sla_tick_count::float8
                           ELSE 0.0 END
                         )) AS sla_rate,
                         service.capture_count
                    FROM "AdEpochServiceRollups" service
                    JOIN "AdEpochRollups" header
                      ON header.game_id = service.game_id
                     AND header.epoch = service.epoch
                ), local_scores AS (
                  SELECT rates.*,
                         LEAST(100.0, GREATEST(0.0,
                           100.0 * sla_rate * LEAST(1.0, GREATEST(0.0,
                             0.4 * offense_rate + 0.4 * defense_rate
                             + 0.2 * SQRT(offense_rate * defense_rate)
                           ))
                         )) AS local_points
                    FROM rates
                ), scored AS (
                  SELECT local_scores.*,
                         local_points * service_weight
                           / SUM(service_weight) OVER (
                               PARTITION BY game_id, epoch, participation_id
                             ) AS point_contribution
                    FROM local_scores
                ), cumulative AS (
                  SELECT scored.*,
                         SUM(point_contribution * epoch_weight) OVER service_history
                           AS cumulative_points_numerator,
                         SUM(epoch_weight) OVER service_history
                           AS cumulative_epoch_weight,
                         SUM(offense_rate * epoch_weight) OVER service_history
                           AS cumulative_offense_numerator,
                         SUM(defense_rate * epoch_weight) OVER service_history
                           AS cumulative_defense_numerator,
                         SUM(sla_rate * epoch_weight) OVER service_history
                           AS cumulative_sla_numerator,
                         (SUM(capture_count) OVER service_history)::bigint
                           AS cumulative_capture_count
                    FROM scored
                  WINDOW service_history AS (
                    PARTITION BY game_id, participation_id, challenge_id
                    ORDER BY epoch ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW
                  )
                )
                UPDATE "AdEpochServiceRollups" service
                   SET local_points = cumulative.local_points,
                       offense_rate = cumulative.offense_rate,
                       defense_rate = cumulative.defense_rate,
                       sla_rate = cumulative.sla_rate,
                       cumulative_points_numerator =
                         cumulative.cumulative_points_numerator,
                       cumulative_epoch_weight = cumulative.cumulative_epoch_weight,
                       cumulative_offense_numerator =
                         cumulative.cumulative_offense_numerator,
                       cumulative_defense_numerator =
                         cumulative.cumulative_defense_numerator,
                       cumulative_sla_numerator = cumulative.cumulative_sla_numerator,
                       cumulative_capture_count = cumulative.cumulative_capture_count
                  FROM cumulative
                 WHERE service.game_id = cumulative.game_id
                   AND service.epoch = cumulative.epoch
                   AND service.participation_id = cumulative.participation_id
                   AND service.challenge_id = cumulative.challenge_id;

                ALTER TABLE "AdEpochServiceRollups"
                  ALTER COLUMN local_points DROP DEFAULT,
                  ALTER COLUMN offense_rate DROP DEFAULT,
                  ALTER COLUMN defense_rate DROP DEFAULT,
                  ALTER COLUMN sla_rate DROP DEFAULT,
                  ALTER COLUMN cumulative_points_numerator DROP DEFAULT,
                  ALTER COLUMN cumulative_epoch_weight DROP DEFAULT,
                  ALTER COLUMN cumulative_offense_numerator DROP DEFAULT,
                  ALTER COLUMN cumulative_defense_numerator DROP DEFAULT,
                  ALTER COLUMN cumulative_sla_numerator DROP DEFAULT,
                  ALTER COLUMN cumulative_capture_count DROP DEFAULT;

                ALTER TABLE "AdEpochServiceRollups"
                  DROP CONSTRAINT IF EXISTS ck_ad_epoch_service_rollups_scores;
                ALTER TABLE "AdEpochServiceRollups"
                  ADD CONSTRAINT ck_ad_epoch_service_rollups_scores CHECK (
                    local_points BETWEEN 0.0 AND 100.0
                    AND offense_rate BETWEEN 0.0 AND 1.0
                    AND defense_rate BETWEEN 0.0 AND 1.0
                    AND sla_rate BETWEEN 0.0 AND 1.0
                    AND cumulative_epoch_weight > 0.0
                    AND cumulative_points_numerator >= 0.0
                    AND cumulative_points_numerator
                          <= 100.0 * cumulative_epoch_weight + 0.000000001
                    AND cumulative_offense_numerator >= 0.0
                    AND cumulative_offense_numerator
                          <= cumulative_epoch_weight + 0.000000001
                    AND cumulative_defense_numerator >= 0.0
                    AND cumulative_defense_numerator
                          <= cumulative_epoch_weight + 0.000000001
                    AND cumulative_sla_numerator >= 0.0
                    AND cumulative_sla_numerator
                          <= cumulative_epoch_weight + 0.000000001
                    AND cumulative_capture_count >= capture_count
                  );
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
                ALTER TABLE "AdEpochServiceRollups"
                  DROP CONSTRAINT IF EXISTS ck_ad_epoch_service_rollups_scores,
                  DROP COLUMN IF EXISTS cumulative_capture_count,
                  DROP COLUMN IF EXISTS cumulative_sla_numerator,
                  DROP COLUMN IF EXISTS cumulative_defense_numerator,
                  DROP COLUMN IF EXISTS cumulative_offense_numerator,
                  DROP COLUMN IF EXISTS cumulative_epoch_weight,
                  DROP COLUMN IF EXISTS cumulative_points_numerator,
                  DROP COLUMN IF EXISTS sla_rate,
                  DROP COLUMN IF EXISTS defense_rate,
                  DROP COLUMN IF EXISTS offense_rate,
                  DROP COLUMN IF EXISTS local_points;
                "#,
            )
            .await?;
        Ok(())
    }
}
