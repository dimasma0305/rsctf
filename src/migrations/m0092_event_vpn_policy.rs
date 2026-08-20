//! Event-scoped VPN access policy and per-user WireGuard credential intent.
//!
//! All policy switches default off. Existing games therefore retain their
//! public access behavior until an organizer explicitly enables a feature.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP_SQL: &str = r#"
ALTER TABLE "Games"
    ADD COLUMN IF NOT EXISTS vpn_access_required BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS vpn_behavior_telemetry_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS vpn_flag_scan_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS vpn_provider_dns_telemetry_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS vpn_source_asn_telemetry_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS vpn_device_sharing_telemetry_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS vpn_policy_revision BIGINT NOT NULL DEFAULT 1;

-- A fresh install may already contain these entity-derived columns before this
-- migration runs. `ADD COLUMN IF NOT EXISTS` then leaves their defaults alone,
-- so set the opt-in defaults explicitly for both fresh and upgraded schemas.
ALTER TABLE "Games"
    ALTER COLUMN vpn_access_required SET DEFAULT FALSE,
    ALTER COLUMN vpn_behavior_telemetry_enabled SET DEFAULT FALSE,
    ALTER COLUMN vpn_flag_scan_enabled SET DEFAULT FALSE,
    ALTER COLUMN vpn_provider_dns_telemetry_enabled SET DEFAULT FALSE,
    ALTER COLUMN vpn_source_asn_telemetry_enabled SET DEFAULT FALSE,
    ALTER COLUMN vpn_device_sharing_telemetry_enabled SET DEFAULT FALSE,
    ALTER COLUMN vpn_policy_revision SET DEFAULT 1;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_games_vpn_policy_revision'
           AND conrelid = '"Games"'::regclass
    ) THEN
        ALTER TABLE "Games"
            ADD CONSTRAINT ck_games_vpn_policy_revision
            CHECK (vpn_policy_revision >= 1);
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_games_vpn_telemetry_requires_access'
           AND conrelid = '"Games"'::regclass
    ) THEN
        ALTER TABLE "Games"
            ADD CONSTRAINT ck_games_vpn_telemetry_requires_access CHECK (
                vpn_access_required
                OR NOT (
                    vpn_behavior_telemetry_enabled OR vpn_flag_scan_enabled
                    OR vpn_provider_dns_telemetry_enabled
                    OR vpn_source_asn_telemetry_enabled
                    OR vpn_device_sharing_telemetry_enabled
                )
            );
    END IF;
END $$;

CREATE TABLE IF NOT EXISTS "EventVpnUserPeers" (
    id UUID PRIMARY KEY,
    game_id INTEGER NOT NULL,
    user_id UUID NOT NULL,
    participation_id INTEGER NOT NULL,
    public_key TEXT NOT NULL,
    private_key_ciphertext BYTEA NOT NULL,
    private_key_nonce BYTEA NOT NULL CHECK (OCTET_LENGTH(private_key_nonce) = 12),
    address TEXT NOT NULL,
    generation INTEGER NOT NULL DEFAULT 1 CHECK (generation >= 1),
    issued_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    last_config_download_at_utc TIMESTAMPTZ NULL,
    revoked_at_utc TIMESTAMPTZ NULL,
    CONSTRAINT fk_event_vpn_user_peer_game
        FOREIGN KEY (game_id) REFERENCES "Games"(id) ON DELETE RESTRICT,
    CONSTRAINT fk_event_vpn_user_peer_user
        FOREIGN KEY (user_id) REFERENCES "AspNetUsers"(id) ON DELETE RESTRICT,
    CONSTRAINT fk_event_vpn_user_peer_participation
        FOREIGN KEY (game_id, participation_id)
        REFERENCES "Participations"(game_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_event_vpn_user_peer_key_shape CHECK (
        LENGTH(public_key) BETWEEN 40 AND 64
        AND OCTET_LENGTH(private_key_ciphertext) BETWEEN 32 AND 256
    ),
    CONSTRAINT ck_event_vpn_user_peer_address CHECK (
        address::inet = host(address::inet)::inet
        AND family(address::inet) = 4
    )
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_event_vpn_user_peers_public_key
    ON "EventVpnUserPeers"(public_key);
CREATE UNIQUE INDEX IF NOT EXISTS ux_event_vpn_user_peers_address
    ON "EventVpnUserPeers"(address);
CREATE UNIQUE INDEX IF NOT EXISTS ux_event_vpn_user_peers_live_user
    ON "EventVpnUserPeers"(game_id, user_id)
    WHERE revoked_at_utc IS NULL;
CREATE INDEX IF NOT EXISTS ix_event_vpn_user_peers_live_game
    ON "EventVpnUserPeers"(game_id, participation_id, user_id)
    WHERE revoked_at_utc IS NULL;

CREATE TABLE IF NOT EXISTS "EventVpnGateOverrides" (
    id UUID PRIMARY KEY,
    game_id INTEGER NOT NULL,
    created_by_user_id UUID NOT NULL,
    reason TEXT NOT NULL CHECK (LENGTH(BTRIM(reason)) BETWEEN 8 AND 512),
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    expires_at_utc TIMESTAMPTZ NOT NULL,
    revoked_at_utc TIMESTAMPTZ NULL,
    CONSTRAINT fk_event_vpn_gate_override_game
        FOREIGN KEY (game_id) REFERENCES "Games"(id) ON DELETE RESTRICT,
    CONSTRAINT fk_event_vpn_gate_override_actor
        FOREIGN KEY (created_by_user_id) REFERENCES "AspNetUsers"(id) ON DELETE RESTRICT,
    CONSTRAINT ck_event_vpn_gate_override_window CHECK (
        expires_at_utc > created_at_utc
        AND expires_at_utc <= created_at_utc + INTERVAL '60 minutes'
        AND (revoked_at_utc IS NULL OR revoked_at_utc >= created_at_utc)
    )
);

CREATE INDEX IF NOT EXISTS ix_event_vpn_gate_overrides_active
    ON "EventVpnGateOverrides"(game_id, expires_at_utc DESC)
    WHERE revoked_at_utc IS NULL;

CREATE TABLE IF NOT EXISTS "EventVpnPolicyAudit" (
    id BIGSERIAL PRIMARY KEY,
    game_id INTEGER NOT NULL,
    actor_user_id UUID NOT NULL,
    old_revision BIGINT NOT NULL,
    new_revision BIGINT NOT NULL,
    old_policy JSONB NOT NULL,
    new_policy JSONB NOT NULL,
    reason TEXT NULL CHECK (reason IS NULL OR LENGTH(reason) <= 512),
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT fk_event_vpn_policy_audit_game
        FOREIGN KEY (game_id) REFERENCES "Games"(id) ON DELETE RESTRICT,
    CONSTRAINT fk_event_vpn_policy_audit_actor
        FOREIGN KEY (actor_user_id) REFERENCES "AspNetUsers"(id) ON DELETE RESTRICT,
    CONSTRAINT ck_event_vpn_policy_audit_revision CHECK (
        old_revision >= 1 AND new_revision = old_revision + 1
    )
);

CREATE INDEX IF NOT EXISTS ix_event_vpn_policy_audit_game_time
    ON "EventVpnPolicyAudit"(game_id, created_at_utc DESC, id DESC);

CREATE OR REPLACE FUNCTION guard_event_vpn_peer_mutation()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'EventVpnUserPeers cannot be deleted' USING ERRCODE = '55000';
    END IF;
    IF OLD.id IS DISTINCT FROM NEW.id
       OR OLD.game_id IS DISTINCT FROM NEW.game_id
       OR OLD.user_id IS DISTINCT FROM NEW.user_id
       OR OLD.participation_id IS DISTINCT FROM NEW.participation_id
       OR OLD.public_key IS DISTINCT FROM NEW.public_key
       OR OLD.private_key_ciphertext IS DISTINCT FROM NEW.private_key_ciphertext
       OR OLD.private_key_nonce IS DISTINCT FROM NEW.private_key_nonce
       OR OLD.address IS DISTINCT FROM NEW.address
       OR OLD.generation IS DISTINCT FROM NEW.generation
       OR OLD.issued_at_utc IS DISTINCT FROM NEW.issued_at_utc
       OR (OLD.revoked_at_utc IS NOT NULL AND NEW IS DISTINCT FROM OLD)
       OR (OLD.revoked_at_utc IS NULL AND NEW.revoked_at_utc IS NOT NULL
           AND NEW.revoked_at_utc < OLD.issued_at_utc)
       OR (OLD.last_config_download_at_utc IS NOT NULL
           AND NEW.last_config_download_at_utc < OLD.last_config_download_at_utc)
       OR (NEW.last_config_download_at_utc IS NOT NULL
           AND NEW.last_config_download_at_utc < OLD.issued_at_utc)
    THEN
        RAISE EXCEPTION 'EventVpnUserPeers provenance is immutable' USING ERRCODE = '55000';
    END IF;
    RETURN NEW;
END;
$$;

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

CREATE OR REPLACE FUNCTION reject_event_vpn_policy_audit_mutation()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'EventVpnPolicyAudit is append-only' USING ERRCODE = '55000';
END;
$$;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_trigger
         WHERE tgname = 'tr_event_vpn_user_peers_immutable'
           AND tgrelid = '"EventVpnUserPeers"'::regclass
    ) THEN
        CREATE TRIGGER tr_event_vpn_user_peers_immutable
        BEFORE UPDATE OR DELETE ON "EventVpnUserPeers"
        FOR EACH ROW EXECUTE FUNCTION guard_event_vpn_peer_mutation();
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_trigger
         WHERE tgname = 'tr_event_vpn_gate_overrides_immutable'
           AND tgrelid = '"EventVpnGateOverrides"'::regclass
    ) THEN
        CREATE TRIGGER tr_event_vpn_gate_overrides_immutable
        BEFORE UPDATE OR DELETE ON "EventVpnGateOverrides"
        FOR EACH ROW EXECUTE FUNCTION guard_event_vpn_override_mutation();
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_trigger
         WHERE tgname = 'tr_event_vpn_policy_audit_append_only'
           AND tgrelid = '"EventVpnPolicyAudit"'::regclass
    ) THEN
        CREATE TRIGGER tr_event_vpn_policy_audit_append_only
        BEFORE UPDATE OR DELETE ON "EventVpnPolicyAudit"
        FOR EACH ROW EXECUTE FUNCTION reject_event_vpn_policy_audit_mutation();
    END IF;
END $$;
"#;

const DOWN_SQL: &str = r#"
DROP TRIGGER IF EXISTS tr_event_vpn_policy_audit_append_only ON "EventVpnPolicyAudit";
DROP TRIGGER IF EXISTS tr_event_vpn_gate_overrides_immutable ON "EventVpnGateOverrides";
DROP TRIGGER IF EXISTS tr_event_vpn_user_peers_immutable ON "EventVpnUserPeers";
DROP FUNCTION IF EXISTS reject_event_vpn_policy_audit_mutation();
DROP FUNCTION IF EXISTS guard_event_vpn_override_mutation();
DROP FUNCTION IF EXISTS guard_event_vpn_peer_mutation();
DROP TABLE IF EXISTS "EventVpnPolicyAudit";
DROP TABLE IF EXISTS "EventVpnGateOverrides";
DROP TABLE IF EXISTS "EventVpnUserPeers";
ALTER TABLE "Games"
    DROP CONSTRAINT IF EXISTS ck_games_vpn_telemetry_requires_access,
    DROP CONSTRAINT IF EXISTS ck_games_vpn_policy_revision,
    DROP COLUMN IF EXISTS vpn_policy_revision,
    DROP COLUMN IF EXISTS vpn_device_sharing_telemetry_enabled,
    DROP COLUMN IF EXISTS vpn_source_asn_telemetry_enabled,
    DROP COLUMN IF EXISTS vpn_provider_dns_telemetry_enabled,
    DROP COLUMN IF EXISTS vpn_flag_scan_enabled,
    DROP COLUMN IF EXISTS vpn_behavior_telemetry_enabled,
    DROP COLUMN IF EXISTS vpn_access_required;
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
    fn policy_is_opt_in_and_overrides_are_bounded() {
        assert!(UP_SQL.contains("vpn_access_required BOOLEAN NOT NULL DEFAULT FALSE"));
        assert!(UP_SQL.contains("ALTER COLUMN vpn_access_required SET DEFAULT FALSE"));
        assert!(UP_SQL.contains("ALTER COLUMN vpn_policy_revision SET DEFAULT 1"));
        assert!(UP_SQL.contains("INTERVAL '60 minutes'"));
        assert!(UP_SQL.contains("WHERE revoked_at_utc IS NULL"));
        assert!(UP_SQL.contains("private_key_ciphertext BYTEA NOT NULL"));
        assert!(!UP_SQL.contains("private_key TEXT"));
    }
}
