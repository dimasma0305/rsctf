//! AI chat links: an opt-in per-event disclosure where a team attaches the
//! public share links of AI chats it used for a solved challenge, plus the
//! admin-managed provider registry that decides which links are accepted.
//!
//! Built-in provider patterns live in Rust (`services::ai_chat_links`) so a
//! release can correct them; this table stores only built-in enable overrides
//! and custom providers. Links store the provider key and label they matched,
//! so blocking or deleting a provider later never hides saved evidence.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "Games"
    ADD COLUMN IF NOT EXISTS ai_chat_links_enabled BOOLEAN NOT NULL DEFAULT FALSE;
-- A fresh install already has this entity-derived column before this
-- migration runs, so `ADD COLUMN IF NOT EXISTS` would leave its default alone.
ALTER TABLE "Games" ALTER COLUMN ai_chat_links_enabled SET DEFAULT FALSE;

CREATE TABLE IF NOT EXISTS "AiChatProviders" (
    provider_key TEXT PRIMARY KEY
        CHECK (provider_key ~ '^[a-z0-9][a-z0-9-]{0,39}$'),
    builtin BOOLEAN NOT NULL,
    label TEXT,
    pattern TEXT,
    enabled BOOLEAN NOT NULL,
    updated_by UUID REFERENCES "AspNetUsers"(id) ON DELETE SET NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT ck_ai_chat_providers_shape CHECK (
        (builtin AND label IS NULL AND pattern IS NULL)
        OR (NOT builtin
            AND char_length(label) BETWEEN 1 AND 64
            AND char_length(pattern) BETWEEN 1 AND 512)
    )
);

CREATE TABLE IF NOT EXISTS "AiChatLinks" (
    game_id INTEGER NOT NULL REFERENCES "Games"(id) ON DELETE CASCADE,
    participation_id INTEGER NOT NULL REFERENCES "Participations"(id) ON DELETE CASCADE,
    challenge_id INTEGER NOT NULL REFERENCES "GameChallenges"(id) ON DELETE CASCADE,
    links JSONB NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    submitted_by UUID REFERENCES "AspNetUsers"(id) ON DELETE SET NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (game_id, participation_id, challenge_id),
    CONSTRAINT ck_ai_chat_links_shape CHECK (
        jsonb_typeof(links) = 'array'
        AND jsonb_array_length(links) BETWEEN 1 AND 5
    )
);
CREATE INDEX IF NOT EXISTS ix_ai_chat_links_game_updated
    ON "AiChatLinks" (game_id, updated_at DESC, participation_id, challenge_id);
CREATE INDEX IF NOT EXISTS ix_ai_chat_links_participation
    ON "AiChatLinks" (participation_id);
CREATE INDEX IF NOT EXISTS ix_ai_chat_links_challenge
    ON "AiChatLinks" (challenge_id);
"#;

const DOWN_SQL: &str = r#"
DROP TABLE IF EXISTS "AiChatLinks";
DROP TABLE IF EXISTS "AiChatProviders";
ALTER TABLE "Games" DROP COLUMN IF EXISTS ai_chat_links_enabled;
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
    use super::*;

    #[test]
    fn migration_is_idempotent_and_defaults_off() {
        assert!(UP_SQL.contains(
            "ADD COLUMN IF NOT EXISTS ai_chat_links_enabled BOOLEAN NOT NULL DEFAULT FALSE"
        ));
        assert_eq!(UP_SQL.matches("CREATE TABLE IF NOT EXISTS").count(), 2);
        assert_eq!(UP_SQL.matches("CREATE INDEX IF NOT EXISTS").count(), 3);
        assert!(UP_SQL.contains("jsonb_array_length(links) BETWEEN 1 AND 5"));
        assert!(UP_SQL.contains("ALTER COLUMN ai_chat_links_enabled SET DEFAULT FALSE"));
    }
}
