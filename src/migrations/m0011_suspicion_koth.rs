//! Adds `SuspicionRules` (configurable detector weights) + `KothControlResults`
//! (per-round hill control history). Idempotent; runs on existing DBs.
use crate::models::data::{koth_control_result, suspicion_rule};
use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::Schema;

#[derive(DeriveMigrationName)]
pub struct Migration;

macro_rules! create {
    ($m:expr, $s:expr, $e:expr) => {{
        let mut st = $s.create_table_from_entity($e);
        st.if_not_exists();
        $m.create_table(st).await?;
    }};
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let schema = Schema::new(manager.get_database_backend());
        create!(manager, schema, suspicion_rule::Entity);
        create!(manager, schema, koth_control_result::Entity);
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for t in ["KothControlResults", "SuspicionRules"] {
            manager
                .drop_table(Table::drop().table(Alias::new(t)).if_exists().to_owned())
                .await?;
        }
        Ok(())
    }
}
