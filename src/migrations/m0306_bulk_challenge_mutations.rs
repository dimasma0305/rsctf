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
DECLARE affected_game_id INTEGER;
BEGIN
    affected_game_id := CASE WHEN TG_OP = 'DELETE' THEN OLD.game_id ELSE NEW.game_id END;
    UPDATE "Games"
       SET challenge_configuration_revision = challenge_configuration_revision + 1
     WHERE id = affected_game_id;
    IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
    RETURN NEW;
END $$;

DROP TRIGGER IF EXISTS tr_game_challenge_configuration_revision ON "GameChallenges";
CREATE TRIGGER tr_game_challenge_configuration_revision
AFTER INSERT OR DELETE OR UPDATE OF is_enabled, deletion_pending, review_status, "Type", ad_self_hosted
ON "GameChallenges" FOR EACH ROW
EXECUTE FUNCTION bump_challenge_configuration_revision();

CREATE OR REPLACE FUNCTION bump_flag_challenge_configuration_revision()
RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE affected_challenge_id INTEGER;
BEGIN
    affected_challenge_id := CASE
        WHEN TG_OP = 'DELETE' THEN OLD.challenge_id ELSE NEW.challenge_id
    END;
    UPDATE "Games" game
       SET challenge_configuration_revision = game.challenge_configuration_revision + 1
      FROM "GameChallenges" challenge
     WHERE challenge.id = affected_challenge_id AND game.id = challenge.game_id;
    IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
    RETURN NEW;
END $$;

DROP TRIGGER IF EXISTS tr_flag_challenge_configuration_revision ON "FlagContexts";
CREATE TRIGGER tr_flag_challenge_configuration_revision
AFTER INSERT OR DELETE OR UPDATE OF flag, challenge_id ON "FlagContexts"
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

CREATE INDEX IF NOT EXISTS ix_bulk_challenge_mutations_retention
    ON "BulkChallengeMutationOperations" (completed_at_utc, game_id, operation_id)
    WHERE state = 2;
CREATE INDEX IF NOT EXISTS ix_bulk_challenge_mutations_recovery
    ON "BulkChallengeMutationOperations" (lease_expires_at_utc, game_id, operation_id)
    WHERE state = 1;
"#;

const DOWN_SQL: &str = r#"
DROP TABLE IF EXISTS "BulkChallengeMutationOperations";
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
    }
}
