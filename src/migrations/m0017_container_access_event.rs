//! Adds `ContainerAccessEvents` — one row per successful proxy WebSocket open of
//! a challenge container (RSCTF `ContainerAccessEvent`). The ground-truth access
//! log that powers the container-access cheat detectors (cross-team access,
//! delayed/instant submission, never-accessed, IP mismatch). Idempotent; runs on
//! existing DBs.
use crate::models::data::container_access_event;
use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::Schema;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let schema = Schema::new(manager.get_database_backend());
        let mut stmt = schema.create_table_from_entity(container_access_event::Entity);
        stmt.if_not_exists();
        manager.create_table(stmt).await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("ContainerAccessEvents"))
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}
