//! Agent-artifact cheat signals.
//!
//! Teams' solver uploads and writeups are scanned (never executed) for traces
//! that only AI agent tooling leaves behind, such as a coding agent's session
//! scratchpad path or commit trailers. A match is kept as review evidence and
//! raises the `AgentArtifact` suspicion rule; a match on a challenge the team
//! declared "No AI used" raises `AiDeclarationContradiction`. Built-in
//! signatures live in Rust so a release can correct them; this registry stores
//! only built-in enable overrides and custom signatures.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "SuspicionEvents" DROP CONSTRAINT IF EXISTS ck_suspicionevents_kind;
ALTER TABLE "SuspicionEvents"
    ADD CONSTRAINT ck_suspicionevents_kind CHECK (kind BETWEEN 0 AND 39);

CREATE TABLE IF NOT EXISTS "AgentArtifactSignatures" (
    signature_key TEXT PRIMARY KEY
        CHECK (signature_key ~ '^[a-z0-9][a-z0-9-]{0,39}$'),
    builtin BOOLEAN NOT NULL,
    label TEXT,
    pattern TEXT,
    enabled BOOLEAN NOT NULL,
    updated_by UUID REFERENCES "AspNetUsers"(id) ON DELETE SET NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT ck_agent_artifact_signatures_shape CHECK (
        (builtin AND label IS NULL AND pattern IS NULL)
        OR (NOT builtin
            AND char_length(label) BETWEEN 1 AND 64
            AND char_length(pattern) BETWEEN 1 AND 512)
    )
);

CREATE TABLE IF NOT EXISTS "AgentArtifactMatches" (
    id BIGSERIAL PRIMARY KEY,
    game_id INTEGER NOT NULL REFERENCES "Games"(id) ON DELETE CASCADE,
    participation_id INTEGER NOT NULL REFERENCES "Participations"(id) ON DELETE CASCADE,
    challenge_id INTEGER REFERENCES "GameChallenges"(id) ON DELETE CASCADE,
    source TEXT NOT NULL CHECK (source IN ('Solver', 'Writeup')),
    solver_upload_id BIGINT REFERENCES "SolverUploads"(id) ON DELETE CASCADE,
    file_name TEXT NOT NULL CHECK (char_length(file_name) BETWEEN 1 AND 255),
    sha256 BYTEA NOT NULL CHECK (octet_length(sha256) = 32),
    signature_key TEXT NOT NULL CHECK (char_length(signature_key) BETWEEN 1 AND 40),
    signature_label TEXT NOT NULL CHECK (char_length(signature_label) BETWEEN 1 AND 64),
    location TEXT NOT NULL CHECK (location IN ('Raw', 'Stream', 'Text')),
    byte_offset BIGINT NOT NULL CHECK (byte_offset >= 0),
    snippet TEXT NOT NULL CHECK (char_length(snippet) BETWEEN 1 AND 240),
    uploaded_by UUID REFERENCES "AspNetUsers"(id) ON DELETE SET NULL,
    uploaded_at TIMESTAMPTZ NOT NULL,
    scanned_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT ck_agent_artifact_matches_source CHECK (
        -- A restored archive keeps the file hash but not the upload row id.
        (source = 'Solver' AND challenge_id IS NOT NULL)
        OR (source = 'Writeup' AND solver_upload_id IS NULL)
    )
);
CREATE UNIQUE INDEX IF NOT EXISTS ux_agent_artifact_matches_evidence
    ON "AgentArtifactMatches"
       (participation_id, source, COALESCE(challenge_id, 0), sha256, signature_key);
CREATE INDEX IF NOT EXISTS ix_agent_artifact_matches_game
    ON "AgentArtifactMatches" (game_id, participation_id, challenge_id);
CREATE INDEX IF NOT EXISTS ix_agent_artifact_matches_challenge
    ON "AgentArtifactMatches" (challenge_id);
CREATE INDEX IF NOT EXISTS ix_agent_artifact_matches_solver
    ON "AgentArtifactMatches" (solver_upload_id);
CREATE INDEX IF NOT EXISTS ix_agent_artifact_matches_user
    ON "AgentArtifactMatches" (uploaded_by);
"#;

const DOWN_SQL: &str = r#"
DROP TABLE IF EXISTS "AgentArtifactMatches";
DROP TABLE IF EXISTS "AgentArtifactSignatures";
DELETE FROM "SuspicionEvents" WHERE kind IN (38, 39);
ALTER TABLE "SuspicionEvents" DROP CONSTRAINT IF EXISTS ck_suspicionevents_kind;
ALTER TABLE "SuspicionEvents"
    ADD CONSTRAINT ck_suspicionevents_kind CHECK (kind BETWEEN 0 AND 37);
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
    fn agent_artifact_migration_is_idempotent_and_extends_rule_kinds() {
        assert!(UP_SQL.contains("DROP CONSTRAINT IF EXISTS ck_suspicionevents_kind"));
        assert!(UP_SQL.contains("CHECK (kind BETWEEN 0 AND 39)"));
        assert!(UP_SQL.contains("CREATE TABLE IF NOT EXISTS \"AgentArtifactMatches\""));
        assert!(UP_SQL.contains("REFERENCES \"SolverUploads\"(id) ON DELETE CASCADE"));
        assert!(!UP_SQL.contains("CREATE TRIGGER"));
    }
}
