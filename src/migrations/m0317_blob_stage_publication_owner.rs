//! Bind every staged blob receipt to the one domain owner that consumed it.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "BlobStagingOperations"
    ADD COLUMN IF NOT EXISTS published_owner_scope TEXT NULL;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
          FROM pg_constraint
         WHERE conname = 'ck_blobstaging_published_owner_scope'
           AND conrelid = '"BlobStagingOperations"'::regclass
    ) THEN
        ALTER TABLE "BlobStagingOperations"
            ADD CONSTRAINT ck_blobstaging_published_owner_scope
            CHECK (
                (state = 'Published' AND published_owner_scope IS NOT NULL)
                OR (state <> 'Published' AND published_owner_scope IS NULL)
            ) NOT VALID;
    END IF;
END
$$;
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
    fn publication_owner_is_forward_only_and_required_for_new_receipts() {
        assert!(UP_SQL.contains("ADD COLUMN IF NOT EXISTS published_owner_scope"));
        assert!(UP_SQL.contains("ck_blobstaging_published_owner_scope"));
        assert!(UP_SQL.contains("NOT VALID"));
        assert!(!UP_SQL.contains("UPDATE \"BlobStagingOperations\""));
    }
}
