use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "ImageCleanupLeases" (
    installation_scope TEXT PRIMARY KEY,
    next_run_at_utc TIMESTAMPTZ NOT NULL,
    lease_owner UUID,
    lease_until_utc TIMESTAMPTZ,
    updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT ck_imagecleanupleases_owner_pair CHECK (
        (lease_owner IS NULL) = (lease_until_utc IS NULL)
    )
);

CREATE INDEX IF NOT EXISTS ix_imagecleanupleases_due
    ON "ImageCleanupLeases" (next_run_at_utc, lease_until_utc);
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("ImageCleanupLeases"))
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_cadence_is_durable_and_indexed() {
        assert!(UP_SQL.contains("installation_scope TEXT PRIMARY KEY"));
        assert!(UP_SQL.contains("next_run_at_utc TIMESTAMPTZ NOT NULL"));
        assert!(UP_SQL.contains("ix_imagecleanupleases_due"));
    }
}
