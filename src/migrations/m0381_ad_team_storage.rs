use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "AdPatchStarters" ADD COLUMN IF NOT EXISTS data_mount TEXT
    CHECK (data_mount='/data' OR data_mount ~ '^/var/lib/[A-Za-z0-9_-]{1,128}$');
ALTER TABLE "AdPatchStarters" ADD COLUMN IF NOT EXISTS data_mount_claimed BOOLEAN NOT NULL DEFAULT FALSE;
CREATE TABLE IF NOT EXISTS "AdTeamStorageClaims" (
    participation_id INTEGER NOT NULL REFERENCES "Participations"(id) ON DELETE CASCADE,
    challenge_id INTEGER NOT NULL REFERENCES "AdPatchStarters"(challenge_id) ON DELETE CASCADE,
    allocation_id UUID NOT NULL UNIQUE,
    claim_uid TEXT CHECK (length(claim_uid) BETWEEN 1 AND 128),
    PRIMARY KEY (participation_id, challenge_id)
);
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
