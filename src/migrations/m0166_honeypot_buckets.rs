use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "HoneypotHitBuckets" (
    bucket_start_utc TIMESTAMPTZ NOT NULL,
    bait VARCHAR(128) NOT NULL,
    source_hash VARCHAR(128) NOT NULL,
    user_id UUID,
    user_agent VARCHAR(256),
    hit_count BIGINT NOT NULL DEFAULT 1 CHECK (hit_count > 0),
    last_hit_at_utc TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (bucket_start_utc, bait, source_hash)
);

CREATE INDEX IF NOT EXISTS ix_honeypothitbuckets_retention
    ON "HoneypotHitBuckets" (last_hit_at_utc);
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telemetry_is_bucketed_and_has_a_retention_index() {
        assert!(UP_SQL.contains("PRIMARY KEY (bucket_start_utc, bait, source_hash)"));
        assert!(UP_SQL.contains("hit_count BIGINT NOT NULL"));
        assert!(UP_SQL.contains("ix_honeypothitbuckets_retention"));
    }
}
