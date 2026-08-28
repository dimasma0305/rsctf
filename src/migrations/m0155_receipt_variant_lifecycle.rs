use sea_orm_migration::prelude::*;

pub const UP_SQL: &str = r#"
ALTER TABLE "SolveReceipts" ADD COLUMN IF NOT EXISTS attempt_hash BYTEA;
UPDATE "SolveReceipts"
   SET attempt_hash = sha256(convert_to(id::text, 'UTF8'))
 WHERE attempt_hash IS NULL;
ALTER TABLE "SolveReceipts" ALTER COLUMN attempt_hash SET NOT NULL;
DO $$ BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_solve_receipt_attempt_hash'
           AND conrelid = '"SolveReceipts"'::regclass
    ) THEN
        ALTER TABLE "SolveReceipts" ADD CONSTRAINT ck_solve_receipt_attempt_hash
            CHECK (octet_length(attempt_hash) = 32);
    END IF;
END $$;
CREATE UNIQUE INDEX IF NOT EXISTS ux_solve_receipts_attempt
    ON "SolveReceipts"(attempt_hash);
CREATE INDEX IF NOT EXISTS ix_solve_receipts_expiry
    ON "SolveReceipts"(expires_at_utc, id)
    WHERE consumed_at_utc IS NULL;

CREATE TABLE IF NOT EXISTS "SolveReceiptAudit" (
    receipt_id UUID PRIMARY KEY,
    game_id INTEGER NOT NULL,
    challenge_id INTEGER NOT NULL,
    participation_id INTEGER NOT NULL,
    user_id UUID,
    variant_id UUID,
    issuer_identity TEXT NOT NULL,
    attempt_hash BYTEA NOT NULL CHECK (octet_length(attempt_hash) = 32),
    token_hash BYTEA NOT NULL CHECK (octet_length(token_hash) = 32),
    consumed_submission_id INTEGER NOT NULL,
    issued_at_utc TIMESTAMPTZ NOT NULL,
    consumed_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

DROP TRIGGER IF EXISTS tr_solve_receipts_immutable ON "SolveReceipts";
DROP TRIGGER IF EXISTS tr_challenge_variants_immutable ON "ChallengeVariants";

CREATE OR REPLACE FUNCTION guard_solve_receipt_mutation()
RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    token_finalization BOOLEAN;
    legacy_consumption BOOLEAN;
BEGIN
    IF OLD.id IS DISTINCT FROM NEW.id
       OR OLD.game_id IS DISTINCT FROM NEW.game_id
       OR OLD.challenge_id IS DISTINCT FROM NEW.challenge_id
       OR OLD.participation_id IS DISTINCT FROM NEW.participation_id
       OR OLD.user_id IS DISTINCT FROM NEW.user_id
       OR OLD.variant_id IS DISTINCT FROM NEW.variant_id
       OR OLD.answer_hash IS DISTINCT FROM NEW.answer_hash
       OR OLD.issuer_identity IS DISTINCT FROM NEW.issuer_identity
       OR OLD.nonce_hash IS DISTINCT FROM NEW.nonce_hash
       OR OLD.attempt_hash IS DISTINCT FROM NEW.attempt_hash
       OR OLD.issued_at_utc IS DISTINCT FROM NEW.issued_at_utc
       OR OLD.expires_at_utc IS DISTINCT FROM NEW.expires_at_utc
    THEN
        RAISE EXCEPTION 'SolveReceipts provenance is immutable' USING ERRCODE = '55000';
    END IF;

    -- New issuance first inserts an opaque attempt placeholder, then replaces
    -- only token_hash after reconstructing the DB-timestamped proof.
    token_finalization := OLD.token_hash = OLD.attempt_hash
        AND NEW.token_hash IS DISTINCT FROM OLD.token_hash
        AND OLD.consumed_at_utc IS NOT DISTINCT FROM NEW.consumed_at_utc
        AND OLD.consumed_submission_id IS NOT DISTINCT FROM NEW.consumed_submission_id;
    -- Retain the shipped one-time transition during rolling upgrades. New
    -- code archives and deletes atomically instead.
    legacy_consumption := OLD.token_hash = NEW.token_hash
        AND OLD.consumed_at_utc IS NULL
        AND OLD.consumed_submission_id IS NULL
        AND NEW.consumed_at_utc IS NOT NULL
        AND NEW.consumed_submission_id IS NOT NULL;

    IF token_finalization OR legacy_consumption OR OLD IS NOT DISTINCT FROM NEW THEN
        RETURN NEW;
    END IF;
    RAISE EXCEPTION 'SolveReceipts provenance is immutable' USING ERRCODE = '55000';
END;
$$;

CREATE TRIGGER tr_solve_receipts_immutable
    BEFORE UPDATE ON "SolveReceipts"
    FOR EACH ROW EXECUTE FUNCTION guard_solve_receipt_mutation();
CREATE TRIGGER tr_challenge_variants_immutable
    BEFORE UPDATE ON "ChallengeVariants"
    FOR EACH ROW EXECUTE FUNCTION guard_challenge_variant_mutation();

INSERT INTO "SolveReceiptAudit"
    (receipt_id, game_id, challenge_id, participation_id, user_id, variant_id,
     issuer_identity, attempt_hash, token_hash, consumed_submission_id,
     issued_at_utc, consumed_at_utc)
SELECT id, game_id, challenge_id, participation_id, user_id, variant_id,
       issuer_identity, attempt_hash, token_hash, consumed_submission_id,
       issued_at_utc, consumed_at_utc
  FROM "SolveReceipts" WHERE consumed_at_utc IS NOT NULL
ON CONFLICT (receipt_id) DO NOTHING;
DELETE FROM "SolveReceipts" WHERE consumed_at_utc IS NOT NULL;

ALTER TABLE "SolveReceipts"
    DROP CONSTRAINT IF EXISTS fk_solve_receipt_challenge,
    DROP CONSTRAINT IF EXISTS fk_solve_receipt_participation,
    DROP CONSTRAINT IF EXISTS fk_solve_receipt_variant,
    DROP CONSTRAINT IF EXISTS fk_solve_receipt_submission;
ALTER TABLE "SolveReceipts"
    ADD CONSTRAINT fk_solve_receipt_challenge FOREIGN KEY (game_id, challenge_id)
        REFERENCES "GameChallenges"(game_id, id) ON DELETE CASCADE,
    ADD CONSTRAINT fk_solve_receipt_participation FOREIGN KEY (game_id, participation_id)
        REFERENCES "Participations"(game_id, id) ON DELETE CASCADE,
    ADD CONSTRAINT fk_solve_receipt_variant FOREIGN KEY (variant_id)
        REFERENCES "ChallengeVariants"(id) ON DELETE CASCADE,
    ADD CONSTRAINT fk_solve_receipt_submission FOREIGN KEY (consumed_submission_id)
        REFERENCES "Submissions"(id) ON DELETE SET NULL;

ALTER TABLE "ChallengeVariants"
    DROP CONSTRAINT IF EXISTS fk_challenge_variant_challenge,
    DROP CONSTRAINT IF EXISTS fk_challenge_variant_participation;
ALTER TABLE "ChallengeVariants"
    ADD CONSTRAINT fk_challenge_variant_challenge FOREIGN KEY (game_id, challenge_id)
        REFERENCES "GameChallenges"(game_id, id) ON DELETE CASCADE,
    ADD CONSTRAINT fk_challenge_variant_participation FOREIGN KEY (game_id, participation_id)
        REFERENCES "Participations"(game_id, id) ON DELETE CASCADE;
"#;

const DOWN_SQL: &str = r#"
DROP TABLE IF EXISTS "SolveReceiptAudit";
DROP INDEX IF EXISTS ix_solve_receipts_expiry;
DROP INDEX IF EXISTS ux_solve_receipts_attempt;
ALTER TABLE "SolveReceipts" DROP CONSTRAINT IF EXISTS ck_solve_receipt_attempt_hash;
ALTER TABLE "SolveReceipts" DROP COLUMN IF EXISTS attempt_hash;
"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

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
    fn receipts_are_idempotent_bounded_and_do_not_block_parent_deletion() {
        assert!(UP_SQL.contains("ux_solve_receipts_attempt"));
        assert!(UP_SQL.contains("SolveReceiptAudit"));
        assert!(UP_SQL.contains("ON DELETE CASCADE"));
        assert!(UP_SQL.contains("ix_solve_receipts_expiry"));
        assert!(UP_SQL.contains("BEFORE UPDATE ON \"SolveReceipts\""));
        assert!(!UP_SQL.contains("BEFORE UPDATE OR DELETE ON \"SolveReceipts\""));
        assert!(UP_SQL.contains("BEFORE UPDATE ON \"ChallengeVariants\""));
    }
}
