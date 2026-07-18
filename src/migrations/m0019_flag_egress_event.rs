//! Adds `FlagEgressEvents` — one windowed row per (participation, challenge,
//! remote endpoint) where a team's own flag bytes were seen leaving its proxied
//! container (a flag being exfiltrated). Feeds the admin Flag-Egress tab
//! (`GET /api/admin/Games/{id}/FlagEgress`). Ported from RSCTF's FlagEgress
//! subsystem. NOTE: like RSCTF, this never raises a suspicion score — it is an
//! admin monitoring feed, since a team's own flag in its own container is not
//! itself cheating.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("FlagEgressEvents"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("game_id")).integer().not_null())
                    .col(
                        ColumnDef::new(Alias::new("participation_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("challenge_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("container_id")).uuid().null())
                    .col(
                        ColumnDef::new(Alias::new("remote_ip"))
                            .text()
                            .not_null()
                            .default(""),
                    )
                    .col(
                        ColumnDef::new(Alias::new("remote_port"))
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(Alias::new("hit_count"))
                            .integer()
                            .not_null()
                            .default(1),
                    )
                    .col(
                        ColumnDef::new(Alias::new("first_seen_utc"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("last_seen_utc"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("FlagEgressEvents"))
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}
