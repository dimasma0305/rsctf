//! Durable, bounded official A&D epoch score rollups.

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
                -- A live epoch is recomputed from raw evidence. Keep that bounded;
                -- completed epochs are persisted in the rollup tables below.
                UPDATE "Games"
                   SET ad_epoch_ticks = LEAST(64, GREATEST(1, ad_epoch_ticks));
                ALTER TABLE "Games" DROP CONSTRAINT IF EXISTS ck_games_ad_epoch_ticks;
                ALTER TABLE "Games" ADD CONSTRAINT ck_games_ad_epoch_ticks
                  CHECK (ad_epoch_ticks >= 1 AND ad_epoch_ticks <= 64);
                UPDATE "Games"
                   SET ad_flag_lifetime_ticks = LEAST(
                     50, GREATEST(1, ad_flag_lifetime_ticks)
                   )
                 WHERE ad_flag_lifetime_ticks IS NOT NULL
                   AND (ad_flag_lifetime_ticks < 1 OR ad_flag_lifetime_ticks > 50);
                ALTER TABLE "Games"
                  DROP CONSTRAINT IF EXISTS ck_games_ad_flag_lifetime_ticks;
                ALTER TABLE "Games" ADD CONSTRAINT ck_games_ad_flag_lifetime_ticks
                  CHECK (
                    ad_flag_lifetime_ticks IS NULL
                    OR ad_flag_lifetime_ticks BETWEEN 1 AND 50
                  );

                -- Before atomic round preparation, completed checker rows did
                -- not populate sla_credit. A finalized round cannot still have
                -- an in-flight checker, so mark those legacy rows complete; the
                -- official scorer derives normalized credit from status rather
                -- than trusting this legacy field's numeric value.
                UPDATE "AdCheckResults" result SET sla_credit = 0.0
                  FROM "AdRounds" round
                 WHERE round.id = result.round_id
                   AND round.finalized = TRUE
                   AND result.sla_credit IS NULL;

                CREATE TABLE IF NOT EXISTS "AdEpochRollups" (
                  game_id INTEGER NOT NULL REFERENCES "Games"(id) ON DELETE CASCADE,
                  epoch INTEGER NOT NULL,
                  start_round INTEGER NOT NULL,
                  end_round INTEGER NOT NULL,
                  round_count INTEGER NOT NULL,
                  epoch_weight DOUBLE PRECISION NOT NULL,
                  finalized_round INTEGER NOT NULL,
                  eligible_flags BIGINT NOT NULL,
                  captured_flags BIGINT NOT NULL,
                  accepted_captures BIGINT NOT NULL,
                  defense_opportunities BIGINT NOT NULL,
                  protected_opportunities BIGINT NOT NULL,
                  cumulative_eligible_flags BIGINT NOT NULL,
                  cumulative_captured_flags BIGINT NOT NULL,
                  cumulative_accepted_captures BIGINT NOT NULL,
                  cumulative_defense_opportunities BIGINT NOT NULL,
                  cumulative_protected_opportunities BIGINT NOT NULL,
                  created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                  PRIMARY KEY (game_id, epoch),
                  CONSTRAINT ck_ad_epoch_rollups_epoch CHECK (epoch >= 1),
                  CONSTRAINT ck_ad_epoch_rollups_rounds CHECK (
                    start_round >= 1 AND end_round >= start_round
                    AND round_count >= 1 AND finalized_round >= end_round
                  ),
                  CONSTRAINT ck_ad_epoch_rollups_weight CHECK (
                    epoch_weight > 0.0 AND epoch_weight <= 1.0
                  ),
                  CONSTRAINT ck_ad_epoch_rollups_counts CHECK (
                    eligible_flags >= 0 AND captured_flags >= 0
                    AND accepted_captures >= 0 AND defense_opportunities >= 0
                    AND protected_opportunities >= 0
                    AND cumulative_eligible_flags >= eligible_flags
                    AND cumulative_captured_flags >= captured_flags
                    AND cumulative_accepted_captures >= accepted_captures
                    AND cumulative_defense_opportunities >= defense_opportunities
                    AND cumulative_protected_opportunities >= protected_opportunities
                  )
                );

                CREATE TABLE IF NOT EXISTS "AdEpochServiceRollups" (
                  game_id INTEGER NOT NULL,
                  epoch INTEGER NOT NULL,
                  participation_id INTEGER NOT NULL REFERENCES "Participations"(id) ON DELETE CASCADE,
                  challenge_id INTEGER NOT NULL REFERENCES "GameChallenges"(id) ON DELETE CASCADE,
                  service_weight DOUBLE PRECISION NOT NULL,
                  opportunity_count BIGINT NOT NULL,
                  capture_count BIGINT NOT NULL,
                  rarity_sum DOUBLE PRECISION NOT NULL,
                  defense_opportunity_count BIGINT NOT NULL,
                  protected_opportunity_count BIGINT NOT NULL,
                  sla_credit_sum DOUBLE PRECISION NOT NULL,
                  sla_tick_count BIGINT NOT NULL,
                  closing_sla_status SMALLINT NULL,
                  closing_sla_credit DOUBLE PRECISION NULL,
                  PRIMARY KEY (game_id, epoch, participation_id, challenge_id),
                  FOREIGN KEY (game_id, epoch) REFERENCES "AdEpochRollups"(game_id, epoch)
                    ON DELETE CASCADE,
                  CONSTRAINT ck_ad_epoch_service_rollups_weight CHECK (
                    service_weight >= 0.8 AND service_weight <= 1.2
                  ),
                  CONSTRAINT ck_ad_epoch_service_rollups_counts CHECK (
                    opportunity_count >= 0 AND capture_count >= 0
                    AND rarity_sum >= 0.0 AND defense_opportunity_count >= 0
                    AND protected_opportunity_count >= 0
                    AND sla_credit_sum >= 0.0 AND sla_tick_count >= 0
                  ),
                  CONSTRAINT ck_ad_epoch_service_rollups_sla CHECK (
                    closing_sla_credit IS NULL
                    OR (closing_sla_credit >= 0.0 AND closing_sla_credit <= 1.0)
                  )
                );
                CREATE INDEX IF NOT EXISTS ix_ad_epoch_service_rollups_seed
                  ON "AdEpochServiceRollups"
                    (game_id, participation_id, challenge_id, epoch DESC);

                CREATE TABLE IF NOT EXISTS "AdEpochTeamRollups" (
                  game_id INTEGER NOT NULL,
                  epoch INTEGER NOT NULL,
                  participation_id INTEGER NOT NULL REFERENCES "Participations"(id) ON DELETE CASCADE,
                  points DOUBLE PRECISION NOT NULL,
                  epoch_weight DOUBLE PRECISION NOT NULL,
                  cumulative_points_numerator DOUBLE PRECISION NOT NULL,
                  cumulative_epoch_weight DOUBLE PRECISION NOT NULL,
                  cumulative_offense_numerator DOUBLE PRECISION NOT NULL,
                  cumulative_defense_numerator DOUBLE PRECISION NOT NULL,
                  cumulative_sla_numerator DOUBLE PRECISION NOT NULL,
                  cumulative_rate_weight DOUBLE PRECISION NOT NULL,
                  PRIMARY KEY (game_id, epoch, participation_id),
                  FOREIGN KEY (game_id, epoch) REFERENCES "AdEpochRollups"(game_id, epoch)
                    ON DELETE CASCADE,
                  CONSTRAINT ck_ad_epoch_team_rollups_values CHECK (
                    points >= 0.0 AND points <= 100.0
                    AND epoch_weight > 0.0 AND epoch_weight <= 1.0
                    AND cumulative_points_numerator >= 0.0
                    AND cumulative_epoch_weight > 0.0
                    AND cumulative_offense_numerator >= 0.0
                    AND cumulative_defense_numerator >= 0.0
                    AND cumulative_sla_numerator >= 0.0
                    AND cumulative_rate_weight > 0.0
                  )
                );
                CREATE INDEX IF NOT EXISTS ix_ad_epoch_team_rollups_latest
                  ON "AdEpochTeamRollups" (game_id, participation_id, epoch DESC);
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
                DROP TABLE IF EXISTS "AdEpochTeamRollups";
                DROP TABLE IF EXISTS "AdEpochServiceRollups";
                DROP TABLE IF EXISTS "AdEpochRollups";
                ALTER TABLE "Games"
                  DROP CONSTRAINT IF EXISTS ck_games_ad_flag_lifetime_ticks;
                ALTER TABLE "Games" DROP CONSTRAINT IF EXISTS ck_games_ad_epoch_ticks;
                ALTER TABLE "Games" ADD CONSTRAINT ck_games_ad_epoch_ticks
                  CHECK (ad_epoch_ticks >= 1 AND ad_epoch_ticks <= 10000);
                "#,
            )
            .await?;
        Ok(())
    }
}
