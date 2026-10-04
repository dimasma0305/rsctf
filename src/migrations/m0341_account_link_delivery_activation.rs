//! Bind digest-only account-link attempts to their durable mail delivery.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "AccountLinkAttempts"
    ADD COLUMN IF NOT EXISTS mail_operation_id UUID NULL;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'fk_account_link_mail_operation'
           AND conrelid = '"AccountLinkAttempts"'::regclass
    ) THEN
        ALTER TABLE "AccountLinkAttempts"
            ADD CONSTRAINT fk_account_link_mail_operation
            FOREIGN KEY (mail_operation_id)
            REFERENCES "MailOutbox" (operation_id)
            ON DELETE SET NULL
            NOT VALID;
    END IF;
END $$;

ALTER TABLE "AccountLinkAttempts"
    VALIDATE CONSTRAINT fk_account_link_mail_operation;

CREATE UNIQUE INDEX IF NOT EXISTS ux_account_link_mail_operation
    ON "AccountLinkAttempts" (mail_operation_id)
    WHERE mail_operation_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS ix_account_link_delivery_activation
    ON "AccountLinkAttempts" (mail_operation_id, expires_at_utc)
    WHERE mail_operation_id IS NOT NULL
      AND delivered_at_utc IS NULL
      AND consumed_at_utc IS NULL;
"#;

const DOWN_SQL: &str = r#"
DROP INDEX IF EXISTS ix_account_link_delivery_activation;
DROP INDEX IF EXISTS ux_account_link_mail_operation;
ALTER TABLE "AccountLinkAttempts"
    DROP CONSTRAINT IF EXISTS fk_account_link_mail_operation;
ALTER TABLE "AccountLinkAttempts"
    DROP COLUMN IF EXISTS mail_operation_id;
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
    fn activation_identity_is_nullable_unique_digest_only_and_recoverable() {
        assert!(UP_SQL.contains("mail_operation_id UUID NULL"));
        assert!(UP_SQL.contains("REFERENCES \"MailOutbox\" (operation_id)"));
        assert!(UP_SQL.contains("ux_account_link_mail_operation"));
        assert!(UP_SQL.contains("ix_account_link_delivery_activation"));
        assert!(!UP_SQL.contains("token TEXT"));
        assert!(!UP_SQL.contains("token VARCHAR"));
    }
}
