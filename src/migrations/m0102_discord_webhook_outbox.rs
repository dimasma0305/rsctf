//! Durable, bounded delivery state for Discord blood announcements.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "DiscordWebhookOutbox" (
    notice_id            INTEGER PRIMARY KEY,
    game_id              INTEGER NOT NULL,
    attempts             INTEGER NOT NULL DEFAULT 0,
    available_at_utc     TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    freeze_deferred      BOOLEAN NOT NULL DEFAULT FALSE,
    lease_token          UUID NULL,
    lease_expires_at_utc TIMESTAMPTZ NULL,
    delivered_at_utc     TIMESTAMPTZ NULL,
    dead_at_utc          TIMESTAMPTZ NULL,
    last_http_status     SMALLINT NULL,
    last_error           VARCHAR(256) NULL,
    created_at_utc       TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT fk_discord_webhook_outbox_notice
        FOREIGN KEY (notice_id) REFERENCES "GameNotices" (id) ON DELETE CASCADE,
    CONSTRAINT fk_discord_webhook_outbox_game
        FOREIGN KEY (game_id) REFERENCES "Games" (id) ON DELETE CASCADE,
    CONSTRAINT ck_discord_webhook_outbox_attempts
        CHECK (attempts >= 0 AND attempts <= 64),
    CONSTRAINT ck_discord_webhook_outbox_lease_pair
        CHECK ((lease_token IS NULL) = (lease_expires_at_utc IS NULL)),
    CONSTRAINT ck_discord_webhook_outbox_terminal_state
        CHECK (NOT (delivered_at_utc IS NOT NULL AND dead_at_utc IS NOT NULL)),
    CONSTRAINT ck_discord_webhook_outbox_terminal_lease
        CHECK ((delivered_at_utc IS NULL AND dead_at_utc IS NULL)
               OR (lease_token IS NULL AND lease_expires_at_utc IS NULL))
);

CREATE INDEX IF NOT EXISTS ix_discord_webhook_outbox_pending
    ON "DiscordWebhookOutbox" (available_at_utc, notice_id)
    WHERE delivered_at_utc IS NULL AND dead_at_utc IS NULL;

CREATE INDEX IF NOT EXISTS ix_discord_webhook_outbox_game_notice
    ON "DiscordWebhookOutbox" (game_id, notice_id);

CREATE INDEX IF NOT EXISTS ix_discord_webhook_outbox_exhausted
    ON "DiscordWebhookOutbox"
       (attempts,
        (COALESCE(lease_expires_at_utc, '-infinity'::TIMESTAMPTZ)),
        notice_id)
    WHERE delivered_at_utc IS NULL AND dead_at_utc IS NULL;

CREATE INDEX IF NOT EXISTS ix_discord_webhook_outbox_terminal
    ON "DiscordWebhookOutbox"
       ((COALESCE(delivered_at_utc, dead_at_utc)), notice_id)
    WHERE delivered_at_utc IS NOT NULL OR dead_at_utc IS NOT NULL;
"#;

const DOWN_SQL: &str = r#"
DROP TABLE IF EXISTS "DiscordWebhookOutbox";
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
    fn outbox_is_idempotent_bounded_and_contains_no_webhook_secret() {
        assert!(UP_SQL.contains("CREATE TABLE IF NOT EXISTS"));
        assert!(UP_SQL.contains("attempts >= 0 AND attempts <= 64"));
        assert!(UP_SQL.contains("freeze_deferred      BOOLEAN NOT NULL DEFAULT FALSE"));
        assert!(UP_SQL.contains("CREATE INDEX IF NOT EXISTS"));
        assert!(UP_SQL.contains("(game_id, notice_id)"));
        assert!(UP_SQL.contains("ix_discord_webhook_outbox_terminal"));
        assert!(UP_SQL.contains("ix_discord_webhook_outbox_exhausted"));
        assert!(UP_SQL.contains("COALESCE(lease_expires_at_utc, '-infinity'::TIMESTAMPTZ)"));
        assert!(UP_SQL.contains("COALESCE(delivered_at_utc, dead_at_utc)"));
        assert!(UP_SQL.contains("REFERENCES \"GameNotices\""));
        assert!(UP_SQL.contains("REFERENCES \"Games\""));
        assert!(!UP_SQL.contains("webhook_url"));
        assert!(!UP_SQL.contains("webhook_token"));
    }
}
