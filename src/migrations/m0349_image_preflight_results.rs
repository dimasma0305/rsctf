//! Per-challenge outcomes of an operator image preflight. Rows belong to one
//! durable control-plane job and are retained and purged with it.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "ImagePreflightResults" (
    job_id UUID NOT NULL REFERENCES "ControlPlaneJobs"(id) ON DELETE CASCADE,
    challenge_id INTEGER NOT NULL REFERENCES "GameChallenges"(id) ON DELETE CASCADE,
    image VARCHAR(512) NOT NULL,
    backend VARCHAR(16) NOT NULL,
    pull_status VARCHAR(16) NOT NULL,
    start_status VARCHAR(16) NOT NULL,
    duration_ms INTEGER NOT NULL DEFAULT 0 CHECK (duration_ms >= 0),
    error VARCHAR(1024) NULL,
    updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (job_id, challenge_id)
);
CREATE INDEX IF NOT EXISTS ix_image_preflight_results_challenge
    ON "ImagePreflightResults" (challenge_id);
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
    use super::UP_SQL;

    #[test]
    fn results_cascade_from_their_job_and_challenge() {
        assert!(UP_SQL.contains(r#"REFERENCES "ControlPlaneJobs"(id) ON DELETE CASCADE"#));
        assert!(UP_SQL.contains(r#"REFERENCES "GameChallenges"(id) ON DELETE CASCADE"#));
        assert!(UP_SQL.contains("PRIMARY KEY (job_id, challenge_id)"));
        assert!(UP_SQL.contains("error VARCHAR(1024)"));
    }
}
