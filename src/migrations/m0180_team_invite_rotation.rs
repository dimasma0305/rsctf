//! Revisioned, recoverable team invitation credential rotations.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "Teams"
    ADD COLUMN IF NOT EXISTS invite_revision BIGINT NOT NULL DEFAULT 1;

DO $$ BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_teams_invite_revision'
           AND conrelid = '"Teams"'::regclass
    ) THEN
        ALTER TABLE "Teams" ADD CONSTRAINT ck_teams_invite_revision
            CHECK (invite_revision >= 1);
    END IF;
END $$;

CREATE TABLE IF NOT EXISTS "TeamInviteOperations" (
    team_id          INTEGER NOT NULL,
    operation_id     UUID NOT NULL,
    actor_user_id    UUID NOT NULL,
    expected_revision BIGINT NOT NULL,
    result_revision BIGINT NOT NULL,
    result_token     VARCHAR(32) NOT NULL,
    reconciled_at_utc TIMESTAMPTZ NULL,
    created_at_utc  TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (team_id, operation_id),
    CONSTRAINT fk_team_invite_operation_team
        FOREIGN KEY (team_id) REFERENCES "Teams"(id) ON DELETE CASCADE,
    CONSTRAINT fk_team_invite_operation_actor
        FOREIGN KEY (actor_user_id) REFERENCES "AspNetUsers"(id) ON DELETE RESTRICT,
    CONSTRAINT ck_team_invite_operation_revision
        CHECK (expected_revision >= 1 AND result_revision = expected_revision + 1),
    CONSTRAINT ck_team_invite_operation_token
        CHECK (result_token ~ '^[0-9a-f]{32}$')
);

CREATE INDEX IF NOT EXISTS ix_team_invite_operations_retention
    ON "TeamInviteOperations" (created_at_utc, team_id, operation_id);
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn rotation_identity_is_unique_and_reconciliation_is_durable() {
        assert!(UP_SQL.contains("PRIMARY KEY (team_id, operation_id)"));
        assert!(UP_SQL.contains("result_revision = expected_revision + 1"));
        assert!(UP_SQL.contains("reconciled_at_utc"));
    }
}
