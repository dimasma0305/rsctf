use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
            CREATE TABLE IF NOT EXISTS "AdWorkerRelays" (
                workload_id UUID PRIMARY KEY REFERENCES "WorkerWorkloads"(id) ON DELETE CASCADE,
                participation_id INTEGER NOT NULL REFERENCES "Participations"(id) ON DELETE CASCADE,
                challenge_id INTEGER NOT NULL REFERENCES "GameChallenges"(id) ON DELETE CASCADE,
                host TEXT,
                port INTEGER CHECK (port BETWEEN 1 AND 65535),
                ready_until TIMESTAMPTZ,
                CHECK ((host IS NULL) = (port IS NULL)),
                UNIQUE(host,port)
            )
        "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(r#"DROP TABLE IF EXISTS "AdWorkerRelays""#)
            .await?;
        Ok(())
    }
}
