//! Adds `GameChallenges.original_archive_blob_path` — the blob hash of a
//! challenge's persisted source archive (re-zipped on import), read back by the
//! audit modal (`EditController.GetChallengeAuditMeta`). Idempotent, runs on
//! existing DBs.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("GameChallenges"))
                    .add_column_if_not_exists(
                        ColumnDef::new(Alias::new("original_archive_blob_path"))
                            .text()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("GameChallenges"))
                    .drop_column(Alias::new("original_archive_blob_path"))
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}
