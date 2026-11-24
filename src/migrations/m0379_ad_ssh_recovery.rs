use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "AdSshRecovery" (
    service_id integer NOT NULL, revision bigint NOT NULL,
    id uuid NOT NULL UNIQUE, starter_tree text NOT NULL CHECK(starter_tree ~ '^[0-9a-f]{64}$'),
    PRIMARY KEY(service_id,revision),
    FOREIGN KEY(service_id,revision) REFERENCES "AdHostedSshSourceHistory" ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS "AdSshRecoveryBlobs" (
    service_id integer NOT NULL REFERENCES "AdTeamServices" ON DELETE CASCADE,
    oid text NOT NULL CHECK (oid ~ '^[0-9a-f]{64}$'),
    payload bytea NOT NULL CHECK (octet_length(payload)<=245760),
    PRIMARY KEY(service_id,oid)
);
CREATE TABLE IF NOT EXISTS "AdSshRecoveryFiles" (
    service_id integer NOT NULL, revision bigint NOT NULL,
    path text NOT NULL CHECK (octet_length(path) BETWEEN 1 AND 240),
    oid text, executable boolean NOT NULL,
    PRIMARY KEY(service_id,revision,path),
    FOREIGN KEY(service_id,revision) REFERENCES "AdSshRecovery" ON DELETE CASCADE,
    FOREIGN KEY(service_id,oid) REFERENCES "AdSshRecoveryBlobs" DEFERRABLE INITIALLY DEFERRED
);
CREATE OR REPLACE FUNCTION ad_ssh_collect_unused_blobs() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    DELETE FROM "AdSshRecoveryBlobs" b USING removed f
        WHERE b.service_id=f.service_id AND b.oid=f.oid AND NOT EXISTS
        (SELECT 1 FROM "AdSshRecoveryFiles" kept WHERE kept.service_id=b.service_id AND kept.oid=b.oid);
    RETURN NULL;
END $$;
CREATE OR REPLACE TRIGGER ad_ssh_recovery_unreferenced AFTER DELETE ON "AdSshRecoveryFiles"
    REFERENCING OLD TABLE AS removed FOR EACH STATEMENT EXECUTE FUNCTION ad_ssh_collect_unused_blobs();
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
