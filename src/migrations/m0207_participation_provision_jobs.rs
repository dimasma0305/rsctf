//! Durable recovery for resources created after a participation is accepted.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "ParticipationProvisionJobs" (
    participation_id INTEGER PRIMARY KEY
        REFERENCES "Participations" (id) ON DELETE CASCADE,
    game_id INTEGER NOT NULL REFERENCES "Games" (id) ON DELETE CASCADE,
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    lease_owner UUID NULL,
    lease_until TIMESTAMPTZ NULL,
    last_error TEXT NULL,
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX IF NOT EXISTS ix_participationprovisionjobs_due
    ON "ParticipationProvisionJobs" (next_attempt_at, participation_id);
"#;

const DOWN_SQL: &str = r#"
DROP TABLE IF EXISTS "ParticipationProvisionJobs";
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
    fn recovery_queue_is_durable_idempotent_and_due_indexed() {
        assert!(UP_SQL.contains("CREATE TABLE IF NOT EXISTS"));
        assert!(UP_SQL.contains("participation_id INTEGER PRIMARY KEY"));
        assert!(UP_SQL.contains("lease_owner UUID NULL"));
        assert!(UP_SQL.contains("CREATE INDEX IF NOT EXISTS ix_participationprovisionjobs_due"));
    }
}
