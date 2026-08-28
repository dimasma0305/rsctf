//! Durable, bounded account-mail intent and delivery state.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "MailOutbox" (
    operation_id              UUID PRIMARY KEY,
    purpose                   SMALLINT NOT NULL,
    account_id                UUID NOT NULL,
    security_generation_digest BYTEA NOT NULL,
    destination               VARCHAR(320) NOT NULL,
    destination_digest        BYTEA NOT NULL,
    source_digest             BYTEA NULL,
    request_digest            BYTEA NOT NULL,
    subject                   VARCHAR(256) NOT NULL,
    html_body                 TEXT NOT NULL,
    attempts                  SMALLINT NOT NULL DEFAULT 0,
    available_at_utc          TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    lease_token               UUID NULL,
    lease_expires_at_utc      TIMESTAMPTZ NULL,
    delivery_slot             SMALLINT NULL,
    delivered_at_utc          TIMESTAMPTZ NULL,
    dead_at_utc               TIMESTAMPTZ NULL,
    superseded_at_utc         TIMESTAMPTZ NULL,
    last_error                VARCHAR(256) NULL,
    created_at_utc            TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT fk_mail_outbox_account
        FOREIGN KEY (account_id) REFERENCES "AspNetUsers" (id) ON DELETE CASCADE,
    CONSTRAINT ck_mail_outbox_purpose CHECK (purpose BETWEEN 0 AND 2),
    CONSTRAINT ck_mail_outbox_generation_digest
        CHECK (octet_length(security_generation_digest) = 32),
    CONSTRAINT ck_mail_outbox_destination_digest
        CHECK (octet_length(destination_digest) = 32),
    CONSTRAINT ck_mail_outbox_source_digest
        CHECK (source_digest IS NULL OR octet_length(source_digest) = 32),
    CONSTRAINT ck_mail_outbox_request_digest CHECK (octet_length(request_digest) = 32),
    CONSTRAINT ck_mail_outbox_message_bounds
        CHECK (octet_length(destination) BETWEEN 3 AND 320
               AND octet_length(subject) BETWEEN 1 AND 256
               AND octet_length(html_body) BETWEEN 0 AND 65536
               AND (octet_length(html_body) >= 1
                    OR delivered_at_utc IS NOT NULL
                    OR dead_at_utc IS NOT NULL)),
    CONSTRAINT ck_mail_outbox_attempts CHECK (attempts BETWEEN 0 AND 8),
    CONSTRAINT ck_mail_outbox_delivery_slot
        CHECK (delivery_slot IS NULL OR delivery_slot BETWEEN 0 AND 3),
    CONSTRAINT ck_mail_outbox_lease_pair
        CHECK ((lease_token IS NULL) = (lease_expires_at_utc IS NULL)),
    CONSTRAINT ck_mail_outbox_lease_slot
        CHECK ((lease_token IS NULL) = (delivery_slot IS NULL)),
    CONSTRAINT ck_mail_outbox_terminal_state
        CHECK (NOT (delivered_at_utc IS NOT NULL AND dead_at_utc IS NOT NULL)),
    CONSTRAINT ck_mail_outbox_terminal_lease
        CHECK ((delivered_at_utc IS NULL AND dead_at_utc IS NULL)
               OR (lease_token IS NULL AND lease_expires_at_utc IS NULL))
);

CREATE TABLE IF NOT EXISTS "MailDeliverySlots" (
    slot_id              SMALLINT PRIMARY KEY,
    lease_token          UUID NULL,
    lease_expires_at_utc TIMESTAMPTZ NULL,
    CONSTRAINT ck_mail_delivery_slot_id CHECK (slot_id BETWEEN 0 AND 3),
    CONSTRAINT ck_mail_delivery_slot_lease
        CHECK ((lease_token IS NULL) = (lease_expires_at_utc IS NULL))
);

INSERT INTO "MailDeliverySlots" (slot_id)
VALUES (0), (1), (2), (3)
ON CONFLICT (slot_id) DO NOTHING;

CREATE INDEX IF NOT EXISTS ix_mail_outbox_pending
    ON "MailOutbox" (available_at_utc, created_at_utc, operation_id)
    WHERE delivered_at_utc IS NULL AND dead_at_utc IS NULL;

CREATE INDEX IF NOT EXISTS ix_mail_outbox_account_admission
    ON "MailOutbox" (account_id, created_at_utc DESC, operation_id);

CREATE INDEX IF NOT EXISTS ix_mail_outbox_account_active
    ON "MailOutbox" (account_id, purpose, created_at_utc DESC, operation_id)
    WHERE superseded_at_utc IS NULL;

CREATE UNIQUE INDEX IF NOT EXISTS ux_mail_outbox_current_generation
    ON "MailOutbox" (account_id, purpose)
    WHERE superseded_at_utc IS NULL;

CREATE INDEX IF NOT EXISTS ix_mail_outbox_destination_admission
    ON "MailOutbox" (destination_digest, created_at_utc DESC);

CREATE INDEX IF NOT EXISTS ix_mail_outbox_source_admission
    ON "MailOutbox" (source_digest, created_at_utc DESC)
    WHERE source_digest IS NOT NULL;

CREATE INDEX IF NOT EXISTS ix_mail_outbox_terminal
    ON "MailOutbox"
       ((COALESCE(delivered_at_utc, dead_at_utc)), operation_id)
    WHERE delivered_at_utc IS NOT NULL OR dead_at_utc IS NOT NULL;
"#;

const DOWN_SQL: &str = r#"
DROP TABLE IF EXISTS "MailDeliverySlots";
DROP TABLE IF EXISTS "MailOutbox";
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
    fn outbox_has_hard_message_retry_concurrency_and_retention_indexes() {
        assert!(UP_SQL.contains("octet_length(html_body) BETWEEN 0 AND 65536"));
        assert!(UP_SQL.contains("attempts BETWEEN 0 AND 8"));
        assert!(UP_SQL.contains("VALUES (0), (1), (2), (3)"));
        assert!(UP_SQL.contains("ix_mail_outbox_pending"));
        assert!(UP_SQL.contains("ix_mail_outbox_terminal"));
        assert!(UP_SQL.contains("ux_mail_outbox_current_generation"));
        assert!(UP_SQL.contains("destination_digest"));
        assert!(UP_SQL.contains("source_digest"));
    }
}
