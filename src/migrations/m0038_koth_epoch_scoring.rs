//! Adds durable KotH responsibility evidence for bounded epoch scoring.

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
                ALTER TABLE "KothControlResults"
                  ADD COLUMN IF NOT EXISTS responsible_participation_id INTEGER NULL,
                  ADD COLUMN IF NOT EXISTS marker_observed BOOLEAN NOT NULL DEFAULT TRUE;
                ALTER TABLE "Games"
                  ADD COLUMN IF NOT EXISTS koth_scoring_start_round INTEGER NULL;

                UPDATE "KothControlResults"
                   SET marker_observed = TRUE
                 WHERE marker_observed IS NULL;
                ALTER TABLE "KothControlResults"
                  ALTER COLUMN marker_observed SET DEFAULT TRUE,
                  ALTER COLUMN marker_observed SET NOT NULL;

                -- Best-effort legacy backfill: carry the most recently observed
                -- controller forward on the same hill, but never across a refresh
                -- boundary. Historical cadence changes made before this column
                -- existed were not snapshotted, so they cannot be reconstructed;
                -- all legacy windows use the game's current configured cadence.
                WITH game_cadence AS (
                  SELECT id AS game_id,
                         GREATEST(COALESCE(koth_refresh_ticks, 5), 1) AS refresh_ticks
                    FROM "Games"
                ),
                inferred AS (
                  SELECT result.id,
                         (
                           SELECT prior.controlling_participation_id
                             FROM "KothControlResults" prior
                             JOIN "AdRounds" prior_round
                               ON prior_round.id = prior.ad_round_id
                              AND prior_round.game_id = prior.game_id
                             JOIN "Participations" participation
                               ON participation.id = prior.controlling_participation_id
                              AND participation.game_id = prior.game_id
                            WHERE prior.game_id = result.game_id
                              AND prior.challenge_id = result.challenge_id
                              AND prior.controlling_participation_id IS NOT NULL
                              AND prior_round.number BETWEEN
                                  ((result_round.number - 1) / cadence.refresh_ticks)
                                    * cadence.refresh_ticks + 1
                                  AND result_round.number
                            ORDER BY prior_round.number DESC, prior.id DESC
                            LIMIT 1
                         ) AS responsible_participation_id
                    FROM "KothControlResults" result
                    JOIN "AdRounds" result_round
                      ON result_round.id = result.ad_round_id
                     AND result_round.game_id = result.game_id
                    JOIN game_cadence cadence ON cadence.game_id = result.game_id
                   WHERE result.responsible_participation_id IS NULL
                )
                UPDATE "KothControlResults" result
                   SET responsible_participation_id = inferred.responsible_participation_id
                  FROM inferred
                 WHERE result.id = inferred.id
                   AND inferred.responsible_participation_id IS NOT NULL;

                -- Keep a partially-applied/retried migration safe before adding
                -- the foreign key, and reject cross-game responsibility links.
                UPDATE "KothControlResults" result
                   SET responsible_participation_id = NULL
                 WHERE responsible_participation_id IS NOT NULL
                   AND NOT EXISTS (
                     SELECT 1 FROM "Participations" participation
                      WHERE participation.id = result.responsible_participation_id
                        AND participation.game_id = result.game_id
                   );

                -- The new official model starts at a future token boundary and
                -- does not reinterpret legacy additive history. Normalize the
                -- old implicit 8/5 default now; existing tokens may be stale only
                -- before that still-unstarted boundary.
                UPDATE "Games" game
                   SET koth_refresh_ticks = CASE
                         WHEN game.ad_epoch_ticks = 8 THEN 4 ELSE 5 END
                 WHERE game.ad_scoring_start_round IS NOT NULL
                   AND game.koth_refresh_ticks IS NULL
                   AND EXISTS (
                     SELECT 1 FROM "GameChallenges" challenge
                      WHERE challenge.game_id = game.id
                        AND challenge."Type" = 5
                   );

                -- Align every unstarted default event with four-tick windows so
                -- each eight-tick scoring epoch contains two complete windows.
                -- Other custom values and every event with a declared scoring
                -- boundary are preserved.
                UPDATE "Games"
                   SET koth_refresh_ticks = 4
                 WHERE ad_scoring_start_round IS NULL
                   AND ad_epoch_ticks = 8
                   AND COALESCE(koth_refresh_ticks, 5) = 5
                   AND EXISTS (
                     SELECT 1 FROM "GameChallenges" challenge
                      WHERE challenge.game_id = "Games".id
                        AND challenge."Type" = 5
                   );

                DO $$
                BEGIN
                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'fk_kothcontrol_responsible_participation'
                       AND conrelid = '"KothControlResults"'::regclass
                  ) THEN
                    ALTER TABLE "KothControlResults"
                      ADD CONSTRAINT fk_kothcontrol_responsible_participation
                      FOREIGN KEY (responsible_participation_id)
                      REFERENCES "Participations"(id) ON DELETE SET NULL;
                  END IF;
                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'ck_games_koth_scoring_start_round'
                       AND conrelid = '"Games"'::regclass
                  ) THEN
                    ALTER TABLE "Games"
                      ADD CONSTRAINT ck_games_koth_scoring_start_round
                      CHECK (koth_scoring_start_round IS NULL
                             OR koth_scoring_start_round >= 1);
                  END IF;
                END
                $$;

                CREATE INDEX IF NOT EXISTS ix_kothcontrol_game_responsible_round
                  ON "KothControlResults"
                    (game_id, responsible_participation_id, ad_round_id);
                CREATE INDEX IF NOT EXISTS ix_kothcontrol_game_controlling_round
                  ON "KothControlResults"
                    (game_id, controlling_participation_id, ad_round_id);
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
                -- Do not rewrite koth_refresh_ticks here: after migration, a
                -- normalized default 4 is indistinguishable from an explicit 4.
                DROP INDEX IF EXISTS ix_kothcontrol_game_controlling_round;
                DROP INDEX IF EXISTS ix_kothcontrol_game_responsible_round;
                ALTER TABLE "KothControlResults"
                  DROP CONSTRAINT IF EXISTS fk_kothcontrol_responsible_participation,
                  DROP COLUMN IF EXISTS marker_observed,
                  DROP COLUMN IF EXISTS responsible_participation_id;
                ALTER TABLE "Games"
                  DROP CONSTRAINT IF EXISTS ck_games_koth_scoring_start_round,
                  DROP COLUMN IF EXISTS koth_scoring_start_round;
                "#,
            )
            .await?;
        Ok(())
    }
}
