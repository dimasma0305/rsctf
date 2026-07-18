//! Adds the supplementary tables introduced after the initial schema: the team
//! roster, the audit log, and suspicion events. Runs on existing databases (that
//! already applied `m0001_init`) so they gain the new tables without a reset.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::Schema;

use crate::models::data::{log_entry, suspicion_event, team_member};

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
        create!(manager, schema, team_member::Entity);
        create!(manager, schema, log_entry::Entity);
        create!(manager, schema, suspicion_event::Entity);
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for t in ["SuspicionEvents", "Logs", "TeamMembers"] {
            manager
                .drop_table(Table::drop().table(Alias::new(t)).if_exists().to_owned())
                .await?;
        }
        Ok(())
    }
}
