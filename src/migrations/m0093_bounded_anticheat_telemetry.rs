//! Bounded aggregate VPN telemetry and an evidence-family finding ledger.
//!
//! No table stores packet payloads, DNS names, or submitted flag plaintext.
//! High-volume tables contain only fixed-width counters or keyed hashes.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "AntiCheatTelemetryUsage" (
    game_id INTEGER PRIMARY KEY,
    logical_bytes BIGINT NOT NULL DEFAULT 0 CHECK (logical_bytes BETWEEN 0 AND 268435456),
    row_count BIGINT NOT NULL DEFAULT 0 CHECK (row_count >= 0),
    disabled_at_utc TIMESTAMPTZ NULL,
    updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT fk_anticheat_telemetry_usage_game
        FOREIGN KEY (game_id) REFERENCES "Games"(id) ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS "AntiCheatTelemetryGlobalUsage" (
    id SMALLINT PRIMARY KEY CHECK (id = 1),
    logical_bytes BIGINT NOT NULL DEFAULT 0 CHECK (logical_bytes BETWEEN 0 AND 5368709120),
    row_count BIGINT NOT NULL DEFAULT 0 CHECK (row_count >= 0),
    updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
INSERT INTO "AntiCheatTelemetryGlobalUsage" (id) VALUES (1) ON CONFLICT DO NOTHING;

CREATE TABLE IF NOT EXISTS "VpnFlowTelemetryBuckets" (
    id BIGSERIAL PRIMARY KEY,
    game_id INTEGER NOT NULL,
    user_id UUID NOT NULL,
    participation_id INTEGER NOT NULL,
    peer_id UUID NOT NULL,
    challenge_id INTEGER NULL,
    container_generation INTEGER NULL,
    bucket_start_utc TIMESTAMPTZ NOT NULL,
    packets_up BIGINT NOT NULL CHECK (packets_up >= 0),
    packets_down BIGINT NOT NULL CHECK (packets_down >= 0),
    bytes_up BIGINT NOT NULL CHECK (bytes_up >= 0),
    bytes_down BIGINT NOT NULL CHECK (bytes_down >= 0),
    distinct_destinations INTEGER NOT NULL CHECK (distinct_destinations >= 0),
    connection_count INTEGER NOT NULL CHECK (connection_count >= 0),
    active_seconds INTEGER NOT NULL CHECK (active_seconds BETWEEN 0 AND 300),
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT fk_vpn_flow_bucket_peer
        FOREIGN KEY (peer_id) REFERENCES "EventVpnUserPeers"(id) ON DELETE RESTRICT,
    CONSTRAINT fk_vpn_flow_bucket_participation
        FOREIGN KEY (game_id, participation_id)
        REFERENCES "Participations"(game_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_vpn_flow_bucket_challenge
        FOREIGN KEY (game_id, challenge_id)
        REFERENCES "GameChallenges"(game_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_vpn_flow_bucket_boundary CHECK (
        bucket_start_utc = date_trunc('minute', bucket_start_utc)
        AND EXTRACT(MINUTE FROM bucket_start_utc)::integer % 5 = 0
    )
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_vpn_flow_bucket_identity
    ON "VpnFlowTelemetryBuckets"(
        game_id, user_id, participation_id, peer_id,
        COALESCE(challenge_id, -1), COALESCE(container_generation, -1), bucket_start_utc
    );
CREATE INDEX IF NOT EXISTS ix_vpn_flow_bucket_game_time
    ON "VpnFlowTelemetryBuckets"(game_id, bucket_start_utc, participation_id);

CREATE TABLE IF NOT EXISTS "VpnDnsProviderBuckets" (
    id BIGSERIAL PRIMARY KEY,
    game_id INTEGER NOT NULL,
    user_id UUID NOT NULL,
    participation_id INTEGER NOT NULL,
    peer_id UUID NOT NULL,
    provider_category SMALLINT NOT NULL CHECK (provider_category BETWEEN 0 AND 31),
    bucket_start_utc TIMESTAMPTZ NOT NULL,
    query_count INTEGER NOT NULL CHECK (query_count >= 0),
    first_seen_at_utc TIMESTAMPTZ NOT NULL,
    last_seen_at_utc TIMESTAMPTZ NOT NULL,
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT fk_vpn_dns_bucket_peer
        FOREIGN KEY (peer_id) REFERENCES "EventVpnUserPeers"(id) ON DELETE RESTRICT,
    CONSTRAINT fk_vpn_dns_bucket_participation
        FOREIGN KEY (game_id, participation_id)
        REFERENCES "Participations"(game_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_vpn_dns_bucket_window CHECK (
        bucket_start_utc = date_trunc('minute', bucket_start_utc)
        AND EXTRACT(MINUTE FROM bucket_start_utc)::integer % 15 = 0
        AND first_seen_at_utc >= bucket_start_utc
        AND last_seen_at_utc >= first_seen_at_utc
        AND last_seen_at_utc < bucket_start_utc + INTERVAL '15 minutes'
    )
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_vpn_dns_provider_bucket_identity
    ON "VpnDnsProviderBuckets"(
        game_id, user_id, participation_id, peer_id, provider_category, bucket_start_utc
    );
CREATE INDEX IF NOT EXISTS ix_vpn_dns_provider_bucket_game_time
    ON "VpnDnsProviderBuckets"(game_id, bucket_start_utc, participation_id);

CREATE TABLE IF NOT EXISTS "VpnPeerNetworkObservations" (
    id BIGSERIAL PRIMARY KEY,
    game_id INTEGER NOT NULL,
    user_id UUID NOT NULL,
    participation_id INTEGER NOT NULL,
    peer_id UUID NOT NULL,
    endpoint_hash BYTEA NOT NULL CHECK (OCTET_LENGTH(endpoint_hash) = 32),
    source_asn BIGINT NULL CHECK (source_asn IS NULL OR source_asn BETWEEN 0 AND 4294967295),
    network_class SMALLINT NOT NULL CHECK (network_class BETWEEN 0 AND 7),
    first_seen_at_utc TIMESTAMPTZ NOT NULL,
    last_seen_at_utc TIMESTAMPTZ NOT NULL,
    handshake_count INTEGER NOT NULL CHECK (handshake_count >= 1),
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT fk_vpn_peer_network_peer
        FOREIGN KEY (peer_id) REFERENCES "EventVpnUserPeers"(id) ON DELETE RESTRICT,
    CONSTRAINT fk_vpn_peer_network_participation
        FOREIGN KEY (game_id, participation_id)
        REFERENCES "Participations"(game_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_vpn_peer_network_window CHECK (last_seen_at_utc >= first_seen_at_utc)
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_vpn_peer_network_observation
    ON "VpnPeerNetworkObservations"(game_id, peer_id, endpoint_hash, first_seen_at_utc);
CREATE INDEX IF NOT EXISTS ix_vpn_peer_network_game_time
    ON "VpnPeerNetworkObservations"(game_id, last_seen_at_utc, participation_id);

CREATE TABLE IF NOT EXISTS "VpnFlagTransportEvents" (
    id BIGSERIAL PRIMARY KEY,
    game_id INTEGER NOT NULL,
    challenge_id INTEGER NOT NULL,
    receiving_user_id UUID NOT NULL,
    receiving_participation_id INTEGER NOT NULL,
    owning_participation_id INTEGER NOT NULL,
    peer_id UUID NOT NULL,
    flag_value_hash BYTEA NOT NULL CHECK (OCTET_LENGTH(flag_value_hash) = 32),
    transport SMALLINT NOT NULL CHECK (transport BETWEEN 0 AND 15),
    direction SMALLINT NOT NULL CHECK (direction BETWEEN 0 AND 1),
    observed_at_utc TIMESTAMPTZ NOT NULL,
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT ck_vpn_flag_transport_cross_team CHECK (
        receiving_participation_id <> owning_participation_id
    ),
    CONSTRAINT fk_vpn_flag_transport_peer
        FOREIGN KEY (peer_id) REFERENCES "EventVpnUserPeers"(id) ON DELETE RESTRICT,
    CONSTRAINT fk_vpn_flag_transport_receiving
        FOREIGN KEY (game_id, receiving_participation_id)
        REFERENCES "Participations"(game_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_vpn_flag_transport_owning
        FOREIGN KEY (game_id, owning_participation_id)
        REFERENCES "Participations"(game_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_vpn_flag_transport_challenge
        FOREIGN KEY (game_id, challenge_id)
        REFERENCES "GameChallenges"(game_id, id) ON DELETE RESTRICT
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_vpn_flag_transport_dedup
    ON "VpnFlagTransportEvents"(
        game_id, challenge_id, receiving_participation_id,
        owning_participation_id, flag_value_hash, transport, direction
    );
CREATE INDEX IF NOT EXISTS ix_vpn_flag_transport_game_time
    ON "VpnFlagTransportEvents"(game_id, observed_at_utc, receiving_participation_id);

CREATE TABLE IF NOT EXISTS "AntiCheatTelemetryDrops" (
    id BIGSERIAL PRIMARY KEY,
    game_id INTEGER NULL,
    source SMALLINT NOT NULL CHECK (source BETWEEN 0 AND 15),
    reason SMALLINT NOT NULL CHECK (reason BETWEEN 0 AND 15),
    dropped_rows BIGINT NOT NULL CHECK (dropped_rows > 0),
    dropped_bytes BIGINT NOT NULL CHECK (dropped_bytes >= 0),
    bucket_start_utc TIMESTAMPTZ NOT NULL DEFAULT date_trunc('hour', clock_timestamp()),
    observed_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
CREATE UNIQUE INDEX IF NOT EXISTS ux_anticheat_telemetry_drops_bucket
    ON "AntiCheatTelemetryDrops"(
        COALESCE(game_id, -1), source, reason, bucket_start_utc
    );
CREATE INDEX IF NOT EXISTS ix_anticheat_telemetry_drops_game_time
    ON "AntiCheatTelemetryDrops"(game_id, observed_at_utc DESC);

CREATE TABLE IF NOT EXISTS "AntiCheatFindings" (
    id BIGSERIAL PRIMARY KEY,
    game_id INTEGER NOT NULL,
    participation_id INTEGER NOT NULL,
    user_id UUID NULL,
    challenge_id INTEGER NULL,
    detector_code TEXT NOT NULL CHECK (LENGTH(detector_code) BETWEEN 1 AND 64),
    detector_version INTEGER NOT NULL CHECK (detector_version >= 1),
    evidence_family SMALLINT NOT NULL CHECK (evidence_family BETWEEN 0 AND 5),
    evidence_tier SMALLINT NOT NULL CHECK (evidence_tier BETWEEN 0 AND 3),
    score_delta INTEGER NOT NULL CHECK (score_delta BETWEEN 0 AND 10000),
    evidence_key TEXT NOT NULL CHECK (LENGTH(evidence_key) BETWEEN 1 AND 160),
    occurred_at_utc TIMESTAMPTZ NOT NULL,
    details JSONB NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(details) = 'object'),
    shadow BOOLEAN NOT NULL DEFAULT TRUE,
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT fk_anticheat_finding_participation
        FOREIGN KEY (game_id, participation_id)
        REFERENCES "Participations"(game_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_anticheat_finding_challenge
        FOREIGN KEY (game_id, challenge_id)
        REFERENCES "GameChallenges"(game_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_anticheat_finding_context_score CHECK (
        evidence_tier <> 0 OR score_delta = 0
    ),
    CONSTRAINT ux_anticheat_findings_game_id UNIQUE (game_id, id)
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_anticheat_finding_evidence
    ON "AntiCheatFindings"(
        game_id, participation_id, detector_code, detector_version, evidence_key
    );
CREATE INDEX IF NOT EXISTS ix_anticheat_findings_report
    ON "AntiCheatFindings"(game_id, participation_id, occurred_at_utc, id);
CREATE INDEX IF NOT EXISTS ix_anticheat_findings_family
    ON "AntiCheatFindings"(game_id, participation_id, evidence_family, evidence_tier)
    WHERE shadow = FALSE;

CREATE TABLE IF NOT EXISTS "AntiCheatEvidenceRelationships" (
    id BIGSERIAL PRIMARY KEY,
    game_id INTEGER NOT NULL,
    finding_id BIGINT NOT NULL,
    related_finding_id BIGINT NULL,
    relation_kind SMALLINT NOT NULL CHECK (relation_kind BETWEEN 0 AND 6),
    related_source_type TEXT NULL CHECK (
        related_source_type IS NULL OR LENGTH(related_source_type) BETWEEN 1 AND 48
    ),
    related_source_key TEXT NULL CHECK (
        related_source_key IS NULL OR LENGTH(related_source_key) BETWEEN 1 AND 160
    ),
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT fk_anticheat_relationship_finding
        FOREIGN KEY (finding_id) REFERENCES "AntiCheatFindings"(id) ON DELETE RESTRICT,
    CONSTRAINT fk_anticheat_relationship_related_finding
        FOREIGN KEY (related_finding_id) REFERENCES "AntiCheatFindings"(id) ON DELETE RESTRICT,
    CONSTRAINT fk_anticheat_relationship_finding_game
        FOREIGN KEY (game_id, finding_id)
        REFERENCES "AntiCheatFindings"(game_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_anticheat_relationship_related_game
        FOREIGN KEY (game_id, related_finding_id)
        REFERENCES "AntiCheatFindings"(game_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_anticheat_relationship_target CHECK (
        (related_finding_id IS NOT NULL AND related_source_type IS NULL AND related_source_key IS NULL)
        OR
        (related_finding_id IS NULL AND related_source_type IS NOT NULL AND related_source_key IS NOT NULL)
    ),
    CONSTRAINT ck_anticheat_relationship_not_self CHECK (
        related_finding_id IS NULL OR related_finding_id <> finding_id
    )
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_anticheat_relationship_finding
    ON "AntiCheatEvidenceRelationships"(
        finding_id, relation_kind,
        COALESCE(related_finding_id, -1),
        COALESCE(related_source_type, ''),
        COALESCE(related_source_key, '')
    );
CREATE INDEX IF NOT EXISTS ix_anticheat_relationship_game
    ON "AntiCheatEvidenceRelationships"(game_id, finding_id);

CREATE TABLE IF NOT EXISTS "AntiCheatFindingReviews" (
    id BIGSERIAL PRIMARY KEY,
    finding_id BIGINT NOT NULL,
    game_id INTEGER NOT NULL,
    status SMALLINT NOT NULL CHECK (status BETWEEN 0 AND 4),
    reviewed_by_user_id UUID NOT NULL,
    note TEXT NULL CHECK (note IS NULL OR LENGTH(note) <= 4000),
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT fk_anticheat_finding_review_finding
        FOREIGN KEY (finding_id) REFERENCES "AntiCheatFindings"(id) ON DELETE RESTRICT,
    CONSTRAINT fk_anticheat_finding_review_game
        FOREIGN KEY (game_id, finding_id)
        REFERENCES "AntiCheatFindings"(game_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_anticheat_finding_review_actor
        FOREIGN KEY (reviewed_by_user_id) REFERENCES "AspNetUsers"(id) ON DELETE RESTRICT
);
CREATE INDEX IF NOT EXISTS ix_anticheat_finding_reviews_latest
    ON "AntiCheatFindingReviews"(finding_id, created_at_utc DESC, id DESC);

CREATE TABLE IF NOT EXISTS "AntiCheatTelemetryPurges" (
    id BIGSERIAL PRIMARY KEY,
    game_id INTEGER NULL,
    requested_by_user_id UUID NOT NULL,
    reason TEXT NOT NULL CHECK (LENGTH(BTRIM(reason)) BETWEEN 8 AND 512),
    rows_removed BIGINT NOT NULL CHECK (rows_removed >= 0),
    logical_bytes_removed BIGINT NOT NULL CHECK (logical_bytes_removed >= 0),
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT fk_anticheat_telemetry_purge_actor
        FOREIGN KEY (requested_by_user_id) REFERENCES "AspNetUsers"(id) ON DELETE RESTRICT
);

CREATE OR REPLACE FUNCTION reject_anticheat_finding_ledger_mutation()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION '% is append-only', TG_TABLE_NAME USING ERRCODE = '55000';
END;
$$;

DO $$
DECLARE table_name TEXT;
BEGIN
    FOREACH table_name IN ARRAY ARRAY[
        'AntiCheatFindings',
        'AntiCheatEvidenceRelationships',
        'AntiCheatFindingReviews',
        'AntiCheatTelemetryPurges'
    ]
    LOOP
        IF NOT EXISTS (
            SELECT 1 FROM pg_trigger
             WHERE tgname = 'tr_' || lower(table_name) || '_append_only'
               AND tgrelid = format('"%s"', table_name)::regclass
        ) THEN
            EXECUTE format(
                'CREATE TRIGGER %I BEFORE UPDATE OR DELETE ON %I '
                || 'FOR EACH ROW EXECUTE FUNCTION reject_anticheat_finding_ledger_mutation()',
                'tr_' || lower(table_name) || '_append_only', table_name
            );
        END IF;
    END LOOP;
END $$;
"#;

const DOWN_SQL: &str = r#"
DROP TRIGGER IF EXISTS tr_anticheattelemetrypurges_append_only ON "AntiCheatTelemetryPurges";
DROP TRIGGER IF EXISTS tr_anticheatfindingreviews_append_only ON "AntiCheatFindingReviews";
DROP TRIGGER IF EXISTS tr_anticheatevidencerelationships_append_only ON "AntiCheatEvidenceRelationships";
DROP TRIGGER IF EXISTS tr_anticheatfindings_append_only ON "AntiCheatFindings";
DROP FUNCTION IF EXISTS reject_anticheat_finding_ledger_mutation();
DROP TABLE IF EXISTS "AntiCheatTelemetryPurges";
DROP TABLE IF EXISTS "AntiCheatFindingReviews";
DROP TABLE IF EXISTS "AntiCheatEvidenceRelationships";
DROP TABLE IF EXISTS "AntiCheatFindings";
DROP TABLE IF EXISTS "AntiCheatTelemetryDrops";
DROP TABLE IF EXISTS "VpnFlagTransportEvents";
DROP TABLE IF EXISTS "VpnPeerNetworkObservations";
DROP TABLE IF EXISTS "VpnDnsProviderBuckets";
DROP TABLE IF EXISTS "VpnFlowTelemetryBuckets";
DROP TABLE IF EXISTS "AntiCheatTelemetryGlobalUsage";
DROP TABLE IF EXISTS "AntiCheatTelemetryUsage";
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
    fn telemetry_is_aggregate_bounded_and_context_cannot_score() {
        assert!(UP_SQL.contains("logical_bytes BETWEEN 0 AND 268435456"));
        assert!(UP_SQL.contains("logical_bytes BETWEEN 0 AND 5368709120"));
        assert!(UP_SQL
            .contains("active_seconds INTEGER NOT NULL CHECK (active_seconds BETWEEN 0 AND 300)"));
        assert!(UP_SQL.contains("evidence_tier <> 0 OR score_delta = 0"));
        for forbidden in ["packet_payload", "dns_name", "flag_plaintext"] {
            assert!(!UP_SQL.contains(forbidden));
        }
    }
}
