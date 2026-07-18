//! Repair historical KotH identity rows and enforce their database invariants.

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
                LOCK TABLE "Games", "GameChallenges", "AdRounds", "Participations",
                           "KothTargets", "KothTokens", "KothControlResults",
                           "KothEpochTeamRollups", "KothEpochHillRollups"
                  IN SHARE ROW EXCLUSIVE MODE;

                -- A target is meaningful only when its challenge belongs to the
                -- same game. Null legacy token pointers before removing bad rows.
                UPDATE "KothTokens" token
                   SET target_id = NULL
                 WHERE token.target_id IS NOT NULL
                   AND NOT EXISTS (
                         SELECT 1
                           FROM "KothTargets" target
                           JOIN "Games" game ON game.id = target.game_id
                           JOIN "GameChallenges" challenge
                             ON challenge.id = target.challenge_id
                            AND challenge.game_id = target.game_id
                          WHERE target.id = token.target_id
                   );

                DELETE FROM "KothTargets" target
                 WHERE NOT EXISTS (
                       SELECT 1 FROM "Games" game
                        WHERE game.id = target.game_id
                 )
                    OR NOT EXISTS (
                       SELECT 1 FROM "GameChallenges" challenge
                        WHERE challenge.id = target.challenge_id
                          AND challenge.game_id = target.game_id
                 );

                UPDATE "KothTargets" target
                   SET holder_participation_id = NULL, held_since = NULL
                 WHERE target.holder_participation_id IS NOT NULL
                   AND NOT EXISTS (
                         SELECT 1 FROM "Participations" participation
                          WHERE participation.id = target.holder_participation_id
                            AND participation.game_id = target.game_id
                   );

                -- Prefer a container-backed or otherwise reachable endpoint, then
                -- the oldest id. Some historical races left an empty row before the
                -- usable row, so retaining MIN(id) alone can take a live hill down.
                -- Ownership is ambiguous across duplicates; elect it next tick.
                WITH ranked_targets AS (
                  SELECT id,
                         FIRST_VALUE(id) OVER (
                           PARTITION BY game_id, challenge_id
                           ORDER BY
                             (NULLIF(BTRIM(container_id), '') IS NOT NULL) DESC,
                             (NULLIF(BTRIM(host), '') IS NOT NULL
                               AND port BETWEEN 1 AND 65535) DESC,
                             id
                         ) AS retained_id,
                         COUNT(*) OVER (
                           PARTITION BY game_id, challenge_id
                         ) AS target_count
                    FROM "KothTargets"
                ), duplicate_mapping AS (
                  SELECT id AS duplicate_id, retained_id
                    FROM ranked_targets
                   WHERE target_count > 1 AND id <> retained_id
                )
                UPDATE "KothTokens" token
                   SET target_id = mapping.retained_id
                  FROM duplicate_mapping mapping
                 WHERE token.target_id = mapping.duplicate_id;

                WITH ranked_targets AS (
                  SELECT id,
                         FIRST_VALUE(id) OVER (
                           PARTITION BY game_id, challenge_id
                           ORDER BY
                             (NULLIF(BTRIM(container_id), '') IS NOT NULL) DESC,
                             (NULLIF(BTRIM(host), '') IS NOT NULL
                               AND port BETWEEN 1 AND 65535) DESC,
                             id
                         ) AS retained_id,
                         COUNT(*) OVER (
                           PARTITION BY game_id, challenge_id
                         ) AS target_count
                    FROM "KothTargets"
                )
                UPDATE "KothTargets" target
                   SET holder_participation_id = NULL, held_since = NULL
                  FROM ranked_targets ranked
                 WHERE ranked.target_count > 1
                   AND target.id = ranked.retained_id;

                DELETE FROM "KothTargets" target
                 USING (
                   SELECT id, retained_id
                     FROM (
                       SELECT id,
                              FIRST_VALUE(id) OVER (
                                PARTITION BY game_id, challenge_id
                                ORDER BY
                                  (NULLIF(BTRIM(container_id), '') IS NOT NULL) DESC,
                                  (NULLIF(BTRIM(host), '') IS NOT NULL
                                    AND port BETWEEN 1 AND 65535) DESC,
                                  id
                              ) AS retained_id
                         FROM "KothTargets"
                     ) ranked
                    WHERE ranked.id <> ranked.retained_id
                 ) duplicate
                 WHERE target.id = duplicate.id;

                -- A current-window token must point at that participation's game,
                -- exact anchor round. Invalid capabilities are safer to revoke than
                -- to preserve as live bearer credentials.
                DELETE FROM "KothTokens" token
                 WHERE token.round_number IS NOT NULL
                   AND NOT EXISTS (
                         SELECT 1
                           FROM "Participations" participation
                           JOIN "AdRounds" round
                             ON round.id = token.ad_round_id
                            AND round.game_id = participation.game_id
                            AND round.number = token.round_number
                          WHERE participation.id = token.participation_id
                   );

                DELETE FROM "KothTokens" token
                 WHERE token.ad_round_id IS NOT NULL
                   AND NOT EXISTS (
                         SELECT 1 FROM "AdRounds" round
                          WHERE round.id = token.ad_round_id
                   );

                -- The reader contract is one token per participation/window. Keep
                -- the newest valid mint, matching the checker's former preference.
                DELETE FROM "KothTokens" token
                 USING (
                   SELECT id
                     FROM (
                       SELECT id,
                              ROW_NUMBER() OVER (
                                PARTITION BY participation_id, round_number
                                ORDER BY id DESC
                              ) AS duplicate_number
                         FROM "KothTokens"
                        WHERE round_number IS NOT NULL
                     ) ranked
                    WHERE ranked.duplicate_number > 1
                 ) duplicate
                 WHERE token.id = duplicate.id;

                -- Score evidence must reference one coherent game/challenge/round.
                DELETE FROM "KothControlResults" result
                 WHERE NOT EXISTS (
                       SELECT 1 FROM "Games" game
                        WHERE game.id = result.game_id
                 )
                    OR NOT EXISTS (
                       SELECT 1 FROM "GameChallenges" challenge
                        WHERE challenge.id = result.challenge_id
                          AND challenge.game_id = result.game_id
                 )
                    OR NOT EXISTS (
                       SELECT 1 FROM "AdRounds" round
                        WHERE round.id = result.ad_round_id
                          AND round.game_id = result.game_id
                 );

                UPDATE "KothControlResults" result
                   SET controlling_participation_id = NULL
                 WHERE result.controlling_participation_id IS NOT NULL
                   AND NOT EXISTS (
                         SELECT 1 FROM "Participations" participation
                          WHERE participation.id = result.controlling_participation_id
                            AND participation.game_id = result.game_id
                   );

                UPDATE "KothControlResults" result
                   SET responsible_participation_id = NULL
                 WHERE result.responsible_participation_id IS NOT NULL
                   AND NOT EXISTS (
                         SELECT 1 FROM "Participations" participation
                          WHERE participation.id = result.responsible_participation_id
                            AND participation.game_id = result.game_id
                   );

                -- m0039's scalar foreign keys prove that each referenced row
                -- exists, but not that it belongs to the rollup's game. Remove
                -- invalid historical projections before adding composite keys.
                DELETE FROM "KothEpochTeamRollups" rollup
                 WHERE NOT EXISTS (
                       SELECT 1 FROM "Participations" participation
                        WHERE participation.id = rollup.participation_id
                          AND participation.game_id = rollup.game_id
                 );

                DELETE FROM "KothEpochHillRollups" rollup
                 WHERE NOT EXISTS (
                       SELECT 1 FROM "Participations" participation
                        WHERE participation.id = rollup.participation_id
                          AND participation.game_id = rollup.game_id
                 )
                    OR NOT EXISTS (
                       SELECT 1 FROM "GameChallenges" challenge
                        WHERE challenge.id = rollup.challenge_id
                          AND challenge.game_id = rollup.game_id
                 );

                CREATE UNIQUE INDEX IF NOT EXISTS ux_kothtargets_game_challenge
                  ON "KothTargets"(game_id, challenge_id);
                CREATE UNIQUE INDEX IF NOT EXISTS ux_kothtokens_part_window
                  ON "KothTokens"(participation_id, round_number)
                  WHERE round_number IS NOT NULL;

                -- Composite parent keys let child FKs enforce game consistency,
                -- not merely the independent existence of each referenced row.
                CREATE UNIQUE INDEX IF NOT EXISTS ux_gamechallenges_game_id
                  ON "GameChallenges"(game_id, id);
                CREATE UNIQUE INDEX IF NOT EXISTS ux_adrounds_game_id
                  ON "AdRounds"(game_id, id);
                CREATE UNIQUE INDEX IF NOT EXISTS ux_participations_game_id
                  ON "Participations"(game_id, id);

                DO $$
                BEGIN
                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'fk_kothtargets_game'
                       AND conrelid = '"KothTargets"'::regclass
                  ) THEN
                    ALTER TABLE "KothTargets"
                      ADD CONSTRAINT fk_kothtargets_game
                      FOREIGN KEY (game_id) REFERENCES "Games"(id)
                      ON DELETE CASCADE;
                  END IF;

                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'fk_kothtargets_challenge'
                       AND conrelid = '"KothTargets"'::regclass
                  ) THEN
                    ALTER TABLE "KothTargets"
                      ADD CONSTRAINT fk_kothtargets_challenge
                      FOREIGN KEY (game_id, challenge_id)
                      REFERENCES "GameChallenges"(game_id, id)
                      ON DELETE CASCADE;
                  END IF;

                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'fk_kothtokens_target'
                       AND conrelid = '"KothTokens"'::regclass
                  ) THEN
                    ALTER TABLE "KothTokens"
                      ADD CONSTRAINT fk_kothtokens_target
                      FOREIGN KEY (target_id) REFERENCES "KothTargets"(id)
                      ON DELETE SET NULL;
                  END IF;

                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'fk_kothtokens_ad_round'
                       AND conrelid = '"KothTokens"'::regclass
                  ) THEN
                    ALTER TABLE "KothTokens"
                      ADD CONSTRAINT fk_kothtokens_ad_round
                      FOREIGN KEY (ad_round_id) REFERENCES "AdRounds"(id)
                      ON DELETE CASCADE;
                  END IF;

                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'fk_kothcontrol_game'
                       AND conrelid = '"KothControlResults"'::regclass
                  ) THEN
                    ALTER TABLE "KothControlResults"
                      ADD CONSTRAINT fk_kothcontrol_game
                      FOREIGN KEY (game_id) REFERENCES "Games"(id)
                      ON DELETE CASCADE;
                  END IF;

                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'fk_kothcontrol_challenge'
                       AND conrelid = '"KothControlResults"'::regclass
                  ) THEN
                    ALTER TABLE "KothControlResults"
                      ADD CONSTRAINT fk_kothcontrol_challenge
                      FOREIGN KEY (game_id, challenge_id)
                      REFERENCES "GameChallenges"(game_id, id)
                      ON DELETE CASCADE;
                  END IF;

                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'fk_kothcontrol_round'
                       AND conrelid = '"KothControlResults"'::regclass
                  ) THEN
                    ALTER TABLE "KothControlResults"
                      ADD CONSTRAINT fk_kothcontrol_round
                      FOREIGN KEY (game_id, ad_round_id)
                      REFERENCES "AdRounds"(game_id, id)
                      ON DELETE CASCADE;
                  END IF;

                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'fk_kothcontrol_controlling_participation'
                       AND conrelid = '"KothControlResults"'::regclass
                  ) THEN
                    ALTER TABLE "KothControlResults"
                      ADD CONSTRAINT fk_kothcontrol_controlling_participation
                      FOREIGN KEY (controlling_participation_id)
                      REFERENCES "Participations"(id)
                      ON DELETE SET NULL;
                  END IF;

                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'fk_kothcontrol_controlling_participation_game'
                       AND conrelid = '"KothControlResults"'::regclass
                  ) THEN
                    ALTER TABLE "KothControlResults"
                      ADD CONSTRAINT fk_kothcontrol_controlling_participation_game
                      FOREIGN KEY (game_id, controlling_participation_id)
                      REFERENCES "Participations"(game_id, id)
                      ON DELETE SET NULL (controlling_participation_id);
                  END IF;

                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'fk_kothcontrol_responsible_participation_game'
                       AND conrelid = '"KothControlResults"'::regclass
                  ) THEN
                    ALTER TABLE "KothControlResults"
                      ADD CONSTRAINT fk_kothcontrol_responsible_participation_game
                      FOREIGN KEY (game_id, responsible_participation_id)
                      REFERENCES "Participations"(game_id, id)
                      ON DELETE SET NULL (responsible_participation_id);
                  END IF;

                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'fk_koth_epoch_team_participation_game'
                       AND conrelid = '"KothEpochTeamRollups"'::regclass
                  ) THEN
                    ALTER TABLE "KothEpochTeamRollups"
                      ADD CONSTRAINT fk_koth_epoch_team_participation_game
                      FOREIGN KEY (game_id, participation_id)
                      REFERENCES "Participations"(game_id, id)
                      ON DELETE CASCADE;
                  END IF;

                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'fk_koth_epoch_hill_participation_game'
                       AND conrelid = '"KothEpochHillRollups"'::regclass
                  ) THEN
                    ALTER TABLE "KothEpochHillRollups"
                      ADD CONSTRAINT fk_koth_epoch_hill_participation_game
                      FOREIGN KEY (game_id, participation_id)
                      REFERENCES "Participations"(game_id, id)
                      ON DELETE CASCADE;
                  END IF;

                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'fk_koth_epoch_hill_challenge_game'
                       AND conrelid = '"KothEpochHillRollups"'::regclass
                  ) THEN
                    ALTER TABLE "KothEpochHillRollups"
                      ADD CONSTRAINT fk_koth_epoch_hill_challenge_game
                      FOREIGN KEY (game_id, challenge_id)
                      REFERENCES "GameChallenges"(game_id, id)
                      ON DELETE CASCADE;
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
                ALTER TABLE "KothEpochHillRollups"
                  DROP CONSTRAINT IF EXISTS fk_koth_epoch_hill_challenge_game,
                  DROP CONSTRAINT IF EXISTS fk_koth_epoch_hill_participation_game;
                ALTER TABLE "KothEpochTeamRollups"
                  DROP CONSTRAINT IF EXISTS fk_koth_epoch_team_participation_game;
                ALTER TABLE "KothControlResults"
                  DROP CONSTRAINT IF EXISTS fk_kothcontrol_responsible_participation_game,
                  DROP CONSTRAINT IF EXISTS fk_kothcontrol_controlling_participation_game,
                  DROP CONSTRAINT IF EXISTS fk_kothcontrol_controlling_participation,
                  DROP CONSTRAINT IF EXISTS fk_kothcontrol_round,
                  DROP CONSTRAINT IF EXISTS fk_kothcontrol_challenge,
                  DROP CONSTRAINT IF EXISTS fk_kothcontrol_game;
                ALTER TABLE "KothTokens"
                  DROP CONSTRAINT IF EXISTS fk_kothtokens_ad_round,
                  DROP CONSTRAINT IF EXISTS fk_kothtokens_target;
                ALTER TABLE "KothTargets"
                  DROP CONSTRAINT IF EXISTS fk_kothtargets_challenge,
                  DROP CONSTRAINT IF EXISTS fk_kothtargets_game;

                DROP INDEX IF EXISTS ux_participations_game_id;
                DROP INDEX IF EXISTS ux_adrounds_game_id;
                DROP INDEX IF EXISTS ux_gamechallenges_game_id;
                DROP INDEX IF EXISTS ux_kothtokens_part_window;
                DROP INDEX IF EXISTS ux_kothtargets_game_challenge;
                "#,
            )
            .await?;
        Ok(())
    }
}
