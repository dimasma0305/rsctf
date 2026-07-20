//! Database migrations. The initial migration is derived directly from the
//! `rsctf-entity` models via `Schema::create_table_from_entity`, so the DDL
//! can never drift from the entity definitions.

use std::collections::HashSet;

pub use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::DatabaseConnection;

mod m0001_init;
mod m0002_extra;
mod m0003_managers;
mod m0004_repo;
mod m0005_anticheat;
mod m0006_builds;
mod m0007_ad_team;
mod m0008_repo_game;
mod m0009_ad_container;
mod m0010_koth_container;
mod m0011_suspicion_koth;
mod m0012_network_mode;
mod m0013_shared_container;
mod m0014_ad_sla_credit;
mod m0015_original_archive;
mod m0016_log_fingerprint;
mod m0017_container_access_event;
mod m0018_honeypot_hit;
mod m0019_flag_egress_event;
mod m0020_ad_vpn_peer;
mod m0021_hot_indexes;
mod m0022_more_hot_indexes;
mod m0023_koth_token_mint;
mod m0024_koth_token_indexes;
mod m0025_advance_race_unique;
mod m0026_ad_attack_dedup_unique;
mod m0027_hot_filter_indexes;
mod m0028_ad_round_id_indexes;
mod m0029_flag_context_index;
mod m0030_instance_uniqueness;
mod m0031_ad_vpn_address_uniqueness;
mod m0032_koth_capability_integrity;
mod m0033_ad_credential_uniqueness;
mod m0034_ad_round_atomicity;
mod m0035_ad_epoch_scoring;
mod m0036_ad_epoch_score_rollups;
mod m0037_ad_service_score_rollups;
mod m0038_koth_epoch_scoring;
mod m0039_koth_epoch_score_rollups;
mod m0040_koth_integrity;
mod m0041_koth_token_revocation;
mod m0042_koth_dead_container_receipts;
mod m0043_jeopardy_integrity;
mod m0044_ad_round_pipeline_lease;
mod m0045_drop_repo_binding_game_id;
mod m0046_koth_crown_cycles;
mod m0047_koth_token_target_cascade;
mod m0048_koth_crown_only;
mod m0049_ad_flag_publication;
mod m0050_ad_flag_delivery_results;
mod m0051_ad_ownership_cascades;
mod m0052_suspicion_event_uniqueness;
mod m0053_roster_indexes;
mod m0054_ad_network_reconcile;
mod m0055_file_hash_uniqueness;
mod m0056_runtime_role_heartbeats;
mod m0057_traffic_capture_reconcile;
mod m0058_constant_koth_scoring;
mod m0059_traffic_capture_results;
mod m0060_build_context_subdir;
mod m0061_traffic_capture_failures;
mod m0062_traffic_capture_owner_lease;
mod m0063_immutable_challenge_images;
mod m0064_runtime_build_fingerprint;
mod m0065_worker_plane;
mod m0066_challenge_workload_spec;
mod m0067_worker_workload_maintenance;
mod m0068_worker_workload_dimensions;
mod m0069_worker_local_image_digest;
mod m0070_flag_egress_identity;
mod m0071_team_deletion_fence;
mod m0072_koth_crown_cycle_defaults;
mod m0073_finite_lockout_end;
mod m0074_game_deletion_fence;
mod m0075_challenge_deletion_fence;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m0001_init::Migration),
            Box::new(m0002_extra::Migration),
            Box::new(m0003_managers::Migration),
            Box::new(m0004_repo::Migration),
            Box::new(m0005_anticheat::Migration),
            Box::new(m0006_builds::Migration),
            Box::new(m0007_ad_team::Migration),
            Box::new(m0008_repo_game::Migration),
            Box::new(m0009_ad_container::Migration),
            Box::new(m0010_koth_container::Migration),
            Box::new(m0011_suspicion_koth::Migration),
            Box::new(m0012_network_mode::Migration),
            Box::new(m0013_shared_container::Migration),
            Box::new(m0014_ad_sla_credit::Migration),
            Box::new(m0015_original_archive::Migration),
            Box::new(m0016_log_fingerprint::Migration),
            Box::new(m0017_container_access_event::Migration),
            Box::new(m0018_honeypot_hit::Migration),
            Box::new(m0019_flag_egress_event::Migration),
            Box::new(m0020_ad_vpn_peer::Migration),
            Box::new(m0021_hot_indexes::Migration),
            Box::new(m0022_more_hot_indexes::Migration),
            Box::new(m0023_koth_token_mint::Migration),
            Box::new(m0024_koth_token_indexes::Migration),
            Box::new(m0025_advance_race_unique::Migration),
            Box::new(m0026_ad_attack_dedup_unique::Migration),
            Box::new(m0027_hot_filter_indexes::Migration),
            Box::new(m0028_ad_round_id_indexes::Migration),
            Box::new(m0029_flag_context_index::Migration),
            Box::new(m0030_instance_uniqueness::Migration),
            Box::new(m0031_ad_vpn_address_uniqueness::Migration),
            Box::new(m0032_koth_capability_integrity::Migration),
            Box::new(m0033_ad_credential_uniqueness::Migration),
            Box::new(m0034_ad_round_atomicity::Migration),
            Box::new(m0035_ad_epoch_scoring::Migration),
            Box::new(m0036_ad_epoch_score_rollups::Migration),
            Box::new(m0037_ad_service_score_rollups::Migration),
            Box::new(m0038_koth_epoch_scoring::Migration),
            Box::new(m0039_koth_epoch_score_rollups::Migration),
            Box::new(m0040_koth_integrity::Migration),
            Box::new(m0041_koth_token_revocation::Migration),
            Box::new(m0042_koth_dead_container_receipts::Migration),
            Box::new(m0043_jeopardy_integrity::Migration),
            Box::new(m0044_ad_round_pipeline_lease::Migration),
            Box::new(m0045_drop_repo_binding_game_id::Migration),
            Box::new(m0046_koth_crown_cycles::Migration),
            Box::new(m0047_koth_token_target_cascade::Migration),
            Box::new(m0048_koth_crown_only::Migration),
            Box::new(m0049_ad_flag_publication::Migration),
            Box::new(m0050_ad_flag_delivery_results::Migration),
            Box::new(m0051_ad_ownership_cascades::Migration),
            Box::new(m0052_suspicion_event_uniqueness::Migration),
            Box::new(m0053_roster_indexes::Migration),
            Box::new(m0054_ad_network_reconcile::Migration),
            Box::new(m0055_file_hash_uniqueness::Migration),
            Box::new(m0056_runtime_role_heartbeats::Migration),
            Box::new(m0057_traffic_capture_reconcile::Migration),
            Box::new(m0058_constant_koth_scoring::Migration),
            Box::new(m0059_traffic_capture_results::Migration),
            Box::new(m0060_build_context_subdir::Migration),
            Box::new(m0061_traffic_capture_failures::Migration),
            Box::new(m0062_traffic_capture_owner_lease::Migration),
            Box::new(m0063_immutable_challenge_images::Migration),
            Box::new(m0064_runtime_build_fingerprint::Migration),
            Box::new(m0065_worker_plane::Migration),
            Box::new(m0066_challenge_workload_spec::Migration),
            Box::new(m0067_worker_workload_maintenance::Migration),
            Box::new(m0068_worker_workload_dimensions::Migration),
            Box::new(m0069_worker_local_image_digest::Migration),
            Box::new(m0070_flag_egress_identity::Migration),
            Box::new(m0071_team_deletion_fence::Migration),
            Box::new(m0072_koth_crown_cycle_defaults::Migration),
            Box::new(m0073_finite_lockout_end::Migration),
            Box::new(m0074_game_deletion_fence::Migration),
            Box::new(m0075_challenge_deletion_fence::Migration),
        ]
    }
}

/// Verify that the database migration ledger exactly matches this binary.
///
/// Split runtime roles deliberately never call [`Migrator::up`]. This check is
/// also deliberately read-only: a missing ledger is an operator error rather
/// than permission for every replica to race to initialize or migrate it.
pub async fn ensure_schema_current(db: &DatabaseConnection) -> anyhow::Result<()> {
    let applied = sqlx::query_scalar::<_, String>(
        r#"SELECT version FROM seaql_migrations ORDER BY version"#,
    )
    .fetch_all(db.get_postgres_connection_pool())
    .await
    .map_err(|error| {
        anyhow::anyhow!(
            "migration ledger check failed: {error}; run the RSCTF_ROLE=migrate job before starting split roles"
        )
    })?;

    let expected = Migrator::migrations()
        .into_iter()
        .map(|migration| migration.name().to_owned())
        .collect::<Vec<_>>();
    let (missing, unexpected) = migration_ledger_diff(&expected, &applied);

    if missing.is_empty() && unexpected.is_empty() {
        return Ok(());
    }

    let mut details = Vec::with_capacity(2);
    if !missing.is_empty() {
        details.push(format!("pending migrations: {}", missing.join(", ")));
    }
    if !unexpected.is_empty() {
        details.push(format!(
            "migrations unknown to this binary: {}",
            unexpected.join(", ")
        ));
    }
    anyhow::bail!(
        "database schema is incompatible with this rsctf binary ({}); run the matching RSCTF_ROLE=migrate job before starting split roles",
        details.join("; ")
    )
}

fn migration_ledger_diff(expected: &[String], applied: &[String]) -> (Vec<String>, Vec<String>) {
    let applied_set = applied.iter().map(String::as_str).collect::<HashSet<_>>();
    let expected_set = expected.iter().map(String::as_str).collect::<HashSet<_>>();
    let mut missing = expected_set
        .difference(&applied_set)
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    let mut unexpected = applied_set
        .difference(&expected_set)
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    missing.sort_unstable();
    unexpected.sort_unstable();
    (missing, unexpected)
}

#[cfg(test)]
mod tests {
    use super::migration_ledger_diff;

    #[test]
    fn migration_ledger_requires_an_exact_version_set() {
        let expected = vec!["m0001".to_owned(), "m0002".to_owned()];
        let current = expected.clone();
        assert_eq!(
            migration_ledger_diff(&expected, &current),
            (Vec::new(), Vec::new())
        );

        let applied = vec!["m0001".to_owned(), "m9999".to_owned()];
        assert_eq!(
            migration_ledger_diff(&expected, &applied),
            (vec!["m0002".to_owned()], vec!["m9999".to_owned()])
        );
    }
}
