//! Revisioned team profiles with coalesced, bounded scoreboard invalidation.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "Teams"
    ADD COLUMN IF NOT EXISTS profile_revision BIGINT NOT NULL DEFAULT 0;

CREATE TABLE IF NOT EXISTS "TeamProfileOperations" (
    operation_id UUID PRIMARY KEY,
    team_id INTEGER NOT NULL REFERENCES "Teams" (id) ON DELETE CASCADE,
    actor_user_id UUID NOT NULL REFERENCES "AspNetUsers" (id) ON DELETE CASCADE,
    request_digest TEXT NOT NULL CHECK (char_length(request_digest) = 64),
    expected_revision BIGINT NOT NULL CHECK (expected_revision >= 0),
    result_revision BIGINT NOT NULL CHECK (result_revision >= 0),
    result JSONB NOT NULL,
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS ix_teamprofileoperations_scope_created
    ON "TeamProfileOperations" (team_id, actor_user_id, created_at_utc DESC);
CREATE INDEX IF NOT EXISTS ix_teamprofileoperations_created
    ON "TeamProfileOperations" (created_at_utc, operation_id);

CREATE TABLE IF NOT EXISTS "TeamProfileInvalidations" (
    team_id INTEGER PRIMARY KEY REFERENCES "Teams" (id) ON DELETE CASCADE,
    profile_revision BIGINT NOT NULL CHECK (profile_revision >= 0),
    after_game_id INTEGER NOT NULL DEFAULT 0 CHECK (after_game_id >= 0),
    updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS ix_teamprofileinvalidations_pending
    ON "TeamProfileInvalidations" (updated_at_utc, team_id);
"#;

const DOWN_SQL: &str = r#"
DROP TABLE IF EXISTS "TeamProfileInvalidations";
DROP TABLE IF EXISTS "TeamProfileOperations";
ALTER TABLE "Teams" DROP COLUMN IF EXISTS profile_revision;
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
    fn team_profile_schema_has_revision_replay_and_one_pending_generation() {
        assert!(UP_SQL.contains("profile_revision BIGINT NOT NULL DEFAULT 0"));
        assert!(UP_SQL.contains("operation_id UUID PRIMARY KEY"));
        assert!(UP_SQL.contains("result JSONB NOT NULL"));
        assert!(UP_SQL.contains("team_id INTEGER PRIMARY KEY"));
        assert!(UP_SQL.contains("after_game_id INTEGER NOT NULL DEFAULT 0"));
    }
}
