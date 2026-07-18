//! Adds `AdVpnPeers` — one WireGuard peer per (game, participation) for the A&D
//! VPN. Each row holds the team's persisted X25519 keypair and the /32 VPN
//! address it's assigned, mirroring RSCTF's `AdVpnPeer` data model. rsctf drives
//! the hub in-process (`services/ad_vpn`) rather than RSCTF's sidecar, but the
//! key/address model is the same: the hub's peer entry and the downloaded
//! `.conf` are rendered from the SAME stored key so the handshake succeeds.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("AdVpnPeers"))
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
                        ColumnDef::new(Alias::new("private_key"))
                            .text()
                            .not_null()
                            .default(""),
                    )
                    .col(
                        ColumnDef::new(Alias::new("public_key"))
                            .text()
                            .not_null()
                            .default(""),
                    )
                    .col(
                        ColumnDef::new(Alias::new("address"))
                            .text()
                            .not_null()
                            .default(""),
                    )
                    .col(
                        ColumnDef::new(Alias::new("created_utc"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // One peer per (game, participation).
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("ux_advpnpeers_game_participation")
                    .table(Alias::new("AdVpnPeers"))
                    .col(Alias::new("game_id"))
                    .col(Alias::new("participation_id"))
                    .unique()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("AdVpnPeers"))
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}
