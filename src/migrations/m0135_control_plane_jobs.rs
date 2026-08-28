//! Durable, bounded admission for slow control-plane mutations.

use sea_orm::{ConnectionTrait, DbErr};
use sea_orm_migration::prelude::*;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "ControlPlaneJobs" (
    id UUID PRIMARY KEY,
    kind VARCHAR(32) NOT NULL,
    scope_key VARCHAR(256) NOT NULL,
    game_id INTEGER NOT NULL REFERENCES "Games"(id) ON DELETE CASCADE,
    challenge_id INTEGER NULL REFERENCES "GameChallenges"(id) ON DELETE CASCADE,
    operation_id UUID NOT NULL,
    fingerprint VARCHAR(64) NOT NULL,
    input JSONB NOT NULL DEFAULT '{}'::jsonb,
    input_revision INTEGER NOT NULL DEFAULT 1,
    status SMALLINT NOT NULL DEFAULT 0,
    progress_current INTEGER NOT NULL DEFAULT 0,
    progress_total INTEGER NOT NULL DEFAULT 0,
    result JSONB NULL,
    error TEXT NULL,
    lease_token UUID NULL,
    lease_expires_at_utc TIMESTAMPTZ NULL,
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    finished_at_utc TIMESTAMPTZ NULL,
    CONSTRAINT ck_control_plane_jobs_status CHECK (status BETWEEN 0 AND 4),
    CONSTRAINT ck_control_plane_jobs_progress CHECK (
        progress_current BETWEEN 0 AND 1000000
        AND progress_total BETWEEN 0 AND 1000000
        AND progress_current <= progress_total
    ),
    CONSTRAINT ck_control_plane_jobs_input_revision CHECK (
        input_revision BETWEEN 1 AND 1000000
    ),
    CONSTRAINT ck_control_plane_jobs_lease CHECK (
        (lease_token IS NULL) = (lease_expires_at_utc IS NULL)
    ),
    CONSTRAINT ck_control_plane_jobs_terminal CHECK (
        (status IN (0, 1) AND finished_at_utc IS NULL)
        OR (status IN (2, 3, 4) AND finished_at_utc IS NOT NULL)
    ),
    CONSTRAINT ck_control_plane_jobs_error_bound CHECK (
        error IS NULL OR octet_length(error) <= 4096
    )
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_control_plane_jobs_operation
    ON "ControlPlaneJobs" (operation_id);
CREATE UNIQUE INDEX IF NOT EXISTS ux_control_plane_jobs_active_scope
    ON "ControlPlaneJobs" (kind, scope_key)
    WHERE status IN (0, 1);
CREATE INDEX IF NOT EXISTS ix_control_plane_jobs_claim
    ON "ControlPlaneJobs" (created_at_utc, id)
    WHERE status = 0 OR (status = 1 AND lease_expires_at_utc IS NOT NULL);
CREATE INDEX IF NOT EXISTS ix_control_plane_jobs_game_recent
    ON "ControlPlaneJobs" (game_id, created_at_utc DESC, id DESC);

CREATE TABLE IF NOT EXISTS "ControlPlaneJobOperations" (
    operation_id UUID PRIMARY KEY,
    job_id UUID NOT NULL REFERENCES "ControlPlaneJobs"(id) ON DELETE CASCADE,
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
INSERT INTO "ControlPlaneJobOperations" (operation_id, job_id)
SELECT operation_id, id FROM "ControlPlaneJobs"
ON CONFLICT (operation_id) DO NOTHING;
CREATE INDEX IF NOT EXISTS ix_control_plane_job_operations_job
    ON "ControlPlaneJobOperations" (job_id);

CREATE TABLE IF NOT EXISTS "ControlPlaneResourceLeases" (
    resource_key VARCHAR(256) PRIMARY KEY,
    owner_job_id UUID NOT NULL REFERENCES "ControlPlaneJobs"(id) ON DELETE CASCADE,
    lease_expires_at_utc TIMESTAMPTZ NOT NULL,
    CONSTRAINT ck_control_plane_resource_lease_future CHECK (
        lease_expires_at_utc > '-infinity'::timestamptz
    )
);
CREATE INDEX IF NOT EXISTS ix_control_plane_resource_lease_recovery
    ON "ControlPlaneResourceLeases" (lease_expires_at_utc);

CREATE TABLE IF NOT EXISTS "VariantGenerationClaims" (
    game_id INTEGER NOT NULL REFERENCES "Games"(id) ON DELETE CASCADE,
    challenge_id INTEGER NOT NULL REFERENCES "GameChallenges"(id) ON DELETE CASCADE,
    participation_id INTEGER NOT NULL REFERENCES "Participations"(id) ON DELETE CASCADE,
    revision INTEGER NOT NULL,
    job_id UUID NOT NULL REFERENCES "ControlPlaneJobs"(id) ON DELETE CASCADE,
    lease_expires_at_utc TIMESTAMPTZ NOT NULL,
    completed_at_utc TIMESTAMPTZ NULL,
    PRIMARY KEY (game_id, challenge_id, participation_id, revision),
    CONSTRAINT ck_variant_generation_claim_revision CHECK (revision >= 1)
);
CREATE INDEX IF NOT EXISTS ix_variant_generation_claim_recovery
    ON "VariantGenerationClaims" (lease_expires_at_utc, game_id)
    WHERE completed_at_utc IS NULL;
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
            .execute_unprepared(
                r#"
DROP TABLE IF EXISTS "VariantGenerationClaims";
DROP TABLE IF EXISTS "ControlPlaneResourceLeases";
DROP TABLE IF EXISTS "ControlPlaneJobOperations";
DROP TABLE IF EXISTS "ControlPlaneJobs";
"#,
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn jobs_have_operation_and_active_scope_uniqueness() {
        assert!(UP_SQL.contains("ux_control_plane_jobs_operation"));
        assert!(UP_SQL.contains("ux_control_plane_jobs_active_scope"));
        assert!(UP_SQL.contains("WHERE status IN (0, 1)"));
        assert!(UP_SQL.contains("error IS NULL OR octet_length(error) <= 4096"));
        assert!(UP_SQL.contains("lease_expires_at_utc"));
        assert!(UP_SQL.contains("VariantGenerationClaims"));
        assert!(UP_SQL.contains("ControlPlaneResourceLeases"));
        assert!(UP_SQL.contains("ControlPlaneJobOperations"));
        assert!(UP_SQL.contains("PRIMARY KEY (game_id, challenge_id, participation_id, revision)"));
    }
}
