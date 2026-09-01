use sea_orm_migration::prelude::*;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "PlayerContainerOperations"
    ADD COLUMN IF NOT EXISTS runtime_started BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS definition_fence TEXT NULL,
    ADD COLUMN IF NOT EXISTS backend_id TEXT NULL;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint catalog_constraint
         WHERE catalog_constraint.conname = 'ck_player_container_operation_definition_fence'
           AND catalog_constraint.conrelid = '"PlayerContainerOperations"'::regclass
    ) THEN
        ALTER TABLE "PlayerContainerOperations"
            ADD CONSTRAINT ck_player_container_operation_definition_fence
            CHECK (definition_fence IS NULL
                   OR octet_length(definition_fence) BETWEEN 1 AND 256);
    END IF;
END $$;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint catalog_constraint
         WHERE catalog_constraint.conname = 'ck_player_container_operation_backend_id'
           AND catalog_constraint.conrelid = '"PlayerContainerOperations"'::regclass
    ) THEN
        ALTER TABLE "PlayerContainerOperations"
            ADD CONSTRAINT ck_player_container_operation_backend_id
            CHECK (backend_id IS NULL
                   OR octet_length(backend_id) BETWEEN 1 AND 512);
    END IF;
END $$;

CREATE TABLE IF NOT EXISTS "ManagedContainerReapOperations" (
    backend_id TEXT PRIMARY KEY,
    container_id UUID NOT NULL UNIQUE,
    scope_key TEXT NOT NULL,
    lease_owner UUID NOT NULL,
    lease_expires_at_utc TIMESTAMPTZ NOT NULL,
    updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    last_error TEXT NULL,
    CHECK (octet_length(backend_id) BETWEEN 1 AND 512),
    CHECK (octet_length(scope_key) BETWEEN 1 AND 255)
);
CREATE INDEX IF NOT EXISTS ix_managed_container_reap_expiry
    ON "ManagedContainerReapOperations" (lease_expires_at_utc, backend_id);
"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

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
    fn player_operations_record_launch_phase_and_definition_identity() {
        assert!(UP_SQL.contains("runtime_started BOOLEAN NOT NULL DEFAULT FALSE"));
        assert!(UP_SQL.contains("definition_fence TEXT NULL"));
        assert!(UP_SQL.contains("backend_id TEXT NULL"));
        assert!(UP_SQL.contains("octet_length(definition_fence) BETWEEN 1 AND 256"));
        assert!(UP_SQL.contains("catalog_constraint.conrelid"));
        assert!(UP_SQL.contains("ManagedContainerReapOperations"));
        assert!(UP_SQL.contains("backend_id TEXT PRIMARY KEY"));
        assert!(UP_SQL.contains("container_id UUID NOT NULL UNIQUE"));
        assert!(UP_SQL.contains("ix_managed_container_reap_expiry"));
    }
}
