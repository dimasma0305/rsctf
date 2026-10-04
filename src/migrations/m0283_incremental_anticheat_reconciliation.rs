//! Durable generations and short leases for incremental anti-cheat reconciliation.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "AntiCheatReconciliationQueue" (
    game_id INTEGER PRIMARY KEY REFERENCES "Games"(id) ON DELETE CASCADE,
    desired_generation BIGINT NOT NULL DEFAULT 0,
    applied_generation BIGINT NOT NULL DEFAULT 0,
    final_requested_at_utc TIMESTAMPTZ NULL,
    final_applied_at_utc TIMESTAMPTZ NULL,
    available_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    lease_token UUID NULL,
    lease_expires_at_utc TIMESTAMPTZ NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_started_at_utc TIMESTAMPTZ NULL,
    last_completed_at_utc TIMESTAMPTZ NULL,
    last_error TEXT NULL,
    updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT ck_anticheat_reconcile_generations CHECK (
        desired_generation >= 0
        AND applied_generation >= 0
        AND applied_generation <= desired_generation
    ),
    CONSTRAINT ck_anticheat_reconcile_lease CHECK (
        (lease_token IS NULL) = (lease_expires_at_utc IS NULL)
    ),
    CONSTRAINT ck_anticheat_reconcile_attempts CHECK (attempts >= 0),
    CONSTRAINT ck_anticheat_reconcile_final CHECK (
        final_applied_at_utc IS NULL OR final_requested_at_utc IS NOT NULL
    ),
    CONSTRAINT ck_anticheat_reconcile_error_bound CHECK (
        last_error IS NULL OR octet_length(last_error) <= 4000
    )
);

CREATE TABLE IF NOT EXISTS "AntiCheatReconciliationSources" (
    game_id INTEGER NOT NULL REFERENCES "Games"(id) ON DELETE CASCADE,
    source_kind SMALLINT NOT NULL CHECK (source_kind BETWEEN 0 AND 9),
    dirty_version BIGINT NOT NULL DEFAULT 0 CHECK (dirty_version >= 0),
    applied_version BIGINT NOT NULL DEFAULT 0 CHECK (applied_version >= 0),
    dirtied_at_utc TIMESTAMPTZ NULL,
    applied_at_utc TIMESTAMPTZ NULL,
    PRIMARY KEY (game_id, source_kind),
    CONSTRAINT ck_anticheat_source_versions CHECK (
        applied_version <= dirty_version
    )
);

CREATE INDEX IF NOT EXISTS ix_anticheat_reconcile_dirty
    ON "AntiCheatReconciliationQueue"(available_at_utc, updated_at_utc, game_id)
    WHERE desired_generation > applied_generation;
CREATE INDEX IF NOT EXISTS ix_games_anticheat_final_due
    ON "Games"(end_time_utc, id)
    WHERE deletion_pending = FALSE;
CREATE INDEX IF NOT EXISTS ix_anticheat_reconcile_lease_recovery
    ON "AntiCheatReconciliationQueue"(lease_expires_at_utc, game_id)
    WHERE lease_token IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_anticheat_reconcile_sources_dirty
    ON "AntiCheatReconciliationSources"(game_id, source_kind, dirty_version)
    WHERE dirty_version > applied_version;

CREATE INDEX IF NOT EXISTS ix_control_plane_jobs_security_generation_receipt
    ON "ControlPlaneJobs"(
        game_id, scope_key,
        ((result->>'reconciliationGeneration')::bigint),
        finished_at_utc DESC, id DESC
    )
    WHERE status = 2 AND kind = 'SecurityDerivation';

CREATE OR REPLACE FUNCTION rsctf_seed_anticheat_reconciliation()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO "AntiCheatReconciliationQueue" (game_id)
    VALUES (NEW.id) ON CONFLICT (game_id) DO NOTHING;
    INSERT INTO "AntiCheatReconciliationSources" (game_id, source_kind)
    SELECT NEW.id, source_kind FROM generate_series(0, 9) source_kind
    ON CONFLICT (game_id, source_kind) DO NOTHING;
    RETURN NEW;
END
$$;

DROP TRIGGER IF EXISTS tr_games_seed_anticheat_reconciliation ON "Games";
CREATE TRIGGER tr_games_seed_anticheat_reconciliation
AFTER INSERT ON "Games"
FOR EACH ROW EXECUTE FUNCTION rsctf_seed_anticheat_reconciliation();

-- Existing games receive one catch-up generation. The source-specific maxima
-- let the first pass use the same bounded cursors as normal steady-state work.
INSERT INTO "AntiCheatReconciliationQueue"
    (game_id, desired_generation, applied_generation)
SELECT game.id, 1, 0 FROM "Games" game
ON CONFLICT (game_id) DO NOTHING;

INSERT INTO "AntiCheatReconciliationSources"
    (game_id, source_kind, dirty_version, applied_version, dirtied_at_utc)
SELECT game.id, source.source_kind,
       CASE WHEN source.source_kind = 2 THEN 0
            ELSE GREATEST(source.dirty_version, 1) END,
       0,
       CASE WHEN source.source_kind = 2 THEN NULL ELSE clock_timestamp() END
  FROM "Games" game
  CROSS JOIN LATERAL (
      SELECT 0::smallint, COALESCE((
          SELECT MAX(job.id) FROM "SuspicionEvaluationOutbox" job
           WHERE job.game_id = game.id AND job.completed_at_utc IS NOT NULL
      ), 0)::bigint
      UNION ALL SELECT 1, COALESCE((
          SELECT MAX(observation.id) FROM "IdentityObservations" observation
           WHERE observation.game_id = game.id
      ), 0)::bigint
      -- Exemptions are observation-time intervals, not retroactive mutations.
      -- Later observations are already carried by the identity source.
      UNION ALL SELECT 2, 0::bigint
      UNION ALL SELECT 3, COALESCE((
          SELECT MAX(bucket.id) FROM "VpnDnsProviderBuckets" bucket
           WHERE bucket.game_id = game.id
      ), 0)::bigint
      UNION ALL SELECT 4, COALESCE((
          SELECT MAX(observation.id) FROM "VpnPeerNetworkObservations" observation
           WHERE observation.game_id = game.id
      ), 0)::bigint
      UNION ALL SELECT 5, COALESCE((
          SELECT MAX(event.id) FROM "VpnFlagTransportEvents" event
           WHERE event.game_id = game.id
      ), 0)::bigint
      UNION ALL SELECT 6, COALESCE((
          SELECT MAX(access.id) FROM "ContainerAccessEvents" access
           WHERE access.game_id = game.id
      ), 0)::bigint
      UNION ALL SELECT 7, COALESCE((
          SELECT MAX(event.id) FROM "SuspicionEvents" event
           WHERE event.game_id = game.id
      ), 0)::bigint
      UNION ALL SELECT 8, COALESCE((
          SELECT MAX(cheat.id) FROM "CheatInfo" cheat
           WHERE cheat.game_id = game.id
      ), 0)::bigint
      UNION ALL SELECT 9, COALESCE((
          SELECT MAX(participation.id) FROM "Participations" participation
           WHERE participation.game_id = game.id
      ), 0)::bigint
  ) AS source(source_kind, dirty_version)
ON CONFLICT (game_id, source_kind) DO UPDATE
  SET dirty_version = GREATEST(
          "AntiCheatReconciliationSources".dirty_version,
          EXCLUDED.dirty_version
      ),
      dirtied_at_utc = CASE
          WHEN "AntiCheatReconciliationSources".dirty_version < EXCLUDED.dirty_version
          THEN EXCLUDED.dirtied_at_utc
          ELSE "AntiCheatReconciliationSources".dirtied_at_utc
      END;

UPDATE "AntiCheatReconciliationQueue" queue
   SET desired_generation = GREATEST(
           queue.desired_generation, queue.applied_generation + 1
       ),
       updated_at_utc = clock_timestamp()
 WHERE EXISTS (
     SELECT 1 FROM "AntiCheatReconciliationSources" source
      WHERE source.game_id = queue.game_id
        AND source.dirty_version > source.applied_version
 );
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Forward-only: these durable cursors may be ahead of detector output.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn queue_is_bounded_leased_and_source_versioned() {
        assert!(UP_SQL.contains("AntiCheatReconciliationQueue"));
        assert!(UP_SQL.contains("AntiCheatReconciliationSources"));
        assert!(UP_SQL.contains("dirty_version > applied_version"));
        assert!(UP_SQL.contains("lease_token IS NULL") && UP_SQL.contains("lease_expires_at_utc"));
        assert!(UP_SQL.contains("source_kind BETWEEN 0 AND 9"));
        assert!(UP_SQL.contains("ON CONFLICT (game_id, source_kind) DO UPDATE"));
        assert!(UP_SQL.contains("ix_anticheat_reconcile_dirty"));
        assert!(UP_SQL.contains("WHERE desired_generation > applied_generation"));
        assert!(UP_SQL.contains("ix_games_anticheat_final_due"));
        assert!(UP_SQL.contains("WHERE deletion_pending = FALSE"));
        assert!(UP_SQL.contains("ix_control_plane_jobs_security_generation_receipt"));
        assert!(UP_SQL.contains("(result->>'reconciliationGeneration')::bigint"));
        assert!(UP_SQL.contains("WHERE status = 2 AND kind = 'SecurityDerivation'"));
    }
}
