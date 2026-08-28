//! Durable, deployment-wide admission leases for traffic archive streams.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "TrafficArchiveLeases" (
    operation_id UUID PRIMARY KEY,
    challenge_id INTEGER NOT NULL,
    participation_id INTEGER NOT NULL,
    reserved_bytes BIGINT NOT NULL CHECK (reserved_bytes > 0),
    expires_at_utc TIMESTAMPTZ NOT NULL,
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS ix_trafficarchiveleases_expiry
    ON "TrafficArchiveLeases" (expires_at_utc);
"#;

const DOWN_SQL: &str = r#"
DROP TABLE IF EXISTS "TrafficArchiveLeases";
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(DOWN_SQL)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn archive_admission_schema_is_bounded_and_reapable() {
        assert!(UP_SQL.contains("operation_id UUID PRIMARY KEY"));
        assert!(UP_SQL.contains("reserved_bytes BIGINT NOT NULL CHECK (reserved_bytes > 0)"));
        assert!(UP_SQL.contains("expires_at_utc TIMESTAMPTZ NOT NULL"));
        assert!(UP_SQL.contains("ix_trafficarchiveleases_expiry"));
    }
}
