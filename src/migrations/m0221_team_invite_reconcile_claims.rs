//! Durable claims for invite-rotation BYOC reconciliation.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "TeamInviteOperations"
    ADD COLUMN IF NOT EXISTS reconcile_claim_id UUID NULL,
    ADD COLUMN IF NOT EXISTS reconcile_claim_expires_at_utc TIMESTAMPTZ NULL;

DO $$ BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_team_invite_reconcile_claim_pair'
           AND conrelid = '"TeamInviteOperations"'::regclass
    ) THEN
        ALTER TABLE "TeamInviteOperations"
            ADD CONSTRAINT ck_team_invite_reconcile_claim_pair CHECK (
                (reconcile_claim_id IS NULL) =
                (reconcile_claim_expires_at_utc IS NULL)
            );
    END IF;
END $$;

CREATE INDEX IF NOT EXISTS ix_team_invite_reconcile_pending
    ON "TeamInviteOperations" (team_id, operation_id)
    WHERE reconciled_at_utc IS NULL;
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

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn reconcile_claim_is_paired_and_pending_work_is_indexed() {
        assert!(UP_SQL.contains("ck_team_invite_reconcile_claim_pair"));
        assert!(UP_SQL.contains("WHERE reconciled_at_utc IS NULL"));
    }
}
