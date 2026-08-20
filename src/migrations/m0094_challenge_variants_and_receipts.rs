//! Deterministic per-participation challenge variants and one-use solve receipts.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP_SQL: &str = r#"
ALTER TABLE "GameChallenges"
    ADD COLUMN IF NOT EXISTS variant_mode SMALLINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS variant_generator_image TEXT NULL,
    ADD COLUMN IF NOT EXISTS variant_generator_digest TEXT NULL,
    ADD COLUMN IF NOT EXISTS solve_receipt_mode SMALLINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS receipt_verifier_identity TEXT NULL;

-- See m0092: entity-derived fresh schemas can already have these columns when
-- this migration runs, so make the disabled defaults explicit and idempotent.
ALTER TABLE "GameChallenges"
    ALTER COLUMN variant_mode SET DEFAULT 0,
    ALTER COLUMN solve_receipt_mode SET DEFAULT 0;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_game_challenges_variant_mode'
           AND conrelid = '"GameChallenges"'::regclass
    ) THEN
        ALTER TABLE "GameChallenges"
            ADD CONSTRAINT ck_game_challenges_variant_mode
            CHECK (variant_mode BETWEEN 0 AND 1);
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_game_challenges_receipt_mode'
           AND conrelid = '"GameChallenges"'::regclass
    ) THEN
        ALTER TABLE "GameChallenges"
            ADD CONSTRAINT ck_game_challenges_receipt_mode
            CHECK (solve_receipt_mode BETWEEN 0 AND 2);
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_game_challenges_variant_config'
           AND conrelid = '"GameChallenges"'::regclass
    ) THEN
        ALTER TABLE "GameChallenges"
            ADD CONSTRAINT ck_game_challenges_variant_config CHECK (
                variant_mode = 0
                OR (
                    variant_generator_image IS NOT NULL
                    AND LENGTH(BTRIM(variant_generator_image)) BETWEEN 1 AND 512
                    AND variant_generator_digest ~ '^sha256:[0-9a-f]{64}$'
                )
            );
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_game_challenges_receipt_config'
           AND conrelid = '"GameChallenges"'::regclass
    ) THEN
        ALTER TABLE "GameChallenges"
            ADD CONSTRAINT ck_game_challenges_receipt_config CHECK (
                solve_receipt_mode = 0
                OR LENGTH(BTRIM(receipt_verifier_identity)) BETWEEN 1 AND 128
            );
    END IF;
END $$;

CREATE TABLE IF NOT EXISTS "ChallengeVariants" (
    id UUID PRIMARY KEY,
    game_id INTEGER NOT NULL,
    challenge_id INTEGER NOT NULL,
    participation_id INTEGER NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 1),
    generator_image TEXT NOT NULL,
    generator_digest TEXT NOT NULL CHECK (generator_digest ~ '^sha256:[0-9a-f]{64}$'),
    seed_hash BYTEA NOT NULL CHECK (OCTET_LENGTH(seed_hash) = 32),
    manifest JSONB NOT NULL CHECK (jsonb_typeof(manifest) = 'object'),
    artifact_hash BYTEA NOT NULL CHECK (OCTET_LENGTH(artifact_hash) = 32),
    determinism_hash BYTEA NOT NULL CHECK (OCTET_LENGTH(determinism_hash) = 32),
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    frozen_at_utc TIMESTAMPTZ NULL,
    CONSTRAINT fk_challenge_variant_challenge
        FOREIGN KEY (game_id, challenge_id)
        REFERENCES "GameChallenges"(game_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_challenge_variant_participation
        FOREIGN KEY (game_id, participation_id)
        REFERENCES "Participations"(game_id, id) ON DELETE RESTRICT
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_challenge_variant_revision
    ON "ChallengeVariants"(game_id, challenge_id, participation_id, revision);
CREATE UNIQUE INDEX IF NOT EXISTS ux_challenge_variant_frozen
    ON "ChallengeVariants"(game_id, challenge_id, participation_id)
    WHERE frozen_at_utc IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_challenge_variants_game
    ON "ChallengeVariants"(game_id, participation_id, challenge_id);

CREATE TABLE IF NOT EXISTS "SolveReceipts" (
    id UUID PRIMARY KEY,
    game_id INTEGER NOT NULL,
    challenge_id INTEGER NOT NULL,
    participation_id INTEGER NOT NULL,
    user_id UUID NULL,
    variant_id UUID NULL,
    answer_hash BYTEA NOT NULL CHECK (OCTET_LENGTH(answer_hash) = 32),
    issuer_identity TEXT NOT NULL CHECK (LENGTH(BTRIM(issuer_identity)) BETWEEN 1 AND 128),
    token_hash BYTEA NOT NULL UNIQUE CHECK (OCTET_LENGTH(token_hash) = 32),
    nonce_hash BYTEA NOT NULL UNIQUE CHECK (OCTET_LENGTH(nonce_hash) = 32),
    issued_at_utc TIMESTAMPTZ NOT NULL,
    expires_at_utc TIMESTAMPTZ NOT NULL,
    consumed_at_utc TIMESTAMPTZ NULL,
    consumed_submission_id INTEGER NULL,
    CONSTRAINT fk_solve_receipt_challenge
        FOREIGN KEY (game_id, challenge_id)
        REFERENCES "GameChallenges"(game_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_solve_receipt_participation
        FOREIGN KEY (game_id, participation_id)
        REFERENCES "Participations"(game_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_solve_receipt_variant
        FOREIGN KEY (variant_id) REFERENCES "ChallengeVariants"(id) ON DELETE RESTRICT,
    CONSTRAINT fk_solve_receipt_submission
        FOREIGN KEY (consumed_submission_id) REFERENCES "Submissions"(id) ON DELETE RESTRICT,
    CONSTRAINT ck_solve_receipt_lifetime CHECK (
        expires_at_utc > issued_at_utc
        AND expires_at_utc <= issued_at_utc + INTERVAL '10 minutes'
    ),
    CONSTRAINT ck_solve_receipt_consumption CHECK (
        (consumed_at_utc IS NULL AND consumed_submission_id IS NULL)
        OR
        (consumed_at_utc IS NOT NULL AND consumed_submission_id IS NOT NULL)
    )
);

CREATE INDEX IF NOT EXISTS ix_solve_receipts_lookup
    ON "SolveReceipts"(game_id, challenge_id, participation_id, expires_at_utc)
    WHERE consumed_at_utc IS NULL;

CREATE OR REPLACE FUNCTION guard_challenge_variant_mutation()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'ChallengeVariants cannot be deleted' USING ERRCODE = '55000';
    END IF;
    IF OLD.id IS DISTINCT FROM NEW.id
       OR OLD.game_id IS DISTINCT FROM NEW.game_id
       OR OLD.challenge_id IS DISTINCT FROM NEW.challenge_id
       OR OLD.participation_id IS DISTINCT FROM NEW.participation_id
       OR OLD.revision IS DISTINCT FROM NEW.revision
       OR OLD.generator_image IS DISTINCT FROM NEW.generator_image
       OR OLD.generator_digest IS DISTINCT FROM NEW.generator_digest
       OR OLD.seed_hash IS DISTINCT FROM NEW.seed_hash
       OR OLD.manifest IS DISTINCT FROM NEW.manifest
       OR OLD.artifact_hash IS DISTINCT FROM NEW.artifact_hash
       OR OLD.determinism_hash IS DISTINCT FROM NEW.determinism_hash
       OR OLD.created_at_utc IS DISTINCT FROM NEW.created_at_utc
       OR OLD.frozen_at_utc IS NOT NULL
       OR NEW.frozen_at_utc IS NULL
    THEN
        RAISE EXCEPTION 'ChallengeVariants provenance is immutable' USING ERRCODE = '55000';
    END IF;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION guard_solve_receipt_mutation()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'SolveReceipts cannot be deleted' USING ERRCODE = '55000';
    END IF;
    IF OLD.id IS DISTINCT FROM NEW.id
       OR OLD.game_id IS DISTINCT FROM NEW.game_id
       OR OLD.challenge_id IS DISTINCT FROM NEW.challenge_id
       OR OLD.participation_id IS DISTINCT FROM NEW.participation_id
       OR OLD.user_id IS DISTINCT FROM NEW.user_id
       OR OLD.variant_id IS DISTINCT FROM NEW.variant_id
       OR OLD.answer_hash IS DISTINCT FROM NEW.answer_hash
       OR OLD.issuer_identity IS DISTINCT FROM NEW.issuer_identity
       OR OLD.token_hash IS DISTINCT FROM NEW.token_hash
       OR OLD.nonce_hash IS DISTINCT FROM NEW.nonce_hash
       OR OLD.issued_at_utc IS DISTINCT FROM NEW.issued_at_utc
       OR OLD.expires_at_utc IS DISTINCT FROM NEW.expires_at_utc
       OR OLD.consumed_at_utc IS NOT NULL
       OR NEW.consumed_at_utc IS NULL
       OR NEW.consumed_submission_id IS NULL
    THEN
        RAISE EXCEPTION 'SolveReceipts provenance is immutable' USING ERRCODE = '55000';
    END IF;
    RETURN NEW;
END;
$$;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_trigger
         WHERE tgname = 'tr_challenge_variants_immutable'
           AND tgrelid = '"ChallengeVariants"'::regclass
    ) THEN
        CREATE TRIGGER tr_challenge_variants_immutable
        BEFORE UPDATE OR DELETE ON "ChallengeVariants"
        FOR EACH ROW EXECUTE FUNCTION guard_challenge_variant_mutation();
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_trigger
         WHERE tgname = 'tr_solve_receipts_immutable'
           AND tgrelid = '"SolveReceipts"'::regclass
    ) THEN
        CREATE TRIGGER tr_solve_receipts_immutable
        BEFORE UPDATE OR DELETE ON "SolveReceipts"
        FOR EACH ROW EXECUTE FUNCTION guard_solve_receipt_mutation();
    END IF;
END $$;
"#;

const DOWN_SQL: &str = r#"
DROP TRIGGER IF EXISTS tr_solve_receipts_immutable ON "SolveReceipts";
DROP TRIGGER IF EXISTS tr_challenge_variants_immutable ON "ChallengeVariants";
DROP FUNCTION IF EXISTS guard_solve_receipt_mutation();
DROP FUNCTION IF EXISTS guard_challenge_variant_mutation();
DROP TABLE IF EXISTS "SolveReceipts";
DROP TABLE IF EXISTS "ChallengeVariants";
ALTER TABLE "GameChallenges"
    DROP CONSTRAINT IF EXISTS ck_game_challenges_receipt_config,
    DROP CONSTRAINT IF EXISTS ck_game_challenges_variant_config,
    DROP CONSTRAINT IF EXISTS ck_game_challenges_receipt_mode,
    DROP CONSTRAINT IF EXISTS ck_game_challenges_variant_mode,
    DROP COLUMN IF EXISTS receipt_verifier_identity,
    DROP COLUMN IF EXISTS solve_receipt_mode,
    DROP COLUMN IF EXISTS variant_generator_digest,
    DROP COLUMN IF EXISTS variant_generator_image,
    DROP COLUMN IF EXISTS variant_mode;
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
    use super::*;

    #[test]
    fn variants_are_deterministic_and_receipts_are_short_lived_one_use() {
        assert!(UP_SQL.contains("ALTER COLUMN variant_mode SET DEFAULT 0"));
        assert!(UP_SQL.contains("ALTER COLUMN solve_receipt_mode SET DEFAULT 0"));
        assert!(UP_SQL.contains("determinism_hash BYTEA NOT NULL"));
        assert!(UP_SQL.contains("INTERVAL '10 minutes'"));
        assert!(UP_SQL.contains("consumed_at_utc IS NULL AND consumed_submission_id IS NULL"));
        assert!(UP_SQL.contains("SolveReceipts provenance is immutable"));
    }
}
