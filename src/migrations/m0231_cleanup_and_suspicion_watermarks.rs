//! Durable fences for bounded image cleanup and incremental anti-cheat sweeps.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "BuildImageOwnerships"
  ADD COLUMN IF NOT EXISTS cleanup_claim_id UUID NULL,
  ADD COLUMN IF NOT EXISTS cleanup_claim_expires_at_utc TIMESTAMPTZ NULL;
ALTER TABLE "ImageCleanupLeases"
  ADD COLUMN IF NOT EXISTS candidate_cursor_ref TEXT NULL;

DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_constraint
     WHERE conname = 'ck_build_image_ownership_cleanup_claim'
       AND conrelid = '"BuildImageOwnerships"'::regclass
  ) THEN
    ALTER TABLE "BuildImageOwnerships"
      ADD CONSTRAINT ck_build_image_ownership_cleanup_claim CHECK (
        (cleanup_claim_id IS NULL) = (cleanup_claim_expires_at_utc IS NULL)
      );
  END IF;
END
$$;

CREATE INDEX IF NOT EXISTS ix_build_image_ownership_cleanup_claim
  ON "BuildImageOwnerships" (installation_scope, cleanup_claim_expires_at_utc)
  WHERE cleanup_claim_id IS NOT NULL;

-- A final cleanup recheck must not rescan the complete challenge catalog. The
-- managed-image canonicalizer accepts a small set of Docker Hub aliases; these
-- expression indexes make exact alias probes bounded and indexable.
CREATE INDEX IF NOT EXISTS ix_gamechallenges_container_image_trimmed
  ON "GameChallenges" ((BTRIM(container_image)))
  WHERE container_image IS NOT NULL AND BTRIM(container_image) <> '';
CREATE INDEX IF NOT EXISTS ix_gamechallenges_checker_image_trimmed
  ON "GameChallenges" ((BTRIM(ad_checker_image)))
  WHERE ad_checker_image IS NOT NULL AND BTRIM(ad_checker_image) <> '';
CREATE INDEX IF NOT EXISTS ix_gamechallenges_generator_image
  ON "GameChallenges" (variant_generator_image)
  WHERE variant_generator_image IS NOT NULL;

CREATE SEQUENCE IF NOT EXISTS rsctf_suspicion_source_reconcile_seq;

ALTER TABLE "VpnDnsProviderBuckets"
  ADD COLUMN IF NOT EXISTS reconcile_revision BIGINT
    DEFAULT nextval('rsctf_suspicion_source_reconcile_seq');
ALTER TABLE "VpnPeerNetworkObservations"
  ADD COLUMN IF NOT EXISTS reconcile_revision BIGINT
    DEFAULT nextval('rsctf_suspicion_source_reconcile_seq');

-- Adding the volatile sequence default assigns every existing row a unique
-- revision without firing its row-level dirty trigger. Move the shared
-- sequence beyond both catalogs; the id fallback also repairs a partially
-- applied pre-release schema safely.
SELECT setval(
  'rsctf_suspicion_source_reconcile_seq',
  GREATEST(
    1,
    (SELECT last_value FROM rsctf_suspicion_source_reconcile_seq),
    COALESCE((SELECT MAX(COALESCE(reconcile_revision, id))
                FROM "VpnDnsProviderBuckets"), 0),
    COALESCE((SELECT MAX(COALESCE(reconcile_revision, id))
                FROM "VpnPeerNetworkObservations"), 0)
  ),
  TRUE
);
UPDATE "VpnDnsProviderBuckets"
   SET reconcile_revision = nextval('rsctf_suspicion_source_reconcile_seq')
 WHERE reconcile_revision IS NULL;
UPDATE "VpnPeerNetworkObservations"
   SET reconcile_revision = nextval('rsctf_suspicion_source_reconcile_seq')
 WHERE reconcile_revision IS NULL;
ALTER TABLE "VpnDnsProviderBuckets"
  ALTER COLUMN reconcile_revision SET DEFAULT nextval('rsctf_suspicion_source_reconcile_seq'),
  ALTER COLUMN reconcile_revision SET NOT NULL;
ALTER TABLE "VpnPeerNetworkObservations"
  ALTER COLUMN reconcile_revision SET DEFAULT nextval('rsctf_suspicion_source_reconcile_seq'),
  ALTER COLUMN reconcile_revision SET NOT NULL;

CREATE OR REPLACE FUNCTION rsctf_touch_suspicion_source_revision()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP = 'UPDATE' OR NEW.reconcile_revision IS NULL THEN
    NEW.reconcile_revision := nextval('rsctf_suspicion_source_reconcile_seq');
  END IF;
  RETURN NEW;
END
$$;

DROP TRIGGER IF EXISTS tr_vpndnsproviderbuckets_reconcile_revision
  ON "VpnDnsProviderBuckets";
CREATE TRIGGER tr_vpndnsproviderbuckets_reconcile_revision
BEFORE INSERT OR UPDATE ON "VpnDnsProviderBuckets"
FOR EACH ROW EXECUTE FUNCTION rsctf_touch_suspicion_source_revision();

DROP TRIGGER IF EXISTS tr_vpnpeernetworkobservations_reconcile_revision
  ON "VpnPeerNetworkObservations";
CREATE TRIGGER tr_vpnpeernetworkobservations_reconcile_revision
BEFORE INSERT OR UPDATE ON "VpnPeerNetworkObservations"
FOR EACH ROW EXECUTE FUNCTION rsctf_touch_suspicion_source_revision();

CREATE INDEX IF NOT EXISTS ix_vpn_dns_provider_reconcile
  ON "VpnDnsProviderBuckets" (game_id, reconcile_revision);
CREATE INDEX IF NOT EXISTS ix_vpn_peer_network_reconcile
  ON "VpnPeerNetworkObservations" (game_id, reconcile_revision);
CREATE INDEX IF NOT EXISTS ix_identity_observations_reconcile
  ON "IdentityObservations" (game_id, id)
  WHERE game_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_vpn_flag_transport_reconcile
  ON "VpnFlagTransportEvents" (game_id, id);
CREATE INDEX IF NOT EXISTS ix_cheat_info_reconcile
  ON "CheatInfo" (game_id, id);
CREATE INDEX IF NOT EXISTS ix_cheat_info_flag_transport_match
  ON "CheatInfo" (
    game_id, challenge_id, submit_participation_id,
    source_participation_id, observed_at_utc, id
  );
CREATE INDEX IF NOT EXISTS ix_vpn_flag_transport_cheat_match
  ON "VpnFlagTransportEvents" (
    game_id, challenge_id, receiving_participation_id,
    owning_participation_id, observed_at_utc, id
  );

CREATE TABLE IF NOT EXISTS "SuspicionReconciliationWatermarks" (
  game_id INTEGER PRIMARY KEY REFERENCES "Games"(id) ON DELETE CASCADE,
  identity_observation_id BIGINT NOT NULL DEFAULT 0 CHECK (identity_observation_id >= 0),
  dns_revision BIGINT NOT NULL DEFAULT 0 CHECK (dns_revision >= 0),
  network_revision BIGINT NOT NULL DEFAULT 0 CHECK (network_revision >= 0),
  flag_transport_id BIGINT NOT NULL DEFAULT 0 CHECK (flag_transport_id >= 0),
  cheat_info_id BIGINT NOT NULL DEFAULT 0 CHECK (cheat_info_id >= 0),
  updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

-- A newly canonical stolen-flag record can corroborate an older VPN transport
-- finding. Mark only that detector family dirty and let its own id watermark
-- drive the bounded relationship pass.
DROP TRIGGER IF EXISTS tr_cheatinfo_suspicion_dirty ON "CheatInfo";
CREATE TRIGGER tr_cheatinfo_suspicion_dirty
AFTER INSERT ON "CheatInfo"
FOR EACH ROW EXECUTE FUNCTION rsctf_mark_suspicion_game_dirty();
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Forward-only: cleanup claims and detector watermarks are live
        // coordination state and must not be discarded by a rolling rollback.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_claims_and_source_watermarks_are_durable_and_indexed() {
        assert!(UP_SQL.contains("ck_build_image_ownership_cleanup_claim"));
        assert!(UP_SQL.contains("candidate_cursor_ref TEXT NULL"));
        assert!(UP_SQL.contains("SuspicionReconciliationWatermarks"));
        assert!(UP_SQL.contains("identity_observation_id BIGINT NOT NULL DEFAULT 0"));
        assert!(UP_SQL.contains("reconcile_revision"));
        assert!(UP_SQL.contains("ix_vpn_dns_provider_reconcile"));
        assert!(UP_SQL.contains("ix_cheat_info_flag_transport_match"));
        assert!(UP_SQL.contains("tr_cheatinfo_suspicion_dirty"));
    }
}
