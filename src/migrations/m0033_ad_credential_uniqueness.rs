//! Enforce one active A&D API token and SSH key per participation, with SSH
//! fingerprints globally unique so bastion authentication is unambiguous.

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
                LOCK TABLE "AdTeamApiTokens", "AdSshKeys"
                  IN SHARE ROW EXCLUSIVE MODE;

                DELETE FROM "AdTeamApiTokens" older
                 USING "AdTeamApiTokens" newer
                 WHERE older.participation_id = newer.participation_id
                   AND older.id < newer.id;

                DELETE FROM "AdTeamApiTokens" older
                 USING "AdTeamApiTokens" newer
                 WHERE older.token_hash = newer.token_hash
                   AND older.id < newer.id;

                DELETE FROM "AdSshKeys" older
                 USING "AdSshKeys" newer
                 WHERE older.participation_id = newer.participation_id
                   AND older.id < newer.id;

                DELETE FROM "AdSshKeys" older
                 USING "AdSshKeys" newer
                 WHERE older.fingerprint = newer.fingerprint
                   AND older.id < newer.id;

                CREATE UNIQUE INDEX IF NOT EXISTS ux_adteamtokens_participation
                  ON "AdTeamApiTokens" (participation_id);
                CREATE UNIQUE INDEX IF NOT EXISTS ux_adteamtokens_hash
                  ON "AdTeamApiTokens" (token_hash);
                CREATE UNIQUE INDEX IF NOT EXISTS ux_adsshkeys_participation
                  ON "AdSshKeys" (participation_id);
                CREATE UNIQUE INDEX IF NOT EXISTS ux_adsshkeys_fingerprint
                  ON "AdSshKeys" (fingerprint);
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                DROP INDEX IF EXISTS ux_adsshkeys_fingerprint;
                DROP INDEX IF EXISTS ux_adsshkeys_participation;
                DROP INDEX IF EXISTS ux_adteamtokens_hash;
                DROP INDEX IF EXISTS ux_adteamtokens_participation;
                "#,
            )
            .await?;
        Ok(())
    }
}
