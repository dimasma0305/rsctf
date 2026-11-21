use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "AdByocSourceHistory" (
    service_id INTEGER NOT NULL REFERENCES "AdTeamServices"(id) ON DELETE CASCADE,
    revision BIGINT NOT NULL CHECK (revision > 0),
    first_seen TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    last_seen TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    state TEXT NOT NULL CHECK (state IN ('observed','notRegistered','unavailable','legacyAgent')),
    files TEXT NOT NULL CHECK (octet_length(files) <= 65536),
    PRIMARY KEY (service_id, revision)
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
