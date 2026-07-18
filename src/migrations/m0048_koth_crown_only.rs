//! Enforce crown-cycle KotH capabilities as the only supported shape.

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
                  ALTER COLUMN koth_scoring_formula_version SET DEFAULT 2,
                  DROP CONSTRAINT IF EXISTS ck_games_koth_official_cadence;

                DROP INDEX IF EXISTS ux_kothtokens_part_window;
                DROP INDEX IF EXISTS ux_kothtokens_part_round_mint;
                DROP INDEX IF EXISTS ux_kothtokens_v2_cycle_challenge_part;
                DROP INDEX IF EXISTS ix_kothtokens_round_number;
                DROP INDEX IF EXISTS ix_kothtokens_part_round;
                DROP INDEX IF EXISTS ix_kothtokens_active_round_token;
                ALTER TABLE "KothTokens"
                  ADD COLUMN IF NOT EXISTS reset_attempt INTEGER NOT NULL DEFAULT 0,
                  ALTER COLUMN reset_attempt SET DEFAULT 0,
                  DROP CONSTRAINT IF EXISTS ck_kothtokens_v2_scope,
                  DROP CONSTRAINT IF EXISTS ck_kothtokens_crown_scope;
                -- Existing unscoped rows remain immutable evidence. Fresh tables
                -- are already crown-only through the current entity definition.
                ALTER TABLE "KothTokens"
                  ADD CONSTRAINT ck_kothtokens_crown_scope CHECK (
                    reset_attempt >= 0
                    AND NULLIF(BTRIM(token), '') IS NOT NULL
                    AND (
                      (cycle_id IS NULL AND challenge_id IS NULL)
                      OR (
                        cycle_id IS NOT NULL AND challenge_id IS NOT NULL
                        AND target_id IS NOT NULL AND round_number >= 1
                        AND ad_round_id IS NOT NULL
                      )
                    )
                  );
                CREATE UNIQUE INDEX IF NOT EXISTS ux_kothtokens_cycle_attempt_part
                  ON "KothTokens"(cycle_id, challenge_id, reset_attempt, participation_id);
                CREATE INDEX IF NOT EXISTS ix_kothtokens_active_challenge
                  ON "KothTokens"(challenge_id) WHERE revoked_at IS NULL;
                CREATE INDEX IF NOT EXISTS ix_kothtokens_active_participation
                  ON "KothTokens"(participation_id) WHERE revoked_at IS NULL;

                ALTER TABLE "KothControlResults"
                  ADD COLUMN IF NOT EXISTS token_window_attempt INTEGER NOT NULL DEFAULT 0,
                  ALTER COLUMN token_window_attempt SET DEFAULT 0,
                  DROP CONSTRAINT IF EXISTS ck_kothcontrol_token_window_attempt;
                ALTER TABLE "KothControlResults"
                  ADD CONSTRAINT ck_kothcontrol_token_window_attempt
                  CHECK (token_window_attempt >= 0);
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
                -- A legacy cycle can represent only one capability per team.
                -- Refuse the downgrade before destructive DDL when distinct
                -- reset windows exist; silently deleting either row would
                -- destroy immutable scoring evidence.
                DO $$
                BEGIN
                  IF EXISTS (
                    SELECT 1
                      FROM "KothTokens"
                     WHERE cycle_id IS NOT NULL
                     GROUP BY cycle_id, challenge_id, participation_id
                    HAVING COUNT(*) > 1
                  ) THEN
                    RAISE EXCEPTION
                      'cannot roll back m0048: crown-cycle capability evidence has multiple reset windows'
                      USING HINT = 'Retain m0048 or explicitly archive and consolidate the evidence before retrying.';
                  END IF;
                END
                $$;

                -- Establish the legacy invariant while reset_attempt still
                -- exists, so a failed index build cannot leave its evidence
                -- discriminator removed.
                CREATE UNIQUE INDEX IF NOT EXISTS ux_kothtokens_v2_cycle_challenge_part
                  ON "KothTokens"(cycle_id, challenge_id, participation_id)
                  WHERE cycle_id IS NOT NULL;

                ALTER TABLE "KothControlResults"
                  DROP CONSTRAINT IF EXISTS ck_kothcontrol_token_window_attempt,
                  DROP COLUMN IF EXISTS token_window_attempt;

                DROP INDEX IF EXISTS ux_kothtokens_cycle_attempt_part;
                DROP INDEX IF EXISTS ix_kothtokens_active_challenge;
                DROP INDEX IF EXISTS ix_kothtokens_active_participation;
                ALTER TABLE "KothTokens"
                  DROP CONSTRAINT IF EXISTS ck_kothtokens_crown_scope,
                  DROP COLUMN IF EXISTS reset_attempt;
                ALTER TABLE "KothTokens"
                  DROP CONSTRAINT IF EXISTS ck_kothtokens_v2_scope,
                  ADD CONSTRAINT ck_kothtokens_v2_scope CHECK (
                    (cycle_id IS NULL AND challenge_id IS NULL)
                    OR (cycle_id IS NOT NULL AND challenge_id IS NOT NULL
                        AND target_id IS NOT NULL AND round_number IS NOT NULL)
                  );

                CREATE INDEX IF NOT EXISTS ix_kothtokens_round_number
                  ON "KothTokens"(round_number);
                CREATE INDEX IF NOT EXISTS ix_kothtokens_part_round
                  ON "KothTokens"(participation_id, round_number);
                CREATE INDEX IF NOT EXISTS ix_kothtokens_active_round_token
                  ON "KothTokens"(round_number, token, participation_id)
                  WHERE revoked_at IS NULL AND round_number IS NOT NULL;
                CREATE UNIQUE INDEX IF NOT EXISTS ux_kothtokens_part_window
                  ON "KothTokens"(participation_id, round_number)
                  WHERE round_number IS NOT NULL AND cycle_id IS NULL;
                CREATE UNIQUE INDEX IF NOT EXISTS ux_kothtokens_part_round_mint
                  ON "KothTokens"(participation_id, round_number, ad_round_id)
                  WHERE round_number IS NOT NULL AND ad_round_id IS NOT NULL
                    AND cycle_id IS NULL;

                ALTER TABLE "Games"
                  DROP CONSTRAINT IF EXISTS ck_games_koth_official_cadence;
                ALTER TABLE "Games"
                  ADD CONSTRAINT ck_games_koth_official_cadence
                  CHECK (koth_scoring_start_round IS NULL
                         OR koth_refresh_ticks IS NOT NULL) NOT VALID;
                "#,
            )
            .await?;
        Ok(())
    }
}
