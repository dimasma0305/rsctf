//! Durable dirty generations and short leases for game-level anti-cheat work.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP_SQL: &str = r#"
ALTER TABLE "SuspicionReconciliationState"
  ADD COLUMN IF NOT EXISTS dirty_generation BIGINT NOT NULL DEFAULT 1,
  ADD COLUMN IF NOT EXISTS completed_generation BIGINT NOT NULL DEFAULT 0,
  ADD COLUMN IF NOT EXISTS dirty_mask BIGINT NOT NULL DEFAULT 63,
  ADD COLUMN IF NOT EXISTS lease_token UUID NULL,
  ADD COLUMN IF NOT EXISTS lease_expires_at_utc TIMESTAMPTZ NULL;

CREATE INDEX IF NOT EXISTS ix_suspicion_reconciliation_dirty
  ON "SuspicionReconciliationState"(game_id, dirty_generation, completed_generation)
  WHERE sealed_at_utc IS NULL AND dirty_generation > completed_generation
    AND dirty_mask <> 0;

CREATE TABLE IF NOT EXISTS "SuspicionReconciliationOperations" (
  operation_id UUID PRIMARY KEY,
  game_id INTEGER NOT NULL REFERENCES "Games"(id) ON DELETE CASCADE,
  requested_by UUID NOT NULL REFERENCES "AspNetUsers"(id) ON DELETE RESTRICT,
  generation BIGINT NOT NULL CHECK (generation > 0),
  status SMALLINT NOT NULL DEFAULT 0 CHECK (status BETWEEN 0 AND 2),
  inserted_count INTEGER NULL CHECK (inserted_count IS NULL OR inserted_count >= 0),
  requested_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
  completed_at_utc TIMESTAMPTZ NULL
);
CREATE INDEX IF NOT EXISTS ix_suspicion_reconcile_operations_game
  ON "SuspicionReconciliationOperations"(game_id, generation, status);

CREATE OR REPLACE FUNCTION rsctf_mark_suspicion_game_dirty()
RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE target_game INTEGER;
DECLARE target_mask BIGINT;
BEGIN
  target_game := NEW.game_id;
  target_mask := CASE TG_TABLE_NAME
    WHEN 'SuspicionEvaluationOutbox' THEN 47
    WHEN 'IdentityObservations' THEN 36
    WHEN 'ContainerAccessEvents' THEN 44
    WHEN 'HoneypotHits' THEN 16
    WHEN 'Participations' THEN 38
    ELSE 32
  END;
  IF target_game IS NOT NULL THEN
    INSERT INTO "SuspicionReconciliationState"
        (game_id, attempts, dirty_generation, dirty_mask)
    VALUES (target_game, 0, 1, target_mask)
    ON CONFLICT (game_id) DO UPDATE SET
      dirty_generation = "SuspicionReconciliationState".dirty_generation + 1,
      dirty_mask = "SuspicionReconciliationState".dirty_mask | target_mask;
  END IF;
  RETURN NEW;
END
$$;

DO $$
DECLARE relation_name TEXT;
DECLARE trigger_name TEXT;
DECLARE event_clause TEXT;
BEGIN
  FOREACH relation_name IN ARRAY ARRAY[
    'SuspicionEvaluationOutbox', 'IdentityObservations',
    'VpnFlowTelemetryBuckets', 'VpnDnsProviderBuckets',
    'VpnPeerNetworkObservations', 'VpnFlagTransportEvents',
    'ContainerAccessEvents', 'HoneypotHits', 'Participations'
  ] LOOP
    trigger_name := 'tr_' || lower(relation_name) || '_suspicion_dirty';
    event_clause := CASE
      WHEN relation_name = 'Participations'
      THEN 'INSERT OR UPDATE OF status, division_id'
      WHEN relation_name IN ('VpnFlowTelemetryBuckets', 'VpnDnsProviderBuckets',
                             'VpnPeerNetworkObservations')
      THEN 'INSERT OR UPDATE'
      ELSE 'INSERT'
    END;
    IF to_regclass('"' || relation_name || '"') IS NOT NULL
       AND NOT EXISTS (SELECT 1 FROM pg_trigger WHERE tgname = trigger_name) THEN
      EXECUTE format(
        'CREATE TRIGGER %I AFTER %s ON %I FOR EACH ROW EXECUTE FUNCTION rsctf_mark_suspicion_game_dirty()',
        trigger_name, event_clause, relation_name
      );
    END IF;
  END LOOP;
END
$$;
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
    use super::*;

    #[test]
    fn dirty_work_is_indexed_and_claimed_by_generation() {
        assert!(UP_SQL.contains("dirty_generation > completed_generation"));
        assert!(UP_SQL
            .contains("dirty_mask = \"SuspicionReconciliationState\".dirty_mask | target_mask"));
        assert!(UP_SQL.contains("SuspicionReconciliationOperations"));
        assert!(UP_SQL.contains("rsctf_mark_suspicion_game_dirty"));
    }
}
