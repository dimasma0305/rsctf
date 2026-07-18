//! Adds `Logs.browser_fingerprint` — the submitting browser fingerprint the
//! login audit records (RSCTF `LogModel.BrowserFingerprint`), rendered as the
//! `fingerprint` column of the admin Logs table. Idempotent, runs on existing DBs.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("Logs"))
                    .add_column_if_not_exists(
                        ColumnDef::new(Alias::new("browser_fingerprint"))
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
                    .table(Alias::new("Logs"))
                    .drop_column(Alias::new("browser_fingerprint"))
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}
