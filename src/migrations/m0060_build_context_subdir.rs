//! Preserve a complete reviewed source archive while recording which subtree is
//! the Docker build context. Pending submissions can therefore be audited and
//! retried without copying or replacing their immutable source blob.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP_SQL: &str = r#"
    ALTER TABLE "GameChallenges"
        ADD COLUMN IF NOT EXISTS build_context_subdir TEXT NULL;

    UPDATE "GameChallenges"
       SET build_context_subdir = '.'
     WHERE build_context_subdir IS NULL
       AND original_archive_blob_path IS NOT NULL
       AND container_image LIKE 'rsctf/%';
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
                r#"ALTER TABLE "GameChallenges"
                       DROP COLUMN IF EXISTS build_context_subdir;"#,
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn adds_idempotent_context_subdirectory_column() {
        assert!(UP_SQL.contains("ADD COLUMN IF NOT EXISTS build_context_subdir"));
        assert!(UP_SQL.contains("SET build_context_subdir = '.'"));
    }
}
