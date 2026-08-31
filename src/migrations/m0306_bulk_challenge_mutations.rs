//! Durable identity and configuration revisions for bounded challenge batches.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "Games"
    ADD COLUMN IF NOT EXISTS challenge_configuration_revision BIGINT NOT NULL DEFAULT 1;

DO $$ BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_games_challenge_configuration_revision'
           AND conrelid = '"Games"'::regclass
    ) THEN
        ALTER TABLE "Games" ADD CONSTRAINT ck_games_challenge_configuration_revision
            CHECK (challenge_configuration_revision >= 1);
    END IF;
END $$;

CREATE OR REPLACE FUNCTION bump_challenge_configuration_revision()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' AND OLD IS NOT DISTINCT FROM NEW THEN
        RETURN NEW;
    END IF;
    IF TG_OP = 'INSERT' THEN
        UPDATE "Games"
           SET challenge_configuration_revision = challenge_configuration_revision + 1
         WHERE id = NEW.game_id;
    ELSIF TG_OP = 'DELETE' THEN
        UPDATE "Games"
           SET challenge_configuration_revision = challenge_configuration_revision + 1
         WHERE id = OLD.game_id;
        RETURN OLD;
    ELSE
        UPDATE "Games" game
           SET challenge_configuration_revision = game.challenge_configuration_revision + 1
         WHERE game.id IN (OLD.game_id, NEW.game_id);
    END IF;
    RETURN NEW;
END $$;

DROP TRIGGER IF EXISTS tr_game_challenge_configuration_revision ON "GameChallenges";
CREATE TRIGGER tr_game_challenge_configuration_revision
AFTER INSERT OR DELETE OR UPDATE OF
    game_id, title, content, category, "Type", hints, is_enabled,
    deletion_pending, deadline_utc, submission_limit, container_image,
    memory_limit, storage_limit, cpu_count, expose_port, workload_spec,
    file_name, flag_template, review_status, review_note, attachment_id,
    enable_traffic_capture, enable_shared_container, disable_blood_bonus,
    original_score, min_score_rate, difficulty, score_curve, network_mode,
    variant_mode, variant_generator_build_context_subdir, solve_receipt_mode,
    receipt_verifier_identity, ad_checker_image, ad_allow_egress,
    ad_allow_self_reset, ad_ssh_requires_flag, ad_self_hosted,
    ad_scoring_weight
ON "GameChallenges" FOR EACH ROW
EXECUTE FUNCTION bump_challenge_configuration_revision();

CREATE OR REPLACE FUNCTION bump_flag_challenge_configuration_revision()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' AND OLD IS NOT DISTINCT FROM NEW THEN
        RETURN NEW;
    END IF;
    IF TG_OP = 'INSERT' THEN
        IF NEW.is_occupied = FALSE THEN
            UPDATE "Games" game
               SET challenge_configuration_revision = game.challenge_configuration_revision + 1
              FROM "GameChallenges" challenge
             WHERE challenge.id = NEW.challenge_id AND game.id = challenge.game_id;
        END IF;
    ELSIF TG_OP = 'DELETE' THEN
        IF OLD.is_occupied = FALSE THEN
            UPDATE "Games" game
               SET challenge_configuration_revision = game.challenge_configuration_revision + 1
              FROM "GameChallenges" challenge
             WHERE challenge.id = OLD.challenge_id AND game.id = challenge.game_id;
        END IF;
        RETURN OLD;
    ELSE
        UPDATE "Games" game
           SET challenge_configuration_revision = game.challenge_configuration_revision + 1
         WHERE game.id IN (
             SELECT challenge.game_id
               FROM "GameChallenges" challenge
              WHERE OLD.is_occupied = FALSE AND challenge.id = OLD.challenge_id
             UNION
             SELECT challenge.game_id
               FROM "GameChallenges" challenge
              WHERE NEW.is_occupied = FALSE AND challenge.id = NEW.challenge_id
         );
    END IF;
    RETURN NEW;
END $$;

DROP TRIGGER IF EXISTS tr_flag_challenge_configuration_revision ON "FlagContexts";
CREATE TRIGGER tr_flag_challenge_configuration_revision
AFTER INSERT OR DELETE OR UPDATE OF flag, challenge_id, is_occupied ON "FlagContexts"
FOR EACH ROW EXECUTE FUNCTION bump_flag_challenge_configuration_revision();

CREATE TABLE IF NOT EXISTS "BulkChallengeMutationOperations" (
    game_id            INTEGER NOT NULL,
    operation_id       UUID NOT NULL,
    actor_user_id      UUID NOT NULL,
    expected_revision  BIGINT NOT NULL CHECK (expected_revision >= 1),
    action             SMALLINT NOT NULL CHECK (action IN (0, 1, 2)),
    challenge_ids      INTEGER[] NOT NULL
        CHECK (CARDINALITY(challenge_ids) BETWEEN 1 AND 100),
    request_digest     BYTEA NOT NULL CHECK (OCTET_LENGTH(request_digest) = 32),
    state              SMALLINT NOT NULL DEFAULT 0 CHECK (state IN (0, 1, 2)),
    result             JSONB NOT NULL DEFAULT '[]'::jsonb
        CHECK (jsonb_typeof(result) = 'array' AND jsonb_array_length(result) <= 100),
    effects            JSONB NOT NULL DEFAULT '{}'::jsonb
        CHECK (jsonb_typeof(effects) = 'object' AND OCTET_LENGTH(effects::text) <= 65536),
    cleanup_completed_ids INTEGER[] NOT NULL DEFAULT '{}'::integer[]
        CHECK (CARDINALITY(cleanup_completed_ids) <= 100),
    effect_progress    SMALLINT NOT NULL DEFAULT 0 CHECK (effect_progress BETWEEN 0 AND 7),
    result_revision    BIGINT NULL CHECK (result_revision IS NULL OR result_revision >= 1),
    lease_token        UUID NULL,
    lease_expires_at_utc TIMESTAMPTZ NOT NULL
        DEFAULT (clock_timestamp() + INTERVAL '5 minutes'),
    completed_at_utc   TIMESTAMPTZ NULL,
    created_at_utc     TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (game_id, operation_id),
    CONSTRAINT fk_bulk_challenge_mutation_game
        FOREIGN KEY (game_id) REFERENCES "Games"(id) ON DELETE CASCADE,
    CONSTRAINT fk_bulk_challenge_mutation_actor
        FOREIGN KEY (actor_user_id) REFERENCES "AspNetUsers"(id) ON DELETE RESTRICT,
    CONSTRAINT ck_bulk_challenge_mutation_completion
        CHECK ((state = 2 AND completed_at_utc IS NOT NULL AND result_revision IS NOT NULL)
               OR (state <> 2 AND completed_at_utc IS NULL)),
    CONSTRAINT ck_bulk_challenge_mutation_lease
        CHECK ((state = 1 AND lease_token IS NOT NULL)
               OR (state <> 1 AND lease_token IS NULL))
);

ALTER TABLE "BulkChallengeMutationOperations"
    ADD COLUMN IF NOT EXISTS effects JSONB NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN IF NOT EXISTS cleanup_completed_ids INTEGER[] NOT NULL DEFAULT '{}'::integer[],
    ADD COLUMN IF NOT EXISTS effect_progress SMALLINT NOT NULL DEFAULT 0;

DO $$ BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_bulk_challenge_mutation_effects'
           AND conrelid = '"BulkChallengeMutationOperations"'::regclass
    ) THEN
        ALTER TABLE "BulkChallengeMutationOperations"
            ADD CONSTRAINT ck_bulk_challenge_mutation_effects
            CHECK (jsonb_typeof(effects) = 'object' AND OCTET_LENGTH(effects::text) <= 65536);
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_bulk_challenge_mutation_cleanup_progress'
           AND conrelid = '"BulkChallengeMutationOperations"'::regclass
    ) THEN
        ALTER TABLE "BulkChallengeMutationOperations"
            ADD CONSTRAINT ck_bulk_challenge_mutation_cleanup_progress
            CHECK (CARDINALITY(cleanup_completed_ids) <= 100 AND effect_progress BETWEEN 0 AND 7);
    END IF;
END $$;

CREATE INDEX IF NOT EXISTS ix_bulk_challenge_mutations_retention
    ON "BulkChallengeMutationOperations" (completed_at_utc, game_id, operation_id)
    WHERE state = 2;
DROP INDEX IF EXISTS ix_bulk_challenge_mutations_recovery;
CREATE INDEX ix_bulk_challenge_mutations_recovery
    ON "BulkChallengeMutationOperations" (lease_expires_at_utc, game_id, operation_id)
    WHERE state IN (0, 1);
CREATE INDEX IF NOT EXISTS ix_bulk_challenge_mutations_abandoned
    ON "BulkChallengeMutationOperations" (created_at_utc, lease_expires_at_utc,
                                           game_id, operation_id)
    WHERE state IN (0, 1);

CREATE TABLE IF NOT EXISTS "BulkChallengeDeletionSlots" (
    slot_id          SMALLINT PRIMARY KEY CHECK (slot_id BETWEEN 0 AND 1),
    lease_token      UUID NULL,
    expires_at_utc   TIMESTAMPTZ NULL,
    CHECK ((lease_token IS NULL) = (expires_at_utc IS NULL))
);
INSERT INTO "BulkChallengeDeletionSlots" (slot_id)
VALUES (0), (1)
ON CONFLICT DO NOTHING;
CREATE INDEX IF NOT EXISTS ix_bulk_challenge_deletion_slot_expiry
    ON "BulkChallengeDeletionSlots" (expires_at_utc, slot_id);

CREATE TABLE IF NOT EXISTS "BulkChallengeDesiredStateSlots" (
    slot_id          SMALLINT PRIMARY KEY CHECK (slot_id BETWEEN 0 AND 3),
    lease_token      UUID NULL,
    expires_at_utc   TIMESTAMPTZ NULL,
    CHECK ((lease_token IS NULL) = (expires_at_utc IS NULL))
);
INSERT INTO "BulkChallengeDesiredStateSlots" (slot_id)
VALUES (0), (1), (2), (3)
ON CONFLICT DO NOTHING;
CREATE INDEX IF NOT EXISTS ix_bulk_challenge_desired_slot_expiry
    ON "BulkChallengeDesiredStateSlots" (expires_at_utc, slot_id);

ALTER TABLE "GameNotices"
    ADD COLUMN IF NOT EXISTS bulk_operation_id UUID NULL;
CREATE UNIQUE INDEX IF NOT EXISTS ux_game_notices_bulk_operation
    ON "GameNotices" (game_id, bulk_operation_id)
    WHERE bulk_operation_id IS NOT NULL;
"#;

const DOWN_SQL: &str = r#"
DROP TABLE IF EXISTS "BulkChallengeMutationOperations";
DROP TABLE IF EXISTS "BulkChallengeDeletionSlots";
DROP TABLE IF EXISTS "BulkChallengeDesiredStateSlots";
DROP INDEX IF EXISTS ux_game_notices_bulk_operation;
ALTER TABLE "GameNotices" DROP COLUMN IF EXISTS bulk_operation_id;
DROP TRIGGER IF EXISTS tr_flag_challenge_configuration_revision ON "FlagContexts";
DROP FUNCTION IF EXISTS bump_flag_challenge_configuration_revision();
DROP TRIGGER IF EXISTS tr_game_challenge_configuration_revision ON "GameChallenges";
DROP FUNCTION IF EXISTS bump_challenge_configuration_revision();
ALTER TABLE "Games"
    DROP CONSTRAINT IF EXISTS ck_games_challenge_configuration_revision,
    DROP COLUMN IF EXISTS challenge_configuration_revision;
"#;

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
    use super::UP_SQL;

    #[test]
    fn batch_identity_and_work_are_strictly_bounded() {
        assert!(UP_SQL.contains("PRIMARY KEY (game_id, operation_id)"));
        assert!(UP_SQL.contains("CARDINALITY(challenge_ids) BETWEEN 1 AND 100"));
        assert!(UP_SQL.contains("jsonb_array_length(result) <= 100"));
        assert!(UP_SQL.contains("ix_bulk_challenge_mutations_abandoned"));
        assert!(UP_SQL.contains("BulkChallengeDeletionSlots"));
        assert!(UP_SQL.contains("BulkChallengeDesiredStateSlots"));
        assert!(UP_SQL.contains("OCTET_LENGTH(effects::text) <= 65536"));
        assert!(UP_SQL.contains("CARDINALITY(cleanup_completed_ids) <= 100"));
    }
}
