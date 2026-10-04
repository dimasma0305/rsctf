//! Deployment-wide slots for bounded account-mail preparation.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "MailPreparationSlots" (
    slot_id              SMALLINT PRIMARY KEY,
    lease_token          UUID NULL,
    lease_expires_at_utc TIMESTAMPTZ NULL,
    CONSTRAINT ck_mail_preparation_slot_id CHECK (slot_id BETWEEN 0 AND 15),
    CONSTRAINT ck_mail_preparation_slot_lease
        CHECK ((lease_token IS NULL) = (lease_expires_at_utc IS NULL))
);

INSERT INTO "MailPreparationSlots" (slot_id)
SELECT slot_id FROM generate_series(0, 15) AS slot_id
ON CONFLICT (slot_id) DO NOTHING;
"#;

const DOWN_SQL: &str = r#"
DROP TABLE IF EXISTS "MailPreparationSlots";
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
    fn preparation_slots_are_fixed_and_lease_paired() {
        assert!(UP_SQL.contains("slot_id BETWEEN 0 AND 15"));
        assert!(UP_SQL.contains("(lease_token IS NULL) = (lease_expires_at_utc IS NULL)"));
        assert!(UP_SQL.contains("generate_series(0, 15)"));
        assert!(UP_SQL.contains("ON CONFLICT (slot_id) DO NOTHING"));
    }
}
