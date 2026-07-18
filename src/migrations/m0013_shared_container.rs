//! Adds `GameChallenges.shared_container_id` (the single challenge-owned shared
//! container's id; RSCTF `GameChallenge.SharedContainerId`, nullable uuid). A
//! `StaticContainer` with `enable_shared_container` serves ONE container to every
//! team and stores its id here. Idempotent, runs on existing DBs — existing rows
//! default to NULL (no shared container yet).
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
                        ColumnDef::new(Alias::new("shared_container_id"))
                            .uuid()
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
                    .drop_column(Alias::new("shared_container_id"))
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}
