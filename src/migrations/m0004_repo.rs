//! Adds the repo-binding tables (`RepoBindings` + `RepoBindingScans`) introduced
//! after the initial schema. Runs on existing databases (idempotent, `if_not_exists`)
//! so a deployed instance gains git challenge-sync without a reset.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::Schema;

use crate::models::data::{repo_binding, repo_binding_scan};

#[derive(DeriveMigrationName)]
pub struct Migration;

macro_rules! create {
    ($manager:expr, $schema:expr, $entity:expr) => {{
        let mut stmt = $schema.create_table_from_entity($entity);
        stmt.if_not_exists();
        $manager.create_table(stmt).await?;
    }};
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let schema = Schema::new(backend);
        create!(manager, schema, repo_binding::Entity);
        create!(manager, schema, repo_binding_scan::Entity);
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for t in ["RepoBindingScans", "RepoBindings"] {
            manager
                .drop_table(Table::drop().table(Alias::new(t)).if_exists().to_owned())
                .await?;
        }
        Ok(())
    }
}
