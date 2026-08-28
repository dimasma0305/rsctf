//! Exactly-once temporary Event-VPN override transitions and bounded history.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "EventVpnGateOverrides"
    ADD COLUMN IF NOT EXISTS policy_revision BIGINT NULL,
    ADD COLUMN IF NOT EXISTS revoked_by_user_id UUID NULL,
    ADD COLUMN IF NOT EXISTS revoke_policy_revision BIGINT NULL;

CREATE TABLE IF NOT EXISTS "EventVpnOverrideOperations" (
    game_id            INTEGER NOT NULL,
    operation_id       UUID NOT NULL,
    actor_user_id      UUID NOT NULL,
    action             VARCHAR(8) NOT NULL,
    override_id        UUID NOT NULL,
    request_digest     BYTEA NOT NULL CHECK (OCTET_LENGTH(request_digest) = 32),
    result_revision    BIGINT NOT NULL CHECK (result_revision >= 1),
    created_at_utc     TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (game_id, operation_id),
    CONSTRAINT fk_event_vpn_override_operation_game
        FOREIGN KEY (game_id) REFERENCES "Games"(id) ON DELETE RESTRICT,
    CONSTRAINT fk_event_vpn_override_operation_actor
        FOREIGN KEY (actor_user_id) REFERENCES "AspNetUsers"(id) ON DELETE RESTRICT,
    CONSTRAINT fk_event_vpn_override_operation_override
        FOREIGN KEY (override_id) REFERENCES "EventVpnGateOverrides"(id) ON DELETE RESTRICT,
    CONSTRAINT ck_event_vpn_override_operation_action
        CHECK (action IN ('create', 'revoke'))
);

CREATE INDEX IF NOT EXISTS ix_event_vpn_override_operations_retention
    ON "EventVpnOverrideOperations" (created_at_utc, game_id, operation_id);
CREATE INDEX IF NOT EXISTS ix_event_vpn_gate_overrides_active_complete
    ON "EventVpnGateOverrides" (game_id, created_at_utc DESC, id DESC)
    WHERE revoked_at_utc IS NULL;

-- Preserve an immutable 30-day audit window while allowing the bounded
-- maintenance path to remove terminal history after its operation record has
-- expired. Active grants can never be deleted.
CREATE OR REPLACE FUNCTION guard_event_vpn_override_mutation()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        IF (OLD.revoked_at_utc IS NOT NULL OR OLD.expires_at_utc <= clock_timestamp())
           AND OLD.created_at_utc < clock_timestamp() - INTERVAL '30 days'
        THEN
            RETURN OLD;
        END IF;
        RAISE EXCEPTION 'EventVpnGateOverrides cannot be deleted inside retention'
            USING ERRCODE = '55000';
    END IF;
    IF OLD.id IS DISTINCT FROM NEW.id
       OR OLD.game_id IS DISTINCT FROM NEW.game_id
       OR OLD.created_by_user_id IS DISTINCT FROM NEW.created_by_user_id
       OR OLD.reason IS DISTINCT FROM NEW.reason
       OR OLD.created_at_utc IS DISTINCT FROM NEW.created_at_utc
       OR OLD.expires_at_utc IS DISTINCT FROM NEW.expires_at_utc
       OR OLD.policy_revision IS DISTINCT FROM NEW.policy_revision
       OR OLD.revoked_at_utc IS NOT NULL
       OR NEW.revoked_at_utc IS NULL
    THEN
        RAISE EXCEPTION 'EventVpnGateOverrides provenance is immutable' USING ERRCODE = '55000';
    END IF;
    RETURN NEW;
END;
$$;
"#;

const DOWN_SQL: &str = r#"
DROP TABLE IF EXISTS "EventVpnOverrideOperations";
DROP INDEX IF EXISTS ix_event_vpn_gate_overrides_active_complete;
CREATE OR REPLACE FUNCTION guard_event_vpn_override_mutation()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'EventVpnGateOverrides cannot be deleted' USING ERRCODE = '55000';
    END IF;
    IF OLD.id IS DISTINCT FROM NEW.id
       OR OLD.game_id IS DISTINCT FROM NEW.game_id
       OR OLD.created_by_user_id IS DISTINCT FROM NEW.created_by_user_id
       OR OLD.reason IS DISTINCT FROM NEW.reason
       OR OLD.created_at_utc IS DISTINCT FROM NEW.created_at_utc
       OR OLD.expires_at_utc IS DISTINCT FROM NEW.expires_at_utc
       OR OLD.revoked_at_utc IS NOT NULL
       OR NEW.revoked_at_utc IS NULL
    THEN
        RAISE EXCEPTION 'EventVpnGateOverrides provenance is immutable' USING ERRCODE = '55000';
    END IF;
    RETURN NEW;
END;
$$;
ALTER TABLE "EventVpnGateOverrides"
    DROP COLUMN IF EXISTS revoke_policy_revision,
    DROP COLUMN IF EXISTS revoked_by_user_id,
    DROP COLUMN IF EXISTS policy_revision;
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
    use super::{DOWN_SQL, UP_SQL};

    #[test]
    fn operation_identity_and_active_listing_are_indexed() {
        assert!(UP_SQL.contains("PRIMARY KEY (game_id, operation_id)"));
        assert!(UP_SQL.contains("OCTET_LENGTH(request_digest) = 32"));
        assert!(UP_SQL.contains("WHERE revoked_at_utc IS NULL"));
        assert!(!DOWN_SQL.contains("OLD.policy_revision"));
    }
}
