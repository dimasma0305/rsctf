//! Official epoch-based A&D scoring configuration and evidence snapshots.

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
                  ADD COLUMN IF NOT EXISTS ad_epoch_ticks INTEGER NOT NULL DEFAULT 8,
                  ADD COLUMN IF NOT EXISTS ad_scoring_start_round INTEGER NULL;
                ALTER TABLE "Games" DROP COLUMN IF EXISTS ad_scoring_mode;
                UPDATE "Games"
                   SET ad_epoch_ticks = LEAST(
                     10000, GREATEST(1, COALESCE(ad_epoch_ticks, 8))
                   )
                 WHERE ad_epoch_ticks IS NULL
                    OR ad_epoch_ticks < 1 OR ad_epoch_ticks > 10000;
                UPDATE "Games" SET ad_scoring_start_round = NULL
                 WHERE ad_scoring_start_round < 1;
                ALTER TABLE "Games"
                  ALTER COLUMN ad_epoch_ticks SET DEFAULT 8,
                  ALTER COLUMN ad_epoch_ticks SET NOT NULL;

                ALTER TABLE "AdFlags"
                  ADD COLUMN IF NOT EXISTS checker_qualified BOOLEAN NOT NULL DEFAULT FALSE,
                  ADD COLUMN IF NOT EXISTS service_weight DOUBLE PRECISION NOT NULL DEFAULT 1.0;
                UPDATE "AdFlags"
                   SET checker_qualified = COALESCE(checker_qualified, FALSE),
                       service_weight = LEAST(1.2, GREATEST(0.8, COALESCE(service_weight, 1.0)))
                 WHERE checker_qualified IS NULL OR service_weight IS NULL
                    OR service_weight < 0.8 OR service_weight > 1.2;
                ALTER TABLE "AdFlags"
                  ALTER COLUMN checker_qualified SET DEFAULT FALSE,
                  ALTER COLUMN checker_qualified SET NOT NULL,
                  ALTER COLUMN service_weight SET DEFAULT 1.0,
                  ALTER COLUMN service_weight SET NOT NULL;

                ALTER TABLE "GameChallenges"
                  ADD COLUMN IF NOT EXISTS ad_scoring_weight DOUBLE PRECISION NOT NULL DEFAULT 1.0;
                UPDATE "GameChallenges"
                   SET ad_scoring_weight = LEAST(
                     1.2, GREATEST(0.8, COALESCE(ad_scoring_weight, 1.0))
                   )
                 WHERE ad_scoring_weight IS NULL
                    OR ad_scoring_weight < 0.8 OR ad_scoring_weight > 1.2;
                ALTER TABLE "GameChallenges"
                  ALTER COLUMN ad_scoring_weight SET DEFAULT 1.0,
                  ALTER COLUMN ad_scoring_weight SET NOT NULL;

                ALTER TABLE "AdCheckResults"
                  ADD COLUMN IF NOT EXISTS flag_verified BOOLEAN NOT NULL DEFAULT FALSE;
                UPDATE "AdCheckResults" SET flag_verified = FALSE WHERE flag_verified IS NULL;
                ALTER TABLE "AdCheckResults"
                  ALTER COLUMN flag_verified SET DEFAULT FALSE,
                  ALTER COLUMN flag_verified SET NOT NULL;

                -- The engine treats one team/challenge service as one scoring
                -- identity. Remove invalid legacy duplicates and their orphan-prone
                -- evidence before enforcing that invariant in PostgreSQL.
                LOCK TABLE "AdTeamServices", "AdFlags", "AdAttacks", "AdCheckResults"
                  IN SHARE ROW EXCLUSIVE MODE;
                CREATE TEMP TABLE duplicate_ad_services ON COMMIT DROP AS
                SELECT id
                  FROM (
                    SELECT id, row_number() OVER (
                             PARTITION BY participation_id, challenge_id
                             ORDER BY (container_id IS NOT NULL) DESC, id
                           ) AS duplicate_number
                      FROM "AdTeamServices"
                  ) ranked
                 WHERE duplicate_number > 1;
                DELETE FROM "AdAttacks" attack
                 USING "AdFlags" flag, duplicate_ad_services duplicate
                 WHERE attack.flag_id = flag.id
                   AND flag.team_service_id = duplicate.id;
                DELETE FROM "AdAttacks" attack
                 USING duplicate_ad_services duplicate
                 WHERE attack.victim_team_service_id = duplicate.id;
                DELETE FROM "AdCheckResults" result
                 USING duplicate_ad_services duplicate
                 WHERE result.team_service_id = duplicate.id;
                DELETE FROM "AdFlags" flag
                 USING duplicate_ad_services duplicate
                 WHERE flag.team_service_id = duplicate.id;
                DELETE FROM "AdTeamServices" service
                 USING duplicate_ad_services duplicate
                 WHERE service.id = duplicate.id;
                DROP INDEX IF EXISTS ix_adteamservices_part_challenge;
                CREATE UNIQUE INDEX IF NOT EXISTS ux_adteamservices_part_challenge
                  ON "AdTeamServices"(participation_id, challenge_id);

                DO $$
                BEGIN
                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'ck_games_ad_epoch_ticks'
                       AND conrelid = '"Games"'::regclass
                  ) THEN
                    ALTER TABLE "Games" ADD CONSTRAINT ck_games_ad_epoch_ticks
                      CHECK (ad_epoch_ticks >= 1 AND ad_epoch_ticks <= 10000);
                  END IF;
                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'ck_games_ad_scoring_start_round'
                       AND conrelid = '"Games"'::regclass
                  ) THEN
                    ALTER TABLE "Games" ADD CONSTRAINT ck_games_ad_scoring_start_round
                      CHECK (ad_scoring_start_round IS NULL OR ad_scoring_start_round >= 1);
                  END IF;
                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'ck_adflags_service_weight'
                       AND conrelid = '"AdFlags"'::regclass
                  ) THEN
                    ALTER TABLE "AdFlags" ADD CONSTRAINT ck_adflags_service_weight
                      CHECK (service_weight >= 0.8 AND service_weight <= 1.2);
                  END IF;
                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'ck_gamechallenges_ad_scoring_weight'
                       AND conrelid = '"GameChallenges"'::regclass
                  ) THEN
                    ALTER TABLE "GameChallenges"
                      ADD CONSTRAINT ck_gamechallenges_ad_scoring_weight
                      CHECK (ad_scoring_weight >= 0.8 AND ad_scoring_weight <= 1.2);
                  END IF;
                END
                $$;
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
                ALTER TABLE "Games"
                  DROP COLUMN IF EXISTS ad_scoring_start_round,
                  DROP COLUMN IF EXISTS ad_epoch_ticks;
                ALTER TABLE "AdFlags"
                  DROP COLUMN IF EXISTS service_weight,
                  DROP COLUMN IF EXISTS checker_qualified;
                ALTER TABLE "GameChallenges"
                  DROP COLUMN IF EXISTS ad_scoring_weight;
                ALTER TABLE "AdCheckResults"
                  DROP COLUMN IF EXISTS flag_verified;
                DROP INDEX IF EXISTS ux_adteamservices_part_challenge;
                CREATE INDEX IF NOT EXISTS ix_adteamservices_part_challenge
                  ON "AdTeamServices"(participation_id, challenge_id);
                "#,
            )
            .await?;
        Ok(())
    }
}
