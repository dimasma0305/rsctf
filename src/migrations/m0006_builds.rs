//! Adds the `BuildRecords` table (challenge image build audit history).
//! Idempotent, runs on existing databases.
use crate::models::data::build_record;
use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::Schema;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let schema = Schema::new(manager.get_database_backend());
        let mut stmt = schema.create_table_from_entity(build_record::Entity);
        stmt.if_not_exists();
        manager.create_table(stmt).await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("BuildRecords"))
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}
