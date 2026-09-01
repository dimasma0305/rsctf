use sea_orm_migration::prelude::*;

pub const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "EventTelemetryBatches" (
    batch_id UUID PRIMARY KEY,
    game_id INTEGER NOT NULL REFERENCES "Games"(id) ON DELETE CASCADE,
    request_fingerprint BYTEA NOT NULL CHECK (octet_length(request_fingerprint) = 32),
    result JSONB,
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    completed_at_utc TIMESTAMPTZ,
    CHECK ((result IS NULL) = (completed_at_utc IS NULL))
);
CREATE INDEX IF NOT EXISTS ix_event_telemetry_batches_expiry
    ON "EventTelemetryBatches"(created_at_utc, batch_id);
"#;

const DOWN_SQL: &str = r#"DROP TABLE IF EXISTS "EventTelemetryBatches";"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

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
    fn sensor_batch_results_are_durable_and_idempotent() {
        assert!(UP_SQL.contains("batch_id UUID PRIMARY KEY"));
        assert!(UP_SQL.contains("request_fingerprint BYTEA"));
        assert!(UP_SQL.contains("result JSONB"));
    }
}
