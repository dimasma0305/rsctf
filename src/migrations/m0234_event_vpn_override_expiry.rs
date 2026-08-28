//! Durable reconciliation receipts for naturally expired Event-VPN bypasses.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "EventVpnOverrideExpirations" (
    override_id       UUID PRIMARY KEY,
    game_id           INTEGER NOT NULL,
    policy_revision   BIGINT NOT NULL CHECK (policy_revision >= 1),
    reconciled_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT fk_event_vpn_override_expiration_override
        FOREIGN KEY (override_id) REFERENCES "EventVpnGateOverrides"(id) ON DELETE CASCADE,
    CONSTRAINT fk_event_vpn_override_expiration_game
        FOREIGN KEY (game_id) REFERENCES "Games"(id) ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS ix_event_vpn_override_expirations_game_revision
    ON "EventVpnOverrideExpirations" (game_id, policy_revision, override_id);

CREATE INDEX IF NOT EXISTS ix_event_vpn_gate_overrides_expiry_reconcile
    ON "EventVpnGateOverrides" (expires_at_utc, game_id, id)
    WHERE revoked_at_utc IS NULL;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Production migrations are forward-only. Retain expiry receipts so a
        // rollback cannot advance the same VPN policy transition a second time.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn expiration_receipts_are_unique_bounded_and_indexed() {
        assert!(UP_SQL.contains("override_id       UUID PRIMARY KEY"));
        assert!(UP_SQL.contains("ON DELETE CASCADE"));
        assert!(UP_SQL.contains("expires_at_utc, game_id, id"));
        assert!(UP_SQL.contains("WHERE revoked_at_utc IS NULL"));
    }
}
