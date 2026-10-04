//! Durable terminal results for account links delivered through the mail outbox.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "MailOutbox"
    ADD COLUMN IF NOT EXISTS consumed_at_utc TIMESTAMPTZ NULL;

CREATE INDEX IF NOT EXISTS ix_mail_outbox_consumed
    ON "MailOutbox" (consumed_at_utc, operation_id)
    WHERE consumed_at_utc IS NOT NULL;
"#;

const DOWN_SQL: &str = r#"
DROP INDEX IF EXISTS ix_mail_outbox_consumed;
ALTER TABLE "MailOutbox" DROP COLUMN IF EXISTS consumed_at_utc;
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
    fn mail_link_terminal_results_are_durable_and_bounded_by_outbox_retention() {
        assert!(UP_SQL.contains("consumed_at_utc TIMESTAMPTZ"));
        assert!(UP_SQL.contains("ix_mail_outbox_consumed"));
    }
}
