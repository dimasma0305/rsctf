//! Required AI chat disclosure and its append-only telemetry.
//!
//! An event may require every competitive solve to be followed by a
//! disclosure: public share links, or an explicit "no AI used" declaration.
//! Every create, edit and clear is appended to `AiChatLinkEvents` with a
//! database timestamp, the time since the solve, a keyed network hash, and the
//! links before and after, so reviewers can see whether and how a team edited
//! its disclosure. Rows are append-only by application contract; no triggers
//! guard them, so event purge and foreign-key cascades keep working.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "Games"
    ADD COLUMN IF NOT EXISTS ai_chat_links_required BOOLEAN NOT NULL DEFAULT FALSE;
-- Fresh installs already have the entity-derived column; keep its default.
ALTER TABLE "Games" ALTER COLUMN ai_chat_links_required SET DEFAULT FALSE;

ALTER TABLE "AiChatLinks"
    ADD COLUMN IF NOT EXISTS declared_no_ai BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE "AiChatLinks" DROP CONSTRAINT IF EXISTS ck_ai_chat_links_shape;
ALTER TABLE "AiChatLinks" ADD CONSTRAINT ck_ai_chat_links_shape CHECK (
    jsonb_typeof(links) = 'array'
    AND (
        (declared_no_ai AND jsonb_array_length(links) = 0)
        OR (NOT declared_no_ai AND jsonb_array_length(links) BETWEEN 1 AND 5)
    )
);

CREATE TABLE IF NOT EXISTS "AiChatLinkEvents" (
    id BIGSERIAL PRIMARY KEY,
    game_id INTEGER NOT NULL REFERENCES "Games"(id) ON DELETE CASCADE,
    participation_id INTEGER NOT NULL REFERENCES "Participations"(id) ON DELETE CASCADE,
    challenge_id INTEGER NOT NULL REFERENCES "GameChallenges"(id) ON DELETE CASCADE,
    user_id UUID REFERENCES "AspNetUsers"(id) ON DELETE SET NULL,
    action TEXT NOT NULL CHECK (action IN ('Created', 'Edited', 'Cleared')),
    revision INTEGER NOT NULL CHECK (revision >= 0),
    previous_links JSONB NOT NULL CHECK (jsonb_typeof(previous_links) = 'array'),
    links JSONB NOT NULL CHECK (jsonb_typeof(links) = 'array'),
    added_urls JSONB NOT NULL CHECK (jsonb_typeof(added_urls) = 'array'),
    removed_urls JSONB NOT NULL CHECK (jsonb_typeof(removed_urls) = 'array'),
    previous_declared_no_ai BOOLEAN NOT NULL,
    declared_no_ai BOOLEAN NOT NULL,
    solved_at TIMESTAMPTZ,
    seconds_since_solve BIGINT,
    remote_ip_hash BYTEA CHECK (remote_ip_hash IS NULL OR octet_length(remote_ip_hash) = 32),
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX IF NOT EXISTS ix_ai_chat_link_events_record
    ON "AiChatLinkEvents" (game_id, participation_id, challenge_id, occurred_at, id);
CREATE INDEX IF NOT EXISTS ix_ai_chat_link_events_participation
    ON "AiChatLinkEvents" (participation_id);
CREATE INDEX IF NOT EXISTS ix_ai_chat_link_events_challenge
    ON "AiChatLinkEvents" (challenge_id);
CREATE INDEX IF NOT EXISTS ix_ai_chat_link_events_user
    ON "AiChatLinkEvents" (user_id);
"#;

const DOWN_SQL: &str = r#"
DROP TABLE IF EXISTS "AiChatLinkEvents";
DELETE FROM "AiChatLinks" WHERE declared_no_ai;
ALTER TABLE "AiChatLinks" DROP CONSTRAINT IF EXISTS ck_ai_chat_links_shape;
ALTER TABLE "AiChatLinks" DROP COLUMN IF EXISTS declared_no_ai;
ALTER TABLE "AiChatLinks" ADD CONSTRAINT ck_ai_chat_links_shape CHECK (
    jsonb_typeof(links) = 'array' AND jsonb_array_length(links) BETWEEN 1 AND 5
);
ALTER TABLE "Games" DROP COLUMN IF EXISTS ai_chat_links_required;
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
    fn disclosure_migration_is_idempotent_and_defaults_off() {
        assert!(UP_SQL.contains(
            "ADD COLUMN IF NOT EXISTS ai_chat_links_required BOOLEAN NOT NULL DEFAULT FALSE"
        ));
        assert!(UP_SQL.contains("ALTER COLUMN ai_chat_links_required SET DEFAULT FALSE"));
        assert!(UP_SQL.contains("DROP CONSTRAINT IF EXISTS ck_ai_chat_links_shape"));
        assert!(UP_SQL.contains("occurred_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()"));
        assert!(
            !UP_SQL.contains("CREATE TRIGGER"),
            "append-only by contract, not trigger"
        );
    }
}
