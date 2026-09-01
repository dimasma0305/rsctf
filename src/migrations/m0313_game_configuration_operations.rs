//! Revisioned, replay-safe event settings and durable post-commit effects.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "Games"
    ADD COLUMN IF NOT EXISTS configuration_revision BIGINT NOT NULL DEFAULT 0;
ALTER TABLE "Games"
    ALTER COLUMN configuration_revision SET DEFAULT 0;

CREATE TABLE IF NOT EXISTS "GameConfigurationOperations" (
    operation_id UUID PRIMARY KEY,
    game_id INTEGER NOT NULL REFERENCES "Games" (id) ON DELETE CASCADE,
    actor_user_id UUID NOT NULL REFERENCES "AspNetUsers" (id) ON DELETE CASCADE,
    request_digest TEXT NOT NULL CHECK (char_length(request_digest) = 64),
    expected_revision BIGINT NOT NULL CHECK (expected_revision >= 0),
    result_revision BIGINT NOT NULL CHECK (result_revision >= 0),
    result JSONB NOT NULL,
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS ix_gameconfigurationoperations_scope_created
    ON "GameConfigurationOperations" (game_id, actor_user_id, created_at_utc DESC);
CREATE INDEX IF NOT EXISTS ix_gameconfigurationoperations_created
    ON "GameConfigurationOperations" (created_at_utc, operation_id);

CREATE TABLE IF NOT EXISTS "GameConfigurationEffects" (
    game_id INTEGER PRIMARY KEY REFERENCES "Games" (id) ON DELETE CASCADE,
    configuration_revision BIGINT NOT NULL CHECK (configuration_revision >= 0),
    invalidate_game BOOLEAN NOT NULL,
    invalidate_scoreboards BOOLEAN NOT NULL,
    invalidate_policy BOOLEAN NOT NULL,
    claim_id UUID,
    claim_expires_at_utc TIMESTAMPTZ,
    updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS ix_gameconfigurationeffects_pending
    ON "GameConfigurationEffects" (updated_at_utc, game_id);
"#;

const DOWN_SQL: &str = r#"
DROP TABLE IF EXISTS "GameConfigurationEffects";
DROP TABLE IF EXISTS "GameConfigurationOperations";
ALTER TABLE "Games" DROP COLUMN IF EXISTS configuration_revision;
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
    fn game_configuration_schema_has_revision_replay_and_coalesced_effects() {
        assert!(UP_SQL.contains("configuration_revision BIGINT NOT NULL DEFAULT 0"));
        assert!(UP_SQL.contains("ALTER COLUMN configuration_revision SET DEFAULT 0"));
        assert!(UP_SQL.contains("operation_id UUID PRIMARY KEY"));
        assert!(UP_SQL.contains("request_digest TEXT NOT NULL"));
        assert!(UP_SQL.contains("result JSONB NOT NULL"));
        assert!(UP_SQL.contains("game_id INTEGER PRIMARY KEY"));
    }
}
