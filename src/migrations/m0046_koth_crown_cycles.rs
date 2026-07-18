//! Durable, versioned KotH crown-cycle configuration, lifecycle, and evidence.

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
                  ADD COLUMN IF NOT EXISTS koth_epoch_ticks INTEGER NOT NULL DEFAULT 12,
                  ADD COLUMN IF NOT EXISTS koth_cycle_ticks INTEGER NOT NULL DEFAULT 3,
                  ADD COLUMN IF NOT EXISTS koth_champion_cooldown_ticks INTEGER NOT NULL DEFAULT 1,
                  ADD COLUMN IF NOT EXISTS koth_claim_confirmation_ticks INTEGER NOT NULL DEFAULT 2;

                -- A declared v1 boundary is immutable. Only templates which have
                -- not started official KotH scoring opt into the crown-cycle formula.
                UPDATE "Games"
                   SET koth_scoring_formula_version = 2
                 WHERE koth_scoring_start_round IS NULL;
                ALTER TABLE "Games"
                  ALTER COLUMN koth_scoring_formula_version SET DEFAULT 2;

                DO $$
                BEGIN
                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'ck_games_koth_crown_config'
                       AND conrelid = '"Games"'::regclass
                  ) THEN
                    ALTER TABLE "Games"
                      ADD CONSTRAINT ck_games_koth_crown_config CHECK (
                        koth_epoch_ticks BETWEEN 2 AND 64
                        AND koth_cycle_ticks BETWEEN 1 AND koth_epoch_ticks / 2
                        AND MOD(koth_epoch_ticks, koth_cycle_ticks) = 0
                        AND koth_champion_cooldown_ticks BETWEEN 0 AND koth_cycle_ticks - 1
                        AND koth_claim_confirmation_ticks BETWEEN 1 AND koth_cycle_ticks
                      );
                  END IF;
                END
                $$;

                CREATE TABLE IF NOT EXISTS "KothOfficialConfigs" (
                  game_id INTEGER PRIMARY KEY REFERENCES "Games"(id) ON DELETE CASCADE,
                  formula_version SMALLINT NOT NULL,
                  scoring_start_round INTEGER NOT NULL,
                  epoch_ticks INTEGER NOT NULL,
                  cycle_ticks INTEGER NOT NULL,
                  champion_cooldown_ticks INTEGER NOT NULL,
                  claim_confirmation_ticks INTEGER NOT NULL,
                  roster_snapshot JSONB NOT NULL,
                  hills_snapshot JSONB NOT NULL,
                  created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                  CONSTRAINT ux_koth_official_configs_game_formula
                    UNIQUE (game_id, formula_version),
                  CONSTRAINT ck_koth_official_configs_version
                    CHECK (formula_version >= 2),
                  CONSTRAINT ck_koth_official_configs_start
                    CHECK (scoring_start_round >= 1),
                  CONSTRAINT ck_koth_official_configs_shape CHECK (
                    epoch_ticks BETWEEN 2 AND 64
                    AND cycle_ticks BETWEEN 1 AND epoch_ticks / 2
                    AND MOD(epoch_ticks, cycle_ticks) = 0
                    AND champion_cooldown_ticks BETWEEN 0 AND cycle_ticks - 1
                    AND claim_confirmation_ticks BETWEEN 1 AND cycle_ticks
                  ),
                  CONSTRAINT ck_koth_official_configs_snapshots CHECK (
                    jsonb_typeof(roster_snapshot) = 'array'
                    AND jsonb_typeof(hills_snapshot) = 'array'
                  )
                );

                CREATE TABLE IF NOT EXISTS "KothCrownCycles" (
                  id BIGSERIAL PRIMARY KEY,
                  game_id INTEGER NOT NULL,
                  challenge_id INTEGER NOT NULL,
                  formula_version SMALLINT NOT NULL,
                  cycle_number INTEGER NOT NULL,
                  epoch INTEGER NOT NULL,
                  planned_start_round INTEGER NOT NULL,
                  planned_end_round INTEGER NOT NULL,
                  actual_start_round INTEGER NULL,
                  actual_end_round INTEGER NULL,
                  phase TEXT NOT NULL DEFAULT 'FinalizePending',
                  lease_token TEXT NULL,
                  lease_until TIMESTAMPTZ NULL,
                  old_container_id TEXT NULL,
                  replacement_container_id TEXT NULL,
                  replacement_host TEXT NULL,
                  replacement_port INTEGER NULL,
                  expected_image TEXT NOT NULL,
                  champion_participation_id INTEGER NULL,
                  provisional_participation_id INTEGER NULL,
                  confirmed_participation_id INTEGER NULL,
                  confirmation_progress INTEGER NOT NULL DEFAULT 0,
                  reset_attempt INTEGER NOT NULL DEFAULT 0,
                  readiness_attempt INTEGER NOT NULL DEFAULT 0,
                  readiness_failures INTEGER NOT NULL DEFAULT 0,
                  readiness_error TEXT NULL,
                  last_error TEXT NULL,
                  audit_receipt JSONB NULL,
                  filesystem_diff JSONB NULL,
                  finalized_at TIMESTAMPTZ NULL,
                  reset_started_at TIMESTAMPTZ NULL,
                  activated_at TIMESTAMPTZ NULL,
                  completed_at TIMESTAMPTZ NULL,
                  created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                  updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                  CONSTRAINT ux_koth_crown_cycles_identity
                    UNIQUE (game_id, challenge_id, formula_version, cycle_number),
                  CONSTRAINT ux_koth_crown_cycles_id_challenge
                    UNIQUE (id, challenge_id),
                  CONSTRAINT fk_koth_crown_cycles_config
                    FOREIGN KEY (game_id, formula_version)
                    REFERENCES "KothOfficialConfigs"(game_id, formula_version)
                    ON DELETE CASCADE,
                  CONSTRAINT fk_koth_crown_cycles_challenge
                    FOREIGN KEY (game_id, challenge_id)
                    REFERENCES "GameChallenges"(game_id, id) ON DELETE CASCADE,
                  CONSTRAINT fk_koth_crown_cycles_champion
                    FOREIGN KEY (game_id, champion_participation_id)
                    REFERENCES "Participations"(game_id, id)
                    ON DELETE SET NULL (champion_participation_id),
                  CONSTRAINT fk_koth_crown_cycles_provisional
                    FOREIGN KEY (game_id, provisional_participation_id)
                    REFERENCES "Participations"(game_id, id)
                    ON DELETE SET NULL (provisional_participation_id),
                  CONSTRAINT fk_koth_crown_cycles_confirmed
                    FOREIGN KEY (game_id, confirmed_participation_id)
                    REFERENCES "Participations"(game_id, id)
                    ON DELETE SET NULL (confirmed_participation_id),
                  CONSTRAINT ck_koth_crown_cycles_identity CHECK (
                    formula_version >= 2 AND cycle_number >= 1 AND epoch >= 1
                  ),
                  CONSTRAINT ck_koth_crown_cycles_rounds CHECK (
                    planned_start_round >= 1
                    AND planned_end_round >= planned_start_round
                    AND (actual_start_round IS NULL OR actual_start_round >= planned_start_round)
                    AND (actual_end_round IS NULL OR (
                      actual_start_round IS NOT NULL AND actual_end_round >= actual_start_round
                    ))
                  ),
                  CONSTRAINT ck_koth_crown_cycles_phase CHECK (phase IN (
                    'FinalizePending', 'SnapshotPending', 'DestroyPending',
                    'CreatePending', 'PublishPending', 'CapabilityPending',
                    'ReadinessPending', 'FirewallPending', 'Active',
                    'CooldownReleasePending', 'Completed', 'Failed', 'Ended'
                  )),
                  CONSTRAINT ck_koth_crown_cycles_counts CHECK (
                    confirmation_progress >= 0 AND reset_attempt >= 0
                    AND readiness_attempt >= 0 AND readiness_failures >= 0
                  ),
                  CONSTRAINT ck_koth_crown_cycles_image
                    CHECK (NULLIF(BTRIM(expected_image), '') IS NOT NULL),
                  CONSTRAINT ck_koth_crown_cycles_replacement_endpoint CHECK (
                    (replacement_container_id IS NULL
                      AND replacement_host IS NULL AND replacement_port IS NULL)
                    OR (NULLIF(BTRIM(replacement_container_id), '') IS NOT NULL
                      AND NULLIF(BTRIM(replacement_host), '') IS NOT NULL
                      AND replacement_port BETWEEN 1 AND 65535)
                  ),
                  CONSTRAINT ck_koth_crown_cycles_lease CHECK (
                    (lease_token IS NULL) = (lease_until IS NULL)
                  )
                );
                CREATE INDEX IF NOT EXISTS ix_koth_crown_cycles_recovery
                  ON "KothCrownCycles"(phase, lease_until, game_id, challenge_id)
                  WHERE phase <> 'Completed';
                CREATE INDEX IF NOT EXISTS ix_koth_crown_cycles_game_active
                  ON "KothCrownCycles"(game_id, challenge_id, cycle_number DESC);

                CREATE TABLE IF NOT EXISTS "KothCycleCooldowns" (
                  cycle_id BIGINT NOT NULL REFERENCES "KothCrownCycles"(id) ON DELETE CASCADE,
                  participation_id INTEGER NOT NULL
                    REFERENCES "Participations"(id) ON DELETE CASCADE,
                  lead_healthy_controlled_ticks INTEGER NOT NULL,
                  starts_round INTEGER NOT NULL,
                  expires_after_round INTEGER NOT NULL,
                  network_enforced BOOLEAN NOT NULL DEFAULT FALSE,
                  network_enforced_at TIMESTAMPTZ NULL,
                  network_released_at TIMESTAMPTZ NULL,
                  created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                  PRIMARY KEY (cycle_id, participation_id),
                  CONSTRAINT ck_koth_cycle_cooldowns_counts CHECK (
                    lead_healthy_controlled_ticks >= 0 AND starts_round >= 1
                    AND expires_after_round >= starts_round
                  ),
                  CONSTRAINT ck_koth_cycle_cooldowns_network CHECK (
                    (NOT network_enforced OR network_enforced_at IS NOT NULL)
                    AND (network_released_at IS NULL OR network_enforced_at IS NOT NULL)
                  )
                );
                CREATE INDEX IF NOT EXISTS ix_koth_cycle_cooldowns_participation
                  ON "KothCycleCooldowns"(participation_id, cycle_id);

                CREATE TABLE IF NOT EXISTS "KothCycleAuditReceipts" (
                  id BIGSERIAL PRIMARY KEY,
                  cycle_id BIGINT NOT NULL REFERENCES "KothCrownCycles"(id) ON DELETE CASCADE,
                  phase TEXT NOT NULL,
                  attempt INTEGER NOT NULL,
                  receipt JSONB NOT NULL,
                  filesystem_diff JSONB NULL,
                  created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                  CONSTRAINT ux_koth_cycle_audit_phase_attempt
                    UNIQUE (cycle_id, phase, attempt),
                  CONSTRAINT ck_koth_cycle_audit_attempt CHECK (attempt >= 0),
                  CONSTRAINT ck_koth_cycle_audit_receipt
                    CHECK (jsonb_typeof(receipt) = 'object')
                );

                ALTER TABLE "KothTokens"
                  ADD COLUMN IF NOT EXISTS cycle_id BIGINT NULL,
                  ADD COLUMN IF NOT EXISTS challenge_id INTEGER NULL;
                -- V1 keeps one game-wide mint per participation/round. V2 mints
                -- one capability per hill, so the historical uniqueness gates
                -- must apply only to rows without a crown-cycle identity.
                DROP INDEX IF EXISTS ux_kothtokens_part_window;
                CREATE UNIQUE INDEX ux_kothtokens_part_window
                  ON "KothTokens"(participation_id, round_number)
                  WHERE round_number IS NOT NULL AND cycle_id IS NULL;
                DROP INDEX IF EXISTS ux_kothtokens_part_round_mint;
                CREATE UNIQUE INDEX ux_kothtokens_part_round_mint
                  ON "KothTokens"(participation_id, round_number, ad_round_id)
                  WHERE round_number IS NOT NULL AND ad_round_id IS NOT NULL
                    AND cycle_id IS NULL;
                CREATE UNIQUE INDEX IF NOT EXISTS ux_kothtokens_v2_cycle_challenge_part
                  ON "KothTokens"(cycle_id, challenge_id, participation_id)
                  WHERE cycle_id IS NOT NULL;
                CREATE UNIQUE INDEX IF NOT EXISTS ux_kothtokens_v2_cycle_token
                  ON "KothTokens"(cycle_id, token)
                  WHERE cycle_id IS NOT NULL;
                CREATE UNIQUE INDEX IF NOT EXISTS ux_kothtokens_id_cycle
                  ON "KothTokens"(id, cycle_id);
                CREATE UNIQUE INDEX IF NOT EXISTS ux_kothtargets_id_challenge
                  ON "KothTargets"(id, challenge_id);

                CREATE TABLE IF NOT EXISTS "KothClaimStates" (
                  target_id INTEGER PRIMARY KEY REFERENCES "KothTargets"(id) ON DELETE CASCADE,
                  cycle_id BIGINT NOT NULL REFERENCES "KothCrownCycles"(id) ON DELETE CASCADE,
                  container_id TEXT NOT NULL,
                  token_id INTEGER NULL REFERENCES "KothTokens"(id) ON DELETE SET NULL,
                  token_window_round INTEGER NULL,
                  provisional_participation_id INTEGER NULL
                    REFERENCES "Participations"(id) ON DELETE SET NULL,
                  confirmation_streak INTEGER NOT NULL DEFAULT 0,
                  confirmed_participation_id INTEGER NULL
                    REFERENCES "Participations"(id) ON DELETE SET NULL,
                  updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                  CONSTRAINT fk_koth_claim_states_token_cycle
                    FOREIGN KEY (token_id, cycle_id)
                    REFERENCES "KothTokens"(id, cycle_id)
                    ON DELETE SET NULL (token_id),
                  CONSTRAINT ck_koth_claim_states_container
                    CHECK (NULLIF(BTRIM(container_id), '') IS NOT NULL),
                  CONSTRAINT ck_koth_claim_states_streak
                    CHECK (confirmation_streak >= 0),
                  CONSTRAINT ck_koth_claim_states_token_window CHECK (
                    (token_id IS NULL) = (token_window_round IS NULL)
                    AND (token_window_round IS NULL OR token_window_round >= 1)
                  )
                );

                ALTER TABLE "KothControlResults"
                  ADD COLUMN IF NOT EXISTS cycle_id BIGINT NULL,
                  ADD COLUMN IF NOT EXISTS container_id TEXT NULL,
                  ADD COLUMN IF NOT EXISTS token_id INTEGER NULL,
                  ADD COLUMN IF NOT EXISTS token_window_round INTEGER NULL,
                  ADD COLUMN IF NOT EXISTS provisional_participation_id INTEGER NULL,
                  ADD COLUMN IF NOT EXISTS confirmed_participation_id INTEGER NULL,
                  ADD COLUMN IF NOT EXISTS confirmation_streak INTEGER NULL,
                  ADD COLUMN IF NOT EXISTS is_scorable BOOLEAN NOT NULL DEFAULT TRUE,
                  ADD COLUMN IF NOT EXISTS void_reason TEXT NULL;
                UPDATE "KothControlResults"
                   SET is_scorable = FALSE,
                       void_reason = COALESCE(void_reason, error_message, 'platform-attributed failure')
                 WHERE status = 3;

                CREATE TABLE IF NOT EXISTS "KothAcquisitions" (
                  id BIGSERIAL PRIMARY KEY,
                  cycle_id BIGINT NOT NULL REFERENCES "KothCrownCycles"(id) ON DELETE CASCADE,
                  token_id INTEGER NOT NULL REFERENCES "KothTokens"(id) ON DELETE RESTRICT,
                  game_id INTEGER NOT NULL,
                  challenge_id INTEGER NOT NULL,
                  target_id INTEGER NOT NULL REFERENCES "KothTargets"(id) ON DELETE CASCADE,
                  participation_id INTEGER NOT NULL,
                  container_id TEXT NOT NULL,
                  token_window_round INTEGER NOT NULL,
                  ad_round_id INTEGER NOT NULL,
                  confirmed_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                  CONSTRAINT ux_koth_acquisitions_cycle_token UNIQUE (cycle_id, token_id),
                  CONSTRAINT fk_koth_acquisitions_token_cycle
                    FOREIGN KEY (token_id, cycle_id)
                    REFERENCES "KothTokens"(id, cycle_id) ON DELETE RESTRICT,
                  CONSTRAINT fk_koth_acquisitions_cycle_challenge
                    FOREIGN KEY (cycle_id, challenge_id)
                    REFERENCES "KothCrownCycles"(id, challenge_id) ON DELETE CASCADE,
                  CONSTRAINT fk_koth_acquisitions_challenge
                    FOREIGN KEY (game_id, challenge_id)
                    REFERENCES "GameChallenges"(game_id, id) ON DELETE CASCADE,
                  CONSTRAINT fk_koth_acquisitions_target_challenge
                    FOREIGN KEY (target_id, challenge_id)
                    REFERENCES "KothTargets"(id, challenge_id) ON DELETE CASCADE,
                  CONSTRAINT fk_koth_acquisitions_participation
                    FOREIGN KEY (game_id, participation_id)
                    REFERENCES "Participations"(game_id, id) ON DELETE CASCADE,
                  CONSTRAINT fk_koth_acquisitions_round
                    FOREIGN KEY (game_id, ad_round_id)
                    REFERENCES "AdRounds"(game_id, id) ON DELETE CASCADE,
                  CONSTRAINT ck_koth_acquisitions_identity CHECK (
                    token_window_round >= 1
                    AND NULLIF(BTRIM(container_id), '') IS NOT NULL
                  )
                );
                CREATE INDEX IF NOT EXISTS ix_koth_acquisitions_team_hill
                  ON "KothAcquisitions"(game_id, participation_id, challenge_id, confirmed_at);

                DO $$
                BEGIN
                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'fk_kothtokens_cycle'
                       AND conrelid = '"KothTokens"'::regclass
                  ) THEN
                    ALTER TABLE "KothTokens"
                      ADD CONSTRAINT fk_kothtokens_cycle
                      FOREIGN KEY (cycle_id) REFERENCES "KothCrownCycles"(id)
                      ON DELETE CASCADE;
                  END IF;
                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'fk_kothtokens_cycle_challenge'
                       AND conrelid = '"KothTokens"'::regclass
                  ) THEN
                    ALTER TABLE "KothTokens"
                      ADD CONSTRAINT fk_kothtokens_cycle_challenge
                      FOREIGN KEY (cycle_id, challenge_id)
                      REFERENCES "KothCrownCycles"(id, challenge_id)
                      ON DELETE CASCADE;
                  END IF;
                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'fk_kothtokens_target_challenge'
                       AND conrelid = '"KothTokens"'::regclass
                  ) THEN
                    ALTER TABLE "KothTokens"
                      ADD CONSTRAINT fk_kothtokens_target_challenge
                      FOREIGN KEY (target_id, challenge_id)
                      REFERENCES "KothTargets"(id, challenge_id)
                      ON DELETE CASCADE;
                  END IF;
                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'fk_kothtokens_challenge'
                       AND conrelid = '"KothTokens"'::regclass
                  ) THEN
                    ALTER TABLE "KothTokens"
                      ADD CONSTRAINT fk_kothtokens_challenge
                      FOREIGN KEY (challenge_id) REFERENCES "GameChallenges"(id)
                      ON DELETE CASCADE;
                  END IF;
                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'fk_kothcontrol_cycle_challenge'
                       AND conrelid = '"KothControlResults"'::regclass
                  ) THEN
                    ALTER TABLE "KothControlResults"
                      ADD CONSTRAINT fk_kothcontrol_cycle_challenge
                      FOREIGN KEY (cycle_id, challenge_id)
                      REFERENCES "KothCrownCycles"(id, challenge_id)
                      ON DELETE CASCADE;
                  END IF;
                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'fk_kothcontrol_token_cycle'
                       AND conrelid = '"KothControlResults"'::regclass
                  ) THEN
                    ALTER TABLE "KothControlResults"
                      ADD CONSTRAINT fk_kothcontrol_token_cycle
                      FOREIGN KEY (token_id, cycle_id)
                      REFERENCES "KothTokens"(id, cycle_id)
                      ON DELETE SET NULL (token_id);
                  END IF;
                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'ck_kothtokens_v2_scope'
                       AND conrelid = '"KothTokens"'::regclass
                  ) THEN
                    ALTER TABLE "KothTokens"
                      ADD CONSTRAINT ck_kothtokens_v2_scope CHECK (
                        (cycle_id IS NULL AND challenge_id IS NULL)
                        OR (cycle_id IS NOT NULL AND challenge_id IS NOT NULL
                            AND target_id IS NOT NULL AND round_number IS NOT NULL)
                      );
                  END IF;

                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'fk_kothcontrol_cycle'
                       AND conrelid = '"KothControlResults"'::regclass
                  ) THEN
                    ALTER TABLE "KothControlResults"
                      ADD CONSTRAINT fk_kothcontrol_cycle
                      FOREIGN KEY (cycle_id) REFERENCES "KothCrownCycles"(id)
                      ON DELETE CASCADE;
                  END IF;
                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'fk_kothcontrol_token'
                       AND conrelid = '"KothControlResults"'::regclass
                  ) THEN
                    ALTER TABLE "KothControlResults"
                      ADD CONSTRAINT fk_kothcontrol_token
                      FOREIGN KEY (token_id) REFERENCES "KothTokens"(id)
                      ON DELETE SET NULL;
                  END IF;
                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'fk_kothcontrol_provisional'
                       AND conrelid = '"KothControlResults"'::regclass
                  ) THEN
                    ALTER TABLE "KothControlResults"
                      ADD CONSTRAINT fk_kothcontrol_provisional
                      FOREIGN KEY (provisional_participation_id)
                      REFERENCES "Participations"(id) ON DELETE SET NULL;
                  END IF;
                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'fk_kothcontrol_confirmed'
                       AND conrelid = '"KothControlResults"'::regclass
                  ) THEN
                    ALTER TABLE "KothControlResults"
                      ADD CONSTRAINT fk_kothcontrol_confirmed
                      FOREIGN KEY (confirmed_participation_id)
                      REFERENCES "Participations"(id) ON DELETE SET NULL;
                  END IF;
                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'ck_kothcontrol_v2_identity'
                       AND conrelid = '"KothControlResults"'::regclass
                  ) THEN
                    ALTER TABLE "KothControlResults"
                      ADD CONSTRAINT ck_kothcontrol_v2_identity CHECK (
                        cycle_id IS NULL OR (
                          confirmation_streak IS NOT NULL AND confirmation_streak >= 0
                          AND ((is_scorable AND NULLIF(BTRIM(container_id), '') IS NOT NULL)
                               OR (NOT is_scorable AND void_reason IS NOT NULL))
                          AND (token_id IS NULL OR token_window_round >= 1)
                        )
                      );
                  END IF;
                END
                $$;
                CREATE INDEX IF NOT EXISTS ix_kothcontrol_cycle_round
                  ON "KothControlResults"(cycle_id, ad_round_id)
                  WHERE cycle_id IS NOT NULL;
                CREATE INDEX IF NOT EXISTS ix_kothcontrol_token
                  ON "KothControlResults"(token_id, ad_round_id)
                  WHERE token_id IS NOT NULL;
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
                DROP INDEX IF EXISTS ix_kothcontrol_token;
                DROP INDEX IF EXISTS ix_kothcontrol_cycle_round;
                ALTER TABLE "KothControlResults"
                  DROP CONSTRAINT IF EXISTS ck_kothcontrol_v2_identity,
                  DROP CONSTRAINT IF EXISTS fk_kothcontrol_token_cycle,
                  DROP CONSTRAINT IF EXISTS fk_kothcontrol_cycle_challenge,
                  DROP CONSTRAINT IF EXISTS fk_kothcontrol_confirmed,
                  DROP CONSTRAINT IF EXISTS fk_kothcontrol_provisional,
                  DROP CONSTRAINT IF EXISTS fk_kothcontrol_token,
                  DROP CONSTRAINT IF EXISTS fk_kothcontrol_cycle;
                DROP TABLE IF EXISTS "KothAcquisitions";
                ALTER TABLE "KothControlResults"
                  DROP COLUMN IF EXISTS void_reason,
                  DROP COLUMN IF EXISTS is_scorable,
                  DROP COLUMN IF EXISTS confirmation_streak,
                  DROP COLUMN IF EXISTS confirmed_participation_id,
                  DROP COLUMN IF EXISTS provisional_participation_id,
                  DROP COLUMN IF EXISTS token_window_round,
                  DROP COLUMN IF EXISTS token_id,
                  DROP COLUMN IF EXISTS container_id,
                  DROP COLUMN IF EXISTS cycle_id;
                DROP TABLE IF EXISTS "KothClaimStates";
                ALTER TABLE "KothTokens"
                  DROP CONSTRAINT IF EXISTS ck_kothtokens_v2_scope,
                  DROP CONSTRAINT IF EXISTS fk_kothtokens_target_challenge,
                  DROP CONSTRAINT IF EXISTS fk_kothtokens_cycle_challenge,
                  DROP CONSTRAINT IF EXISTS fk_kothtokens_challenge,
                  DROP CONSTRAINT IF EXISTS fk_kothtokens_cycle;
                DROP INDEX IF EXISTS ux_kothtokens_v2_cycle_token;
                DROP INDEX IF EXISTS ux_kothtokens_v2_cycle_challenge_part;
                DROP INDEX IF EXISTS ux_kothtokens_id_cycle;
                DROP INDEX IF EXISTS ux_kothtokens_part_round_mint;
                DROP INDEX IF EXISTS ux_kothtokens_part_window;
                ALTER TABLE "KothTokens"
                  DROP COLUMN IF EXISTS challenge_id,
                  DROP COLUMN IF EXISTS cycle_id;
                CREATE UNIQUE INDEX IF NOT EXISTS ux_kothtokens_part_window
                  ON "KothTokens"(participation_id, round_number)
                  WHERE round_number IS NOT NULL;
                CREATE UNIQUE INDEX IF NOT EXISTS ux_kothtokens_part_round_mint
                  ON "KothTokens"(participation_id, round_number, ad_round_id)
                  WHERE round_number IS NOT NULL AND ad_round_id IS NOT NULL;
                DROP TABLE IF EXISTS "KothCycleAuditReceipts";
                DROP TABLE IF EXISTS "KothCycleCooldowns";
                DROP TABLE IF EXISTS "KothCrownCycles";
                DROP TABLE IF EXISTS "KothOfficialConfigs";
                DROP INDEX IF EXISTS ux_kothtargets_id_challenge;
                ALTER TABLE "Games"
                  DROP CONSTRAINT IF EXISTS ck_games_koth_crown_config,
                  DROP COLUMN IF EXISTS koth_claim_confirmation_ticks,
                  DROP COLUMN IF EXISTS koth_champion_cooldown_ticks,
                  DROP COLUMN IF EXISTS koth_cycle_ticks,
                  DROP COLUMN IF EXISTS koth_epoch_ticks;
                ALTER TABLE "Games"
                  ALTER COLUMN koth_scoring_formula_version SET DEFAULT 1;
                "#,
            )
            .await?;
        Ok(())
    }
}
