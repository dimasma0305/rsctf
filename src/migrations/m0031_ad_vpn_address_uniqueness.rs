//! WireGuard cryptokey routing requires every active peer address to be unique.
//! Older deterministic allocation wrapped by subnet capacity without enforcing
//! that invariant, so peers from different games could receive the same /32.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                LOCK TABLE "AdVpnPeers", "AdTeamServices" IN SHARE ROW EXCLUSIVE MODE;

                CREATE TEMP TABLE duplicate_vpn_peers ON COMMIT DROP AS
                SELECT peer.id, peer.participation_id, peer.address,
                       row_number() OVER (
                         PARTITION BY peer.address
                         ORDER BY COALESCE((
                           participation.status = 1
                           AND participation.game_id = peer.game_id
                           AND game.start_time_utc <= now()
                           AND now() <= game.end_time_utc
                         ), FALSE) DESC,
                         peer.id
                       ) AS duplicate_number
                  FROM "AdVpnPeers" peer
                  LEFT JOIN "Participations" participation
                    ON participation.id = peer.participation_id
                  LEFT JOIN "Games" game ON game.id = peer.game_id;

                -- A deleted duplicate must not leave its BYOC target pointing at
                -- an address now cryptographically owned by the surviving peer.
                UPDATE "AdTeamServices" service
                   SET host = '', port = 0, status = 2
                  FROM duplicate_vpn_peers duplicate
                 WHERE duplicate.duplicate_number > 1
                   AND service.participation_id = duplicate.participation_id
                   AND service.container_id IS NULL
                   AND service.host = duplicate.address;

                DELETE FROM "AdVpnPeers" peer
                 USING duplicate_vpn_peers duplicate
                 WHERE duplicate.duplicate_number > 1
                   AND peer.id = duplicate.id;

                CREATE UNIQUE INDEX IF NOT EXISTS ux_advpnpeers_address
                  ON "AdVpnPeers"(address);
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS ux_advpnpeers_address;")
            .await?;
        Ok(())
    }
}
