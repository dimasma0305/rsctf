//! Recoverable attachment staging for bounded static-flag imports.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "FlagImportOperations"
    ADD COLUMN IF NOT EXISTS staged_attachment_ids INTEGER[] NOT NULL DEFAULT '{}';

DO $constraint$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_flag_import_staged_attachments'
           AND conrelid = '"FlagImportOperations"'::regclass
    ) THEN
        ALTER TABLE "FlagImportOperations"
            ADD CONSTRAINT ck_flag_import_staged_attachments
            CHECK (cardinality(staged_attachment_ids) <= 100);
    END IF;
END
$constraint$;

CREATE INDEX IF NOT EXISTS ix_flag_import_staging_cleanup
    ON "FlagImportOperations" (completed_at_utc, challenge_id, operation_id)
    WHERE cardinality(staged_attachment_ids) > 0;
"#;

const DOWN_SQL: &str = r#"
DROP INDEX IF EXISTS ix_flag_import_staging_cleanup;
ALTER TABLE "FlagImportOperations"
    DROP CONSTRAINT IF EXISTS ck_flag_import_staged_attachments;
ALTER TABLE "FlagImportOperations" DROP COLUMN IF EXISTS staged_attachment_ids;
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
    fn staged_attachment_ownership_is_bounded_and_reconcilable() {
        assert!(UP_SQL.contains("staged_attachment_ids INTEGER[]"));
        assert!(UP_SQL.contains("cardinality(staged_attachment_ids) <= 100"));
        assert!(UP_SQL.contains("ix_flag_import_staging_cleanup"));
    }
}
