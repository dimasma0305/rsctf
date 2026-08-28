//! Bounded, revisioned division metadata and permission replacement.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "Divisions"
    ADD COLUMN IF NOT EXISTS revision BIGINT NOT NULL DEFAULT 1,
    ADD COLUMN IF NOT EXISTS policy_revision BIGINT NOT NULL DEFAULT 1;

CREATE TABLE IF NOT EXISTS "DivisionUpdateOperations" (
    division_id       INTEGER NOT NULL,
    operation_id      UUID NOT NULL,
    actor_user_id     UUID NOT NULL,
    request_digest    BYTEA NOT NULL CHECK (OCTET_LENGTH(request_digest) = 32),
    expected_revision BIGINT NOT NULL CHECK (expected_revision >= 1),
    result_revision   BIGINT NOT NULL CHECK (result_revision >= expected_revision),
    created_at_utc    TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (division_id, operation_id),
    CONSTRAINT fk_division_update_operation_division
        FOREIGN KEY (division_id) REFERENCES "Divisions"(id) ON DELETE CASCADE,
    CONSTRAINT fk_division_update_operation_actor
        FOREIGN KEY (actor_user_id) REFERENCES "AspNetUsers"(id) ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS ix_division_update_operations_retention
    ON "DivisionUpdateOperations" (created_at_utc, division_id, operation_id);
"#;

const DOWN_SQL: &str = r#"
DROP TABLE IF EXISTS "DivisionUpdateOperations";
ALTER TABLE "Divisions"
    DROP COLUMN IF EXISTS policy_revision,
    DROP COLUMN IF EXISTS revision;
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
    fn update_identity_and_revisions_are_durable() {
        assert!(UP_SQL.contains("PRIMARY KEY (division_id, operation_id)"));
        assert!(UP_SQL.contains("OCTET_LENGTH(request_digest) = 32"));
        assert!(UP_SQL.contains("policy_revision BIGINT NOT NULL DEFAULT 1"));
    }
}
