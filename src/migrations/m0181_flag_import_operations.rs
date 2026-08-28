//! Durable identity and bounded recovery for bulk static-flag authoring.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "FlagImportOperations" (
    challenge_id      INTEGER NOT NULL,
    operation_id      UUID NOT NULL,
    actor_user_id     UUID NOT NULL,
    request_digest    BYTEA NOT NULL CHECK (OCTET_LENGTH(request_digest) = 32),
    state             SMALLINT NOT NULL DEFAULT 0 CHECK (state IN (0, 1)),
    inserted_count    INTEGER NULL CHECK (inserted_count >= 0 AND inserted_count <= 100),
    duplicate_count   INTEGER NULL CHECK (duplicate_count >= 0 AND duplicate_count <= 100),
    lease_expires_at_utc TIMESTAMPTZ NOT NULL
        DEFAULT (clock_timestamp() + INTERVAL '5 minutes'),
    completed_at_utc  TIMESTAMPTZ NULL,
    created_at_utc    TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (challenge_id, operation_id),
    CONSTRAINT fk_flag_import_operation_challenge
        FOREIGN KEY (challenge_id) REFERENCES "GameChallenges"(id) ON DELETE CASCADE,
    CONSTRAINT fk_flag_import_operation_actor
        FOREIGN KEY (actor_user_id) REFERENCES "AspNetUsers"(id) ON DELETE RESTRICT,
    CONSTRAINT ck_flag_import_operation_result
        CHECK ((state = 0 AND completed_at_utc IS NULL
                         AND inserted_count IS NULL AND duplicate_count IS NULL)
               OR (state = 1 AND completed_at_utc IS NOT NULL
                         AND inserted_count IS NOT NULL AND duplicate_count IS NOT NULL))
);

CREATE INDEX IF NOT EXISTS ix_flag_import_operations_retention
    ON "FlagImportOperations" (completed_at_utc, challenge_id, operation_id)
    WHERE state = 1;
WITH ranked AS (
    SELECT id,
           ROW_NUMBER() OVER (PARTITION BY challenge_id, flag ORDER BY id) AS ordinal
      FROM "FlagContexts"
     WHERE challenge_id IS NOT NULL
)
DELETE FROM "FlagContexts" context
 USING ranked
 WHERE context.id = ranked.id AND ranked.ordinal > 1;
CREATE UNIQUE INDEX IF NOT EXISTS ux_flag_contexts_challenge_flag
    ON "FlagContexts" (challenge_id, flag)
    WHERE challenge_id IS NOT NULL;
"#;

const DOWN_SQL: &str = r#"
DROP TABLE IF EXISTS "FlagImportOperations";
DROP INDEX IF EXISTS ux_flag_contexts_challenge_flag;
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
    fn imports_have_one_identity_bounded_results_and_indexed_duplicate_checks() {
        assert!(UP_SQL.contains("PRIMARY KEY (challenge_id, operation_id)"));
        assert!(UP_SQL.contains("inserted_count <= 100"));
        assert!(
            UP_SQL.contains("CREATE UNIQUE INDEX IF NOT EXISTS ux_flag_contexts_challenge_flag")
        );
    }
}
