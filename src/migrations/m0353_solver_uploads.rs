//! Optional solver uploads on solved Jeopardy challenges.
//!
//! When an event enables it, a team may upload the script or notes it used to
//! solve a challenge so organizers can verify the solve. Files are small and
//! bounded, so they live in PostgreSQL with the rest of the review evidence:
//! purges and foreign-key cascades remove them in the same transaction, and
//! the competition archive carries them. Every upload is a new immutable
//! version; the server never unpacks, parses, or executes the content.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "Games"
    ADD COLUMN IF NOT EXISTS solver_uploads_enabled BOOLEAN NOT NULL DEFAULT FALSE;
-- Fresh installs already have the entity-derived column; keep its default.
ALTER TABLE "Games" ALTER COLUMN solver_uploads_enabled SET DEFAULT FALSE;

CREATE TABLE IF NOT EXISTS "SolverUploads" (
    id BIGSERIAL PRIMARY KEY,
    game_id INTEGER NOT NULL REFERENCES "Games"(id) ON DELETE CASCADE,
    participation_id INTEGER NOT NULL REFERENCES "Participations"(id) ON DELETE CASCADE,
    challenge_id INTEGER NOT NULL REFERENCES "GameChallenges"(id) ON DELETE CASCADE,
    version INTEGER NOT NULL CHECK (version BETWEEN 1 AND 10),
    file_name TEXT NOT NULL CHECK (char_length(file_name) BETWEEN 1 AND 128),
    size_bytes INTEGER NOT NULL CHECK (size_bytes BETWEEN 1 AND 1048576),
    sha256 BYTEA NOT NULL CHECK (octet_length(sha256) = 32),
    content BYTEA NOT NULL CHECK (octet_length(content) = size_bytes),
    uploaded_by UUID REFERENCES "AspNetUsers"(id) ON DELETE SET NULL,
    operation_id UUID NOT NULL,
    solved_at TIMESTAMPTZ,
    seconds_since_solve BIGINT,
    remote_ip_hash BYTEA CHECK (remote_ip_hash IS NULL OR octet_length(remote_ip_hash) = 32),
    uploaded_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
CREATE UNIQUE INDEX IF NOT EXISTS ux_solver_uploads_version
    ON "SolverUploads" (participation_id, challenge_id, version);
CREATE UNIQUE INDEX IF NOT EXISTS ux_solver_uploads_operation
    ON "SolverUploads" (participation_id, operation_id);
CREATE INDEX IF NOT EXISTS ix_solver_uploads_game
    ON "SolverUploads" (game_id, challenge_id, participation_id);
CREATE INDEX IF NOT EXISTS ix_solver_uploads_challenge
    ON "SolverUploads" (challenge_id);
CREATE INDEX IF NOT EXISTS ix_solver_uploads_user
    ON "SolverUploads" (uploaded_by);
"#;

const DOWN_SQL: &str = r#"
DROP TABLE IF EXISTS "SolverUploads";
ALTER TABLE "Games" DROP COLUMN IF EXISTS solver_uploads_enabled;
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
    fn solver_upload_migration_is_idempotent_and_defaults_off() {
        assert!(UP_SQL.contains(
            "ADD COLUMN IF NOT EXISTS solver_uploads_enabled BOOLEAN NOT NULL DEFAULT FALSE"
        ));
        assert!(UP_SQL.contains("ALTER COLUMN solver_uploads_enabled SET DEFAULT FALSE"));
        assert!(UP_SQL.contains("CREATE TABLE IF NOT EXISTS \"SolverUploads\""));
        assert!(UP_SQL.contains("ON \"SolverUploads\" (participation_id, operation_id)"));
        assert!(!UP_SQL.contains("CREATE TRIGGER"));
    }
}
