//! Durable ownership for the external A&D/KotH round-finishing pipeline.

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
                DO $$
                BEGIN
                  -- Existing history predates the durable pipeline marker. Mark
                  -- settled history complete, but leave a genuinely unfinished
                  -- latest live round claimable after the upgrade.
                  IF NOT EXISTS (
                    SELECT 1
                      FROM information_schema.columns
                     WHERE table_schema = current_schema()
                       AND table_name = 'AdRounds'
                       AND column_name = 'pipeline_completed_at'
                  ) THEN
                    ALTER TABLE "AdRounds"
                      ADD COLUMN pipeline_completed_at TIMESTAMPTZ NULL;
                    UPDATE "AdRounds" round
                       SET pipeline_completed_at = clock_timestamp()
                      FROM "Games" game
                     WHERE game.id = round.game_id
                       AND (
                         round.finalized = TRUE
                         OR round.id IS DISTINCT FROM (
                           SELECT latest.id
                             FROM "AdRounds" latest
                            WHERE latest.game_id = round.game_id
                            ORDER BY latest.number DESC, latest.id DESC
                            LIMIT 1
                         )
                         OR NOT (
                           game.start_time_utc <= clock_timestamp()
                           AND clock_timestamp() < game.end_time_utc
                         )
                         OR (
                           NOT EXISTS (
                             SELECT 1
                               FROM "AdTeamServices" service
                               JOIN "Participations" participation
                                 ON participation.id = service.participation_id
                                AND participation.game_id = service.game_id
                               JOIN "GameChallenges" challenge
                                 ON challenge.id = service.challenge_id
                                AND challenge.game_id = service.game_id
                               LEFT JOIN "AdCheckResults" result
                                 ON result.round_id = round.id
                                AND result.team_service_id = service.id
                              WHERE service.game_id = round.game_id
                                AND participation.status = 1
                                AND challenge.is_enabled = TRUE
                                AND challenge.review_status = 0
                                AND challenge."Type" = 4
                                AND (
                                  result.id IS NULL
                                  OR result.sla_credit IS NULL
                                )
                           )
                           AND NOT EXISTS (
                             SELECT 1
                               FROM "KothTargets" target
                               JOIN "GameChallenges" challenge
                                 ON challenge.id = target.challenge_id
                                AND challenge.game_id = target.game_id
                              WHERE target.game_id = round.game_id
                                AND challenge.is_enabled = TRUE
                                AND challenge.review_status = 0
                                AND challenge."Type" = 5
                                AND NOT EXISTS (
                                  SELECT 1
                                    FROM "KothControlResults" result
                                   WHERE result.game_id = target.game_id
                                     AND result.challenge_id = target.challenge_id
                                     AND result.ad_round_id = round.id
                                )
                           )
                         )
                       );
                  END IF;
                END
                $$;

                ALTER TABLE "AdRounds"
                  ADD COLUMN IF NOT EXISTS pipeline_lease_token TEXT NULL,
                  ADD COLUMN IF NOT EXISTS pipeline_lease_until TIMESTAMPTZ NULL;

                CREATE INDEX IF NOT EXISTS ix_adrounds_pipeline_pending
                  ON "AdRounds"(game_id, number DESC)
                  WHERE pipeline_completed_at IS NULL;

                -- Before cadence snapshotting, official KotH rollups interpreted a
                -- NULL game override with the fixed four-tick fallback. Preserve
                -- that historical wire/scoring contract during the upgrade rather
                -- than letting the first post-upgrade replica choose its local env.
                UPDATE "Games"
                   SET koth_refresh_ticks = 4
                 WHERE koth_scoring_start_round IS NOT NULL
                   AND koth_refresh_ticks IS NULL;

                DO $$
                BEGIN
                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'ck_games_koth_official_cadence'
                       AND conrelid = '"Games"'::regclass
                  ) THEN
                    ALTER TABLE "Games"
                      ADD CONSTRAINT ck_games_koth_official_cadence
                      CHECK (koth_scoring_start_round IS NULL
                             OR koth_refresh_ticks IS NOT NULL);
                  END IF;
                END
                $$;

                -- Older binaries admitted evidence on/after event end and rounds
                -- beginning exactly at the exclusive deadline. Drop the affected
                -- cumulative chains; the bounded builder recreates them using the
                -- strict event-end fence.
                DELETE FROM "AdEpochRollups" rollup
                 USING "Games" game
                 WHERE game.id = rollup.game_id
                   AND (
                     EXISTS (
                       SELECT 1
                         FROM "AdCheckResults" result
                         JOIN "AdRounds" round ON round.id = result.round_id
                        WHERE round.game_id = game.id
                          AND result.sla_credit IS NOT NULL
                          AND result.checked_at >= game.end_time_utc
                     )
                     OR EXISTS (
                       SELECT 1
                         FROM "AdAttacks" attack
                         JOIN "AdRounds" round ON round.id = attack.round_id
                        WHERE round.game_id = game.id
                          AND attack.submitted_at >= game.end_time_utc
                     )
                     OR EXISTS (
                       SELECT 1
                         FROM "AdRounds" round
                        WHERE round.game_id = game.id
                          AND round.start_time_utc >= game.end_time_utc
                          AND game.ad_scoring_start_round IS NOT NULL
                          AND round.number >= game.ad_scoring_start_round
                     )
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
                DROP INDEX IF EXISTS ix_adrounds_pipeline_pending;
                ALTER TABLE "Games"
                  DROP CONSTRAINT IF EXISTS ck_games_koth_official_cadence;
                ALTER TABLE "AdRounds"
                  DROP COLUMN IF EXISTS pipeline_lease_until,
                  DROP COLUMN IF EXISTS pipeline_lease_token,
                  DROP COLUMN IF EXISTS pipeline_completed_at;
                "#,
            )
            .await?;
        Ok(())
    }
}
