//! Commit-ordered source versions and delta indexes for anti-cheat evidence.

use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
pub(crate) const UP_SQL: &str = r#"
-- Drain established evidence writers at their outermost fences before taking
-- child-table locks for the backfill/trigger hand-off. Games is the canonical
-- audit-intake fence; telemetry ingest and purge both serialize through the
-- per-game usage row but touch the three VPN child tables in opposite order.
-- The outer locks prevent a child-lock inversion during this one-time cutover.
LOCK TABLE "Games" IN EXCLUSIVE MODE;
LOCK TABLE "AntiCheatTelemetryUsage" IN EXCLUSIVE MODE;
LOCK TABLE "SuspicionEvents", "Participations", "IdentityObservations",
           "VpnDnsProviderBuckets", "VpnPeerNetworkObservations",
           "VpnFlagTransportEvents", "ContainerAccessEvents",
           "SuspicionEvaluationOutbox", "CheatInfo"
  IN SHARE ROW EXCLUSIVE MODE;

ALTER TABLE "SuspicionEvaluationOutbox"
    ADD COLUMN IF NOT EXISTS reconciliation_version BIGINT NULL;
ALTER TABLE "IdentityObservations"
    ADD COLUMN IF NOT EXISTS reconciliation_version BIGINT NULL;
ALTER TABLE "VpnDnsProviderBuckets"
    ADD COLUMN IF NOT EXISTS reconciliation_version BIGINT NULL;
ALTER TABLE "VpnPeerNetworkObservations"
    ADD COLUMN IF NOT EXISTS reconciliation_version BIGINT NULL;
ALTER TABLE "VpnFlagTransportEvents"
    ADD COLUMN IF NOT EXISTS reconciliation_version BIGINT NULL;
ALTER TABLE "ContainerAccessEvents"
    ADD COLUMN IF NOT EXISTS reconciliation_version BIGINT NULL;
ALTER TABLE "SuspicionEvents"
    ADD COLUMN IF NOT EXISTS reconciliation_version BIGINT NULL;
ALTER TABLE "CheatInfo"
    ADD COLUMN IF NOT EXISTS reconciliation_version BIGINT NULL;
ALTER TABLE "Participations"
    ADD COLUMN IF NOT EXISTS reconciliation_version BIGINT NULL;

-- Keep a transactional rerun private while filling the legacy NULLs. A failed
-- normal migration attempt rolls back atomically; a completed rerun has no
-- NULL versions left to rank again.
DROP TRIGGER IF EXISTS tr_suspicion_outbox_anticheat_dirty
    ON "SuspicionEvaluationOutbox";
DROP TRIGGER IF EXISTS tr_aa_suspicion_outbox_completion_version
    ON "SuspicionEvaluationOutbox";
DROP TRIGGER IF EXISTS tr_identity_observation_anticheat_dirty
    ON "IdentityObservations";
DROP TRIGGER IF EXISTS tr_vpn_dns_anticheat_dirty ON "VpnDnsProviderBuckets";
DROP TRIGGER IF EXISTS tr_vpn_peer_anticheat_dirty
    ON "VpnPeerNetworkObservations";
DROP TRIGGER IF EXISTS tr_vpn_flag_anticheat_dirty ON "VpnFlagTransportEvents";
DROP TRIGGER IF EXISTS tr_container_access_anticheat_dirty
    ON "ContainerAccessEvents";
DROP TRIGGER IF EXISTS tr_suspicion_event_anticheat_dirty ON "SuspicionEvents";
DROP TRIGGER IF EXISTS tr_cheat_info_anticheat_dirty ON "CheatInfo";
DROP TRIGGER IF EXISTS tr_participation_anticheat_dirty ON "Participations";
DROP TRIGGER IF EXISTS zz_suspicion_outbox_anticheat_stamp
    ON "SuspicionEvaluationOutbox";
DROP TRIGGER IF EXISTS zz_identity_observation_anticheat_stamp
    ON "IdentityObservations";
DROP TRIGGER IF EXISTS zz_vpn_dns_anticheat_stamp ON "VpnDnsProviderBuckets";
DROP TRIGGER IF EXISTS zz_vpn_peer_anticheat_stamp
    ON "VpnPeerNetworkObservations";
DROP TRIGGER IF EXISTS zz_vpn_flag_anticheat_stamp ON "VpnFlagTransportEvents";
DROP TRIGGER IF EXISTS zz_container_access_anticheat_stamp
    ON "ContainerAccessEvents";
DROP TRIGGER IF EXISTS zz_suspicion_event_anticheat_stamp ON "SuspicionEvents";
DROP TRIGGER IF EXISTS zz_cheat_info_anticheat_stamp ON "CheatInfo";
DROP TRIGGER IF EXISTS zz_participation_anticheat_stamp ON "Participations";
DROP TRIGGER IF EXISTS zz_vpn_dns_reconciliation_version_immutable
    ON "VpnDnsProviderBuckets";
DROP TRIGGER IF EXISTS zz_vpn_peer_reconciliation_version_immutable
    ON "VpnPeerNetworkObservations";
DROP TRIGGER IF EXISTS zz_vpn_flag_reconciliation_version_immutable
    ON "VpnFlagTransportEvents";
DROP TRIGGER IF EXISTS zz_participation_reconciliation_version_immutable
    ON "Participations";

-- Append-only guards predate this database-owned column. Disable them only
-- inside the writer fence while legacy rows receive their initial version.
ALTER TABLE "IdentityObservations"
    DISABLE TRIGGER tr_identity_observations_append_only;
ALTER TABLE "ContainerAccessEvents"
    DISABLE TRIGGER trg_containeraccess_immutable;
ALTER TABLE "SuspicionEvents"
    DISABLE TRIGGER trg_suspicionevents_immutable;
ALTER TABLE "CheatInfo" DISABLE TRIGGER trg_cheatinfo_immutable;

CREATE OR REPLACE FUNCTION rsctf_mark_anticheat_reconciliation_dirty(
    dirty_game_id INTEGER,
    dirty_source_kind SMALLINT,
    dirty_source_version BIGINT
) RETURNS void LANGUAGE plpgsql AS $$
DECLARE changed BOOLEAN;
BEGIN
    IF dirty_game_id IS NULL OR dirty_source_kind NOT BETWEEN 0 AND 9
       OR dirty_source_version < 0 THEN
        RETURN;
    END IF;
    INSERT INTO "AntiCheatReconciliationQueue" (game_id)
    VALUES (dirty_game_id) ON CONFLICT (game_id) DO NOTHING;
    PERFORM 1 FROM "AntiCheatReconciliationQueue"
     WHERE game_id = dirty_game_id FOR UPDATE;
    WITH dirtied AS (
        INSERT INTO "AntiCheatReconciliationSources"
            (game_id, source_kind, dirty_version, dirtied_at_utc)
        VALUES (dirty_game_id, dirty_source_kind, dirty_source_version,
                clock_timestamp())
        ON CONFLICT (game_id, source_kind) DO UPDATE
          SET dirty_version = EXCLUDED.dirty_version,
              dirtied_at_utc = EXCLUDED.dirtied_at_utc
        WHERE "AntiCheatReconciliationSources".dirty_version
              < EXCLUDED.dirty_version
        RETURNING TRUE
    ) SELECT COALESCE(bool_or(TRUE), FALSE) INTO changed FROM dirtied;
    IF changed THEN
        UPDATE "AntiCheatReconciliationQueue"
           SET desired_generation = desired_generation + 1,
               available_at_utc = LEAST(available_at_utc, clock_timestamp()),
               updated_at_utc = clock_timestamp()
         WHERE game_id = dirty_game_id;
    END IF;
END
$$;

-- Give every legacy row a version above m0283's captured maximum. This forces
-- one catch-up even if a new replica somehow ran between the two migrations.
WITH ranked AS MATERIALIZED (
    SELECT job.id, source.dirty_version
             + row_number() OVER (PARTITION BY job.game_id ORDER BY job.id)
               AS version
      FROM "SuspicionEvaluationOutbox" job
      JOIN "AntiCheatReconciliationSources" source
        ON source.game_id = job.game_id AND source.source_kind = 0
     WHERE job.completed_at_utc IS NOT NULL
       AND job.reconciliation_version IS NULL
)
UPDATE "SuspicionEvaluationOutbox" job
   SET reconciliation_version = ranked.version
  FROM ranked WHERE job.id = ranked.id;
WITH ranked AS MATERIALIZED (
    SELECT observation.id, source.dirty_version
             + row_number() OVER (PARTITION BY observation.game_id ORDER BY observation.id)
               AS version
      FROM "IdentityObservations" observation
      JOIN "AntiCheatReconciliationSources" source
        ON source.game_id = observation.game_id AND source.source_kind = 1
     WHERE observation.game_id IS NOT NULL
       AND observation.reconciliation_version IS NULL
)
UPDATE "IdentityObservations" observation
   SET reconciliation_version = ranked.version
  FROM ranked WHERE observation.id = ranked.id;
WITH ranked AS MATERIALIZED (
    SELECT row.id, source.dirty_version
             + row_number() OVER (PARTITION BY row.game_id ORDER BY row.id) AS version
      FROM "VpnDnsProviderBuckets" row
      JOIN "AntiCheatReconciliationSources" source
        ON source.game_id = row.game_id AND source.source_kind = 3
     WHERE row.reconciliation_version IS NULL
)
UPDATE "VpnDnsProviderBuckets" row
   SET reconciliation_version = ranked.version
  FROM ranked WHERE row.id = ranked.id;
WITH ranked AS MATERIALIZED (
    SELECT row.id, source.dirty_version
             + row_number() OVER (PARTITION BY row.game_id ORDER BY row.id) AS version
      FROM "VpnPeerNetworkObservations" row
      JOIN "AntiCheatReconciliationSources" source
        ON source.game_id = row.game_id AND source.source_kind = 4
     WHERE row.reconciliation_version IS NULL
)
UPDATE "VpnPeerNetworkObservations" row
   SET reconciliation_version = ranked.version
  FROM ranked WHERE row.id = ranked.id;
WITH ranked AS MATERIALIZED (
    SELECT row.id, source.dirty_version
             + row_number() OVER (PARTITION BY row.game_id ORDER BY row.id) AS version
      FROM "VpnFlagTransportEvents" row
      JOIN "AntiCheatReconciliationSources" source
        ON source.game_id = row.game_id AND source.source_kind = 5
     WHERE row.reconciliation_version IS NULL
)
UPDATE "VpnFlagTransportEvents" row
   SET reconciliation_version = ranked.version
  FROM ranked WHERE row.id = ranked.id;
WITH ranked AS MATERIALIZED (
    SELECT row.id, source.dirty_version
             + row_number() OVER (PARTITION BY row.game_id ORDER BY row.id) AS version
      FROM "ContainerAccessEvents" row
      JOIN "AntiCheatReconciliationSources" source
        ON source.game_id = row.game_id AND source.source_kind = 6
     WHERE row.reconciliation_version IS NULL
)
UPDATE "ContainerAccessEvents" row
   SET reconciliation_version = ranked.version
  FROM ranked WHERE row.id = ranked.id;
WITH ranked AS MATERIALIZED (
    SELECT row.id, source.dirty_version
             + row_number() OVER (PARTITION BY row.game_id ORDER BY row.id) AS version
      FROM "SuspicionEvents" row
      JOIN "AntiCheatReconciliationSources" source
        ON source.game_id = row.game_id AND source.source_kind = 7
     WHERE row.reconciliation_version IS NULL
)
UPDATE "SuspicionEvents" row
   SET reconciliation_version = ranked.version
  FROM ranked WHERE row.id = ranked.id;
WITH ranked AS MATERIALIZED (
    SELECT row.id, source.dirty_version
             + row_number() OVER (PARTITION BY row.game_id ORDER BY row.id) AS version
      FROM "CheatInfo" row
      JOIN "AntiCheatReconciliationSources" source
        ON source.game_id = row.game_id AND source.source_kind = 8
     WHERE row.reconciliation_version IS NULL
)
UPDATE "CheatInfo" row
   SET reconciliation_version = ranked.version
  FROM ranked WHERE row.id = ranked.id;
WITH ranked AS MATERIALIZED (
    SELECT row.id, source.dirty_version
             + row_number() OVER (PARTITION BY row.game_id ORDER BY row.id) AS version
      FROM "Participations" row
      JOIN "AntiCheatReconciliationSources" source
        ON source.game_id = row.game_id AND source.source_kind = 9
     WHERE row.reconciliation_version IS NULL
)
UPDATE "Participations" row
   SET reconciliation_version = ranked.version
  FROM ranked WHERE row.id = ranked.id;

ALTER TABLE "IdentityObservations"
    ENABLE TRIGGER tr_identity_observations_append_only;
ALTER TABLE "ContainerAccessEvents"
    ENABLE TRIGGER trg_containeraccess_immutable;
ALTER TABLE "SuspicionEvents"
    ENABLE TRIGGER trg_suspicionevents_immutable;
ALTER TABLE "CheatInfo" ENABLE TRIGGER trg_cheatinfo_immutable;

ALTER TABLE "SuspicionEvaluationOutbox"
    DROP CONSTRAINT IF EXISTS ck_suspicion_outbox_reconciliation_version,
    ADD CONSTRAINT ck_suspicion_outbox_reconciliation_version CHECK (
        (completed_at_utc IS NULL) = (reconciliation_version IS NULL)
    );
ALTER TABLE "IdentityObservations"
    DROP CONSTRAINT IF EXISTS ck_identity_observation_reconciliation_version,
    ADD CONSTRAINT ck_identity_observation_reconciliation_version CHECK (
        (game_id IS NULL) = (reconciliation_version IS NULL)
    );
ALTER TABLE "VpnDnsProviderBuckets"
    ALTER COLUMN reconciliation_version SET NOT NULL;
ALTER TABLE "VpnPeerNetworkObservations"
    ALTER COLUMN reconciliation_version SET NOT NULL;
ALTER TABLE "VpnFlagTransportEvents"
    ALTER COLUMN reconciliation_version SET NOT NULL;
ALTER TABLE "ContainerAccessEvents"
    ALTER COLUMN reconciliation_version SET NOT NULL;
ALTER TABLE "SuspicionEvents"
    ALTER COLUMN reconciliation_version SET NOT NULL;
ALTER TABLE "CheatInfo"
    ALTER COLUMN reconciliation_version SET NOT NULL;
ALTER TABLE "Participations"
    ALTER COLUMN reconciliation_version SET NOT NULL;

-- Close the m0283/m0284 window from the stored versions, not allocation IDs.
SELECT rsctf_mark_anticheat_reconciliation_dirty(game.id, 0, source.max_version)
  FROM "Games" game
  JOIN LATERAL (
      SELECT MAX(reconciliation_version)::bigint AS max_version
        FROM "SuspicionEvaluationOutbox"
       WHERE game_id = game.id AND completed_at_utc IS NOT NULL
  ) source ON source.max_version IS NOT NULL;
SELECT rsctf_mark_anticheat_reconciliation_dirty(game.id, 1, source.max_version)
  FROM "Games" game
  JOIN LATERAL (
      SELECT MAX(reconciliation_version)::bigint AS max_version
        FROM "IdentityObservations" WHERE game_id = game.id
  ) source ON source.max_version IS NOT NULL;
SELECT rsctf_mark_anticheat_reconciliation_dirty(game.id, 3, source.max_version)
  FROM "Games" game JOIN LATERAL (
      SELECT MAX(reconciliation_version)::bigint AS max_version
        FROM "VpnDnsProviderBuckets" WHERE game_id = game.id
  ) source ON source.max_version IS NOT NULL;
SELECT rsctf_mark_anticheat_reconciliation_dirty(game.id, 4, source.max_version)
  FROM "Games" game JOIN LATERAL (
      SELECT MAX(reconciliation_version)::bigint AS max_version
        FROM "VpnPeerNetworkObservations" WHERE game_id = game.id
  ) source ON source.max_version IS NOT NULL;
SELECT rsctf_mark_anticheat_reconciliation_dirty(game.id, 5, source.max_version)
  FROM "Games" game JOIN LATERAL (
      SELECT MAX(reconciliation_version)::bigint AS max_version
        FROM "VpnFlagTransportEvents" WHERE game_id = game.id
  ) source ON source.max_version IS NOT NULL;
SELECT rsctf_mark_anticheat_reconciliation_dirty(game.id, 6, source.max_version)
  FROM "Games" game JOIN LATERAL (
      SELECT MAX(reconciliation_version)::bigint AS max_version
        FROM "ContainerAccessEvents" WHERE game_id = game.id
  ) source ON source.max_version IS NOT NULL;
SELECT rsctf_mark_anticheat_reconciliation_dirty(game.id, 7, source.max_version)
  FROM "Games" game JOIN LATERAL (
      SELECT MAX(reconciliation_version)::bigint AS max_version
        FROM "SuspicionEvents" WHERE game_id = game.id
  ) source ON source.max_version IS NOT NULL;
SELECT rsctf_mark_anticheat_reconciliation_dirty(game.id, 8, source.max_version)
  FROM "Games" game JOIN LATERAL (
      SELECT MAX(reconciliation_version)::bigint AS max_version
        FROM "CheatInfo" WHERE game_id = game.id
  ) source ON source.max_version IS NOT NULL;
SELECT rsctf_mark_anticheat_reconciliation_dirty(game.id, 9, source.max_version)
  FROM "Games" game JOIN LATERAL (
      SELECT MAX(reconciliation_version)::bigint AS max_version
        FROM "Participations" WHERE game_id = game.id
  ) source ON source.max_version IS NOT NULL;

-- A game sealed by the pre-incremental reconciler has already received its
-- authoritative sweep. Stored backfill versions remain queryable, but must not
-- strand that terminal game in a dirty generation it is ineligible to run.
UPDATE "AntiCheatReconciliationSources" source
   SET applied_version = source.dirty_version,
       applied_at_utc = COALESCE(source.applied_at_utc, reconciliation.sealed_at_utc)
  FROM "SuspicionReconciliationState" reconciliation
 WHERE reconciliation.game_id = source.game_id
   AND reconciliation.sealed_at_utc IS NOT NULL;
UPDATE "AntiCheatReconciliationQueue" queue
   SET applied_generation = queue.desired_generation,
       final_requested_at_utc = COALESCE(
           queue.final_requested_at_utc,
           reconciliation.evidence_closed_at_utc,
           reconciliation.sealed_at_utc
       ),
       final_applied_at_utc = COALESCE(
           queue.final_applied_at_utc, reconciliation.sealed_at_utc
       ),
       updated_at_utc = clock_timestamp()
  FROM "SuspicionReconciliationState" reconciliation
 WHERE reconciliation.game_id = queue.game_id
   AND reconciliation.sealed_at_utc IS NOT NULL;

CREATE OR REPLACE FUNCTION rsctf_next_anticheat_reconciliation_version(
    dirty_game_id INTEGER,
    dirty_source_kind SMALLINT
) RETURNS BIGINT LANGUAGE plpgsql AS $$
DECLARE
    stamped_version BIGINT;
    became_dirty BOOLEAN;
    terminal_auto_ack BOOLEAN;
BEGIN
    IF dirty_game_id IS NULL OR dirty_source_kind NOT BETWEEN 0 AND 9 THEN
        RAISE EXCEPTION 'invalid anti-cheat reconciliation source';
    END IF;
    INSERT INTO "AntiCheatReconciliationQueue" (game_id)
    VALUES (dirty_game_id) ON CONFLICT (game_id) DO NOTHING;
    -- Every runtime path takes the shared game row before a source row. This
    -- avoids cross-source inversions when one transaction emits two families.
    SELECT dirty_source_kind IN (6, 9)
           AND (
               queue.final_applied_at_utc IS NOT NULL
               OR EXISTS (
                   SELECT 1 FROM "SuspicionReconciliationState" reconciliation
                    WHERE reconciliation.game_id = dirty_game_id
                      AND reconciliation.sealed_at_utc IS NOT NULL
               )
           )
      INTO terminal_auto_ack
      FROM "AntiCheatReconciliationQueue" queue
     WHERE queue.game_id = dirty_game_id
     FOR UPDATE;
    INSERT INTO "AntiCheatReconciliationSources"
        (game_id, source_kind, dirty_version, applied_version,
         dirtied_at_utc, applied_at_utc)
    VALUES (
        dirty_game_id, dirty_source_kind, 1,
        CASE WHEN terminal_auto_ack THEN 1 ELSE 0 END,
        clock_timestamp(),
        CASE WHEN terminal_auto_ack THEN clock_timestamp() ELSE NULL END
    )
    ON CONFLICT (game_id, source_kind) DO UPDATE
      SET dirty_version = "AntiCheatReconciliationSources".dirty_version + 1,
          applied_version = CASE WHEN terminal_auto_ack
              THEN "AntiCheatReconciliationSources".dirty_version + 1
              ELSE "AntiCheatReconciliationSources".applied_version END,
          dirtied_at_utc = clock_timestamp(),
          applied_at_utc = CASE WHEN terminal_auto_ack
              THEN clock_timestamp()
              ELSE "AntiCheatReconciliationSources".applied_at_utc END
    RETURNING dirty_version,
              NOT terminal_auto_ack
              AND dirty_version = applied_version + 1
         INTO stamped_version, became_dirty;
    IF became_dirty THEN
        UPDATE "AntiCheatReconciliationQueue"
           SET desired_generation = desired_generation + 1,
               available_at_utc = LEAST(available_at_utc, clock_timestamp()),
               updated_at_utc = clock_timestamp()
         WHERE game_id = dirty_game_id;
    END IF;
    RETURN stamped_version;
END
$$;

CREATE OR REPLACE FUNCTION rsctf_stamp_anticheat_insert()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.reconciliation_version IS NOT NULL THEN
        RAISE EXCEPTION 'anti-cheat reconciliation version is database-owned';
    END IF;
    NEW.reconciliation_version := rsctf_next_anticheat_reconciliation_version(
        NEW.game_id, TG_ARGV[0]::smallint
    );
    RETURN NEW;
END
$$;

CREATE OR REPLACE FUNCTION rsctf_stamp_anticheat_outbox_completion()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF NEW.reconciliation_version IS NOT NULL THEN
            RAISE EXCEPTION 'outbox completion version is database-owned';
        END IF;
        IF NEW.completed_at_utc IS NOT NULL THEN
            NEW.reconciliation_version := rsctf_next_anticheat_reconciliation_version(
                NEW.game_id, 0
            );
        END IF;
        RETURN NEW;
    END IF;
    IF OLD.completed_at_utc IS NULL AND NEW.completed_at_utc IS NOT NULL THEN
        IF NEW.reconciliation_version IS NOT NULL THEN
            RAISE EXCEPTION 'outbox completion version is database-owned';
        END IF;
        NEW.reconciliation_version := rsctf_next_anticheat_reconciliation_version(
            NEW.game_id, 0
        );
    ELSIF NEW.reconciliation_version IS DISTINCT FROM OLD.reconciliation_version THEN
        RAISE EXCEPTION 'outbox completion version is immutable';
    END IF;
    RETURN NEW;
END
$$;

CREATE OR REPLACE FUNCTION rsctf_stamp_anticheat_participation()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' AND NEW.game_id IS DISTINCT FROM OLD.game_id THEN
        RAISE EXCEPTION 'participation game identity is immutable';
    ELSIF TG_OP = 'UPDATE'
       AND NEW.reconciliation_version IS DISTINCT FROM OLD.reconciliation_version THEN
        RAISE EXCEPTION 'participation reconciliation version is database-owned';
    ELSIF TG_OP = 'INSERT' AND NEW.reconciliation_version IS NOT NULL THEN
        RAISE EXCEPTION 'participation reconciliation version is database-owned';
    END IF;
    IF TG_OP = 'UPDATE'
       AND NEW.status IS NOT DISTINCT FROM OLD.status
       AND NEW.game_id IS NOT DISTINCT FROM OLD.game_id
       AND NEW.competitive_admitted_at_utc
             IS NOT DISTINCT FROM OLD.competitive_admitted_at_utc THEN
        NEW.reconciliation_version := OLD.reconciliation_version;
        RETURN NEW;
    END IF;
    NEW.reconciliation_version := rsctf_next_anticheat_reconciliation_version(
        NEW.game_id, 9
    );
    RETURN NEW;
END
$$;

CREATE OR REPLACE FUNCTION rsctf_guard_anticheat_reconciliation_version()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.reconciliation_version IS DISTINCT FROM OLD.reconciliation_version THEN
        RAISE EXCEPTION 'anti-cheat reconciliation version is immutable';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER zz_suspicion_outbox_anticheat_stamp
BEFORE INSERT OR UPDATE OF completed_at_utc, reconciliation_version
ON "SuspicionEvaluationOutbox"
FOR EACH ROW EXECUTE FUNCTION rsctf_stamp_anticheat_outbox_completion();
CREATE TRIGGER zz_identity_observation_anticheat_stamp
BEFORE INSERT ON "IdentityObservations"
FOR EACH ROW WHEN (NEW.game_id IS NOT NULL)
EXECUTE FUNCTION rsctf_stamp_anticheat_insert('1');
CREATE TRIGGER zz_vpn_dns_anticheat_stamp
BEFORE INSERT ON "VpnDnsProviderBuckets"
FOR EACH ROW EXECUTE FUNCTION rsctf_stamp_anticheat_insert('3');
CREATE TRIGGER zz_vpn_peer_anticheat_stamp
BEFORE INSERT ON "VpnPeerNetworkObservations"
FOR EACH ROW EXECUTE FUNCTION rsctf_stamp_anticheat_insert('4');
CREATE TRIGGER zz_vpn_flag_anticheat_stamp
BEFORE INSERT ON "VpnFlagTransportEvents"
FOR EACH ROW EXECUTE FUNCTION rsctf_stamp_anticheat_insert('5');
CREATE TRIGGER zz_container_access_anticheat_stamp
BEFORE INSERT ON "ContainerAccessEvents"
FOR EACH ROW EXECUTE FUNCTION rsctf_stamp_anticheat_insert('6');
CREATE TRIGGER zz_suspicion_event_anticheat_stamp
BEFORE INSERT ON "SuspicionEvents"
FOR EACH ROW EXECUTE FUNCTION rsctf_stamp_anticheat_insert('7');
CREATE TRIGGER zz_cheat_info_anticheat_stamp
BEFORE INSERT ON "CheatInfo"
FOR EACH ROW EXECUTE FUNCTION rsctf_stamp_anticheat_insert('8');
-- Run after the shipped competitive-admission trigger so NEW.game_id and the
-- resulting roster state are final before the version is assigned.
CREATE TRIGGER zz_participation_anticheat_stamp
BEFORE INSERT OR UPDATE OF status, game_id, competitive_admitted_at_utc
ON "Participations"
FOR EACH ROW EXECUTE FUNCTION rsctf_stamp_anticheat_participation();

CREATE TRIGGER zz_vpn_dns_reconciliation_version_immutable
BEFORE UPDATE OF reconciliation_version ON "VpnDnsProviderBuckets"
FOR EACH ROW EXECUTE FUNCTION rsctf_guard_anticheat_reconciliation_version();
CREATE TRIGGER zz_vpn_peer_reconciliation_version_immutable
BEFORE UPDATE OF reconciliation_version ON "VpnPeerNetworkObservations"
FOR EACH ROW EXECUTE FUNCTION rsctf_guard_anticheat_reconciliation_version();
CREATE TRIGGER zz_vpn_flag_reconciliation_version_immutable
BEFORE UPDATE OF reconciliation_version ON "VpnFlagTransportEvents"
FOR EACH ROW EXECUTE FUNCTION rsctf_guard_anticheat_reconciliation_version();
CREATE TRIGGER zz_participation_reconciliation_version_immutable
BEFORE UPDATE OF reconciliation_version ON "Participations"
FOR EACH ROW EXECUTE FUNCTION rsctf_guard_anticheat_reconciliation_version();

CREATE UNIQUE INDEX IF NOT EXISTS ix_suspicion_outbox_game_reconciliation_delta
    ON "SuspicionEvaluationOutbox"(game_id, reconciliation_version)
    WHERE reconciliation_version IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_suspicion_outbox_game_incomplete_competitive
    ON "SuspicionEvaluationOutbox"(game_id, observed_at_utc)
    WHERE completed_at_utc IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS ix_identity_observation_game_reconciliation_delta
    ON "IdentityObservations"(game_id, reconciliation_version)
    WHERE game_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS ix_vpn_dns_game_reconciliation_delta
    ON "VpnDnsProviderBuckets"(game_id, reconciliation_version);
CREATE UNIQUE INDEX IF NOT EXISTS ix_vpn_peer_game_reconciliation_delta
    ON "VpnPeerNetworkObservations"(game_id, reconciliation_version);
CREATE UNIQUE INDEX IF NOT EXISTS ix_vpn_flag_game_reconciliation_delta
    ON "VpnFlagTransportEvents"(game_id, reconciliation_version);
CREATE UNIQUE INDEX IF NOT EXISTS ix_container_access_game_reconciliation_delta
    ON "ContainerAccessEvents"(game_id, reconciliation_version);
CREATE UNIQUE INDEX IF NOT EXISTS ix_suspicion_event_game_reconciliation_delta
    ON "SuspicionEvents"(game_id, reconciliation_version);
CREATE UNIQUE INDEX IF NOT EXISTS ix_cheat_info_game_reconciliation_delta
    ON "CheatInfo"(game_id, reconciliation_version);
CREATE UNIQUE INDEX IF NOT EXISTS ix_participation_game_reconciliation_delta
    ON "Participations"(game_id, reconciliation_version);

CREATE INDEX IF NOT EXISTS ix_identity_observation_game_kind_value_delta_context
    ON "IdentityObservations"(
        game_id, kind, value_hash, observed_at_utc DESC, id DESC
    ) INCLUDE (user_id, team_id, participation_id)
    WHERE game_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_identity_observation_game_user_kind_delta_context
    ON "IdentityObservations"(
        game_id, user_id, kind, observed_at_utc DESC, id DESC
    ) INCLUDE (value_hash, broad_network_hash, team_id, participation_id)
    WHERE game_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_identity_observation_game_user_ip_network_context
    ON "IdentityObservations"(
        game_id, user_id, observed_at_utc DESC, id DESC
    ) INCLUDE (broad_network_hash)
    WHERE game_id IS NOT NULL AND kind = 'Ip'
      AND broad_network_hash IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_vpn_peer_game_peer_endpoint_context
    ON "VpnPeerNetworkObservations"(
        game_id, peer_id, endpoint_hash, first_seen_at_utc, id
    ) INCLUDE (participation_id, user_id);
CREATE INDEX IF NOT EXISTS ix_anticheat_findings_game_detector_evidence
    ON "AntiCheatFindings"(game_id, detector_code, evidence_key, id);
CREATE INDEX IF NOT EXISTS ix_cheat_info_game_event_context
    ON "CheatInfo"(game_id, submit_participation_id, evidence_key, id)
    INCLUDE (challenge_id, source_participation_id, observed_at_utc);
CREATE INDEX IF NOT EXISTS ix_cheat_info_game_transport_context
    ON "CheatInfo"(
        game_id, challenge_id, submit_participation_id,
        source_participation_id, observed_at_utc, id
    ) INCLUDE (evidence_key);
CREATE INDEX IF NOT EXISTS ix_vpn_flag_game_cheat_context
    ON "VpnFlagTransportEvents"(
        game_id, challenge_id, receiving_participation_id,
        owning_participation_id, observed_at_utc DESC, id DESC
    );
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Forward-only: removing commit-ordered cursors can strand evidence.
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn every_live_source_is_commit_ordered_and_indexed() {
        for table in [
            "SuspicionEvaluationOutbox",
            "IdentityObservations",
            "VpnDnsProviderBuckets",
            "VpnPeerNetworkObservations",
            "VpnFlagTransportEvents",
            "ContainerAccessEvents",
            "SuspicionEvents",
            "CheatInfo",
            "Participations",
        ] {
            assert!(UP_SQL.contains(table), "missing version edge for {table}");
        }
        assert!(UP_SQL.contains("IN SHARE ROW EXCLUSIVE MODE"));
        assert!(UP_SQL.contains("LOCK TABLE \"Games\" IN EXCLUSIVE MODE"));
        assert!(UP_SQL.contains("LOCK TABLE \"AntiCheatTelemetryUsage\" IN EXCLUSIVE MODE"));
        assert!(
            UP_SQL.find("LOCK TABLE \"Games\" IN EXCLUSIVE MODE")
                < UP_SQL.find("LOCK TABLE \"SuspicionEvents\"")
        );
        assert!(
            UP_SQL.find("LOCK TABLE \"SuspicionEvents\"")
                < UP_SQL.find("\"Participations\", \"IdentityObservations\"")
        );
        assert!(UP_SQL.contains("dirty_version + 1"));
        assert!(UP_SQL.contains("dirty_version = applied_version + 1"));
        assert!(UP_SQL.contains("WHERE game_id = dirty_game_id FOR UPDATE"));
        assert!(UP_SQL.contains("rsctf_next_anticheat_reconciliation_version"));
        assert!(!UP_SQL.contains("dirty_anticheat_from_exemption"));
        for source in [0, 1, 3, 4, 5, 6, 7, 8, 9] {
            assert!(
                UP_SQL.contains(&format!(
                    "rsctf_mark_anticheat_reconciliation_dirty(game.id, {source},"
                )),
                "missing stored-version catch-up for source {source}"
            );
        }
        assert_eq!(UP_SQL.matches("game_reconciliation_delta").count(), 9);
        assert!(UP_SQL.contains("ix_suspicion_outbox_game_incomplete_competitive"));
        assert!(UP_SQL.contains("ix_anticheat_findings_game_detector_evidence"));
        assert!(UP_SQL.contains("ix_cheat_info_game_event_context"));
        assert!(UP_SQL.contains("ix_cheat_info_game_transport_context"));
        assert!(UP_SQL.contains("ix_vpn_flag_game_cheat_context"));
        assert!(
            UP_SQL.contains("BEFORE INSERT OR UPDATE OF completed_at_utc, reconciliation_version")
        );
        assert!(UP_SQL.contains("IF TG_OP = 'INSERT' THEN"));
        assert!(UP_SQL.contains("participation game identity is immutable"));
        assert!(UP_SQL.contains("NEW.status IS NOT DISTINCT FROM OLD.status"));
        assert!(UP_SQL.contains(
            "NEW.competitive_admitted_at_utc\n             IS NOT DISTINCT FROM OLD.competitive_admitted_at_utc"
        ));
    }

    #[tokio::test]
    #[ignore = "requires migrated disposable PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn postgres_stamps_follow_commit_order_without_cross_source_deadlock() {
        use chrono::{DateTime, Utc};
        use sqlx::postgres::PgPoolOptions;
        use tokio::sync::oneshot;
        use uuid::Uuid;

        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(&database_url)
            .await
            .unwrap();
        let game_id: i32 = sqlx::query_scalar(
            r#"SELECT game_id FROM "AntiCheatReconciliationQueue"
                ORDER BY game_id LIMIT 1"#,
        )
        .fetch_one(&pool)
        .await
        .expect("the disposable database needs one game");
        let original_queue: (i64, i64, DateTime<Utc>, DateTime<Utc>) = sqlx::query_as(
            r#"SELECT desired_generation, applied_generation,
                       available_at_utc, updated_at_utc
                  FROM "AntiCheatReconciliationQueue" WHERE game_id = $1"#,
        )
        .bind(game_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        let original_sources: Vec<(i16, i64, i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>)> =
            sqlx::query_as(
                r#"SELECT source_kind, dirty_version, applied_version,
                       dirtied_at_utc, applied_at_utc
                  FROM "AntiCheatReconciliationSources"
                 WHERE game_id = $1 AND source_kind IN (0, 1, 3)
                 ORDER BY source_kind"#,
            )
            .bind(game_id)
            .fetch_all(&pool)
            .await
            .unwrap();

        let probe = format!("anticheat_version_probe_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(
            r#"CREATE TABLE "{probe}" (
                   evidence_id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
                   reconciliation_version BIGINT NULL
               )"#
        ))
        .execute(&pool)
        .await
        .unwrap();

        // An outbox job can allocate its identity long before completion. A
        // higher identity completes and is applied first; the lower identity's
        // later completion must still receive a discoverable later version.
        let mut lower_id_late = pool.begin().await.unwrap();
        let lower_id: i64 = sqlx::query_scalar(&format!(
            r#"INSERT INTO "{probe}" DEFAULT VALUES RETURNING evidence_id"#
        ))
        .fetch_one(&mut *lower_id_late)
        .await
        .unwrap();
        let (higher_id, higher_id_version): (i64, i64) = sqlx::query_as(&format!(
            r#"INSERT INTO "{probe}" (reconciliation_version)
               VALUES (rsctf_next_anticheat_reconciliation_version($1, 0))
               RETURNING evidence_id, reconciliation_version"#
        ))
        .bind(game_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(lower_id < higher_id);

        let mut checkpoint = pool.begin().await.unwrap();
        sqlx::query(
            r#"SELECT 1 FROM "AntiCheatReconciliationQueue"
                WHERE game_id = $1 FOR UPDATE"#,
        )
        .bind(game_id)
        .execute(&mut *checkpoint)
        .await
        .unwrap();
        let applied = sqlx::query(
            r#"UPDATE "AntiCheatReconciliationSources"
                  SET applied_version = $2
                WHERE game_id = $1 AND source_kind = 0
                  AND dirty_version = $2"#,
        )
        .bind(game_id)
        .bind(higher_id_version)
        .execute(&mut *checkpoint)
        .await
        .unwrap();
        assert_eq!(applied.rows_affected(), 1);
        checkpoint.commit().await.unwrap();

        let lower_id_version: i64 = sqlx::query_scalar(&format!(
            r#"UPDATE "{probe}"
                  SET reconciliation_version =
                        rsctf_next_anticheat_reconciliation_version($1, 0)
                WHERE evidence_id = $2
            RETURNING reconciliation_version"#
        ))
        .bind(game_id)
        .bind(lower_id)
        .fetch_one(&mut *lower_id_late)
        .await
        .unwrap();
        lower_id_late.commit().await.unwrap();
        assert!(lower_id_version > higher_id_version);
        let versions: (i64, i64) = sqlx::query_as(
            r#"SELECT applied_version, dirty_version
                 FROM "AntiCheatReconciliationSources"
                WHERE game_id = $1 AND source_kind = 0"#,
        )
        .bind(game_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(versions, (higher_id_version, lower_id_version));
        let pending_ids: Vec<i64> = sqlx::query_scalar(&format!(
            r#"SELECT evidence_id FROM "{probe}"
                WHERE reconciliation_version > $1
                  AND reconciliation_version <= $2
                ORDER BY reconciliation_version"#
        ))
        .bind(higher_id_version)
        .bind(lower_id_version)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(pending_ids, vec![lower_id]);

        // UPDATE OF is syntactic in PostgreSQL. Calling the participation
        // stamper through a probe proves an equal-value update neither assigns
        // a new source-9 version nor advances the game generation.
        let mut no_op = pool.begin().await.unwrap();
        sqlx::raw_sql(
            r#"CREATE TEMP TABLE anticheat_participation_noop_probe (
                   game_id INTEGER NOT NULL,
                   status SMALLINT NOT NULL,
                   competitive_admitted_at_utc TIMESTAMPTZ NULL,
                   reconciliation_version BIGINT NOT NULL
               ) ON COMMIT DROP;
               CREATE TRIGGER zz_probe_participation_stamp
               BEFORE INSERT OR UPDATE OF status, game_id,
                                      competitive_admitted_at_utc
               ON anticheat_participation_noop_probe
               FOR EACH ROW EXECUTE FUNCTION
                   rsctf_stamp_anticheat_participation();"#,
        )
        .execute(&mut *no_op)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO anticheat_participation_noop_probe
               VALUES ($1, 1, NULL, NULL)"#,
        )
        .bind(game_id)
        .execute(&mut *no_op)
        .await
        .unwrap();
        let before_no_op: (i64, i64) = sqlx::query_as(
            r#"SELECT source.dirty_version, queue.desired_generation
                 FROM "AntiCheatReconciliationSources" source
                 JOIN "AntiCheatReconciliationQueue" queue
                   ON queue.game_id = source.game_id
                WHERE source.game_id = $1 AND source.source_kind = 9"#,
        )
        .bind(game_id)
        .fetch_one(&mut *no_op)
        .await
        .unwrap();
        sqlx::query(
            r#"UPDATE anticheat_participation_noop_probe
                  SET status = status, game_id = game_id,
                      competitive_admitted_at_utc = competitive_admitted_at_utc"#,
        )
        .execute(&mut *no_op)
        .await
        .unwrap();
        let after_no_op: (i64, i64, i64) = sqlx::query_as(
            r#"SELECT source.dirty_version, queue.desired_generation,
                      probe.reconciliation_version
                 FROM "AntiCheatReconciliationSources" source
                 JOIN "AntiCheatReconciliationQueue" queue
                   ON queue.game_id = source.game_id
                 CROSS JOIN anticheat_participation_noop_probe probe
                WHERE source.game_id = $1 AND source.source_kind = 9"#,
        )
        .bind(game_id)
        .fetch_one(&mut *no_op)
        .await
        .unwrap();
        assert_eq!((after_no_op.0, after_no_op.1), before_no_op);
        assert!(after_no_op.2 > 0);
        let stable_version = after_no_op.2;
        sqlx::query(
            r#"UPDATE anticheat_participation_noop_probe
                  SET status = status, game_id = game_id,
                      competitive_admitted_at_utc = competitive_admitted_at_utc"#,
        )
        .execute(&mut *no_op)
        .await
        .unwrap();
        let replay_version: i64 = sqlx::query_scalar(
            "SELECT reconciliation_version FROM anticheat_participation_noop_probe",
        )
        .fetch_one(&mut *no_op)
        .await
        .unwrap();
        assert_eq!(replay_version, stable_version);
        no_op.rollback().await.unwrap();

        // Hold the common queue row through source 1, then make a second
        // transaction request source 3 before the first requests source 3.
        // A source-first implementation deadlocks in this exact ordering.
        let mut first = pool.begin().await.unwrap();
        sqlx::query("SET LOCAL lock_timeout = '4s'")
            .execute(&mut *first)
            .await
            .unwrap();
        sqlx::query_scalar::<_, i64>("SELECT rsctf_next_anticheat_reconciliation_version($1, 1)")
            .bind(game_id)
            .fetch_one(&mut *first)
            .await
            .unwrap();
        let first_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *first)
            .await
            .unwrap();
        let (started_tx, started_rx) = oneshot::channel();
        let second_pool = pool.clone();
        let second = tokio::spawn(async move {
            let mut transaction = second_pool.begin().await.unwrap();
            sqlx::query("SET LOCAL lock_timeout = '4s'")
                .execute(&mut *transaction)
                .await
                .unwrap();
            let second_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(&mut *transaction)
                .await
                .unwrap();
            started_tx.send(second_pid).unwrap();
            sqlx::query_scalar::<_, i64>(
                "SELECT rsctf_next_anticheat_reconciliation_version($1, 3)",
            )
            .bind(game_id)
            .fetch_one(&mut *transaction)
            .await
            .unwrap();
            sqlx::query_scalar::<_, i64>(
                "SELECT rsctf_next_anticheat_reconciliation_version($1, 1)",
            )
            .bind(game_id)
            .fetch_one(&mut *transaction)
            .await
            .unwrap();
            transaction.commit().await.unwrap();
        });
        let second_pid = started_rx.await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let queue_blocked_by_first: bool =
                    sqlx::query_scalar("SELECT $1 = ANY(pg_blocking_pids($2))")
                        .bind(first_pid)
                        .bind(second_pid)
                        .fetch_one(&pool)
                        .await
                        .unwrap();
                if queue_blocked_by_first {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("second source transaction never waited on the common queue row");
        sqlx::query_scalar::<_, i64>("SELECT rsctf_next_anticheat_reconciliation_version($1, 3)")
            .bind(game_id)
            .fetch_one(&mut *first)
            .await
            .unwrap();
        first.commit().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), second)
            .await
            .expect("reversed source order must not deadlock")
            .unwrap();

        let mut cleanup = pool.begin().await.unwrap();
        sqlx::query(
            r#"SELECT 1 FROM "AntiCheatReconciliationQueue"
                WHERE game_id = $1 FOR UPDATE"#,
        )
        .bind(game_id)
        .execute(&mut *cleanup)
        .await
        .unwrap();
        for (kind, dirty, applied, dirtied_at, applied_at) in original_sources {
            sqlx::query(
                r#"UPDATE "AntiCheatReconciliationSources"
                      SET dirty_version = $3, applied_version = $4,
                          dirtied_at_utc = $5, applied_at_utc = $6
                    WHERE game_id = $1 AND source_kind = $2"#,
            )
            .bind(game_id)
            .bind(kind)
            .bind(dirty)
            .bind(applied)
            .bind(dirtied_at)
            .bind(applied_at)
            .execute(&mut *cleanup)
            .await
            .unwrap();
        }
        sqlx::query(
            r#"UPDATE "AntiCheatReconciliationQueue"
                  SET desired_generation = $2, applied_generation = $3,
                      available_at_utc = $4, updated_at_utc = $5
                WHERE game_id = $1"#,
        )
        .bind(game_id)
        .bind(original_queue.0)
        .bind(original_queue.1)
        .bind(original_queue.2)
        .bind(original_queue.3)
        .execute(&mut *cleanup)
        .await
        .unwrap();
        cleanup.commit().await.unwrap();
        sqlx::query(&format!(r#"DROP TABLE "{probe}""#))
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }
}

#[cfg(test)]
#[path = "m0284_anticheat_dirty_outboxes_terminal_tests.rs"]
mod terminal_tests;
