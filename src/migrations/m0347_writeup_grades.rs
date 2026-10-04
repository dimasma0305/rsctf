//! Private writeup review metadata. Competition scoring never reads this table.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "WriteupGrades" (
    game_id INTEGER NOT NULL REFERENCES "Games"(id) ON DELETE CASCADE,
    participation_id INTEGER NOT NULL REFERENCES "Participations"(id) ON DELETE CASCADE,
    challenge_id INTEGER NOT NULL REFERENCES "GameChallenges"(id) ON DELETE CASCADE,
    percentage SMALLINT CHECK (percentage BETWEEN 0 AND 100),
    revision INTEGER NOT NULL CHECK (revision > 0),
    operation_id UUID NOT NULL,
    graded_by UUID REFERENCES "AspNetUsers"(id) ON DELETE SET NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (game_id, participation_id, challenge_id)
);
CREATE INDEX IF NOT EXISTS ix_writeupgrades_participation ON "WriteupGrades"(participation_id);
CREATE INDEX IF NOT EXISTS ix_writeupgrades_challenge ON "WriteupGrades"(challenge_id);
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
