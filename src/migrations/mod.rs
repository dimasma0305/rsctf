//! Database migrations. The initial migration is derived directly from the
//! `rsctf-entity` models via `Schema::create_table_from_entity`, so the DDL
//! can never drift from the entity definitions.

use std::collections::HashSet;

pub use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::DatabaseConnection;

#[cfg(test)]
pub(crate) fn test_process_application_name() -> &'static str {
    static APPLICATION_NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    APPLICATION_NAME.get_or_init(|| format!("rsctf:test:process:{}", uuid::Uuid::new_v4().simple()))
}

#[cfg(test)]
pub(crate) fn test_pg_connect_options(database_url: &str) -> sqlx::postgres::PgConnectOptions {
    <sqlx::postgres::PgConnectOptions as std::str::FromStr>::from_str(database_url)
        .expect("parse test database URL")
        .application_name(test_process_application_name())
}

#[cfg(test)]
mod admin_mutation_migration_tests;
#[cfg(test)]
mod credential_workflow_tests;
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
mod m0076_build_image_ownership;
mod m0077_ad_inspector_ownership;
mod m0078_game_manager_lookup_index;
mod m0079_game_configuration_integrity;
mod m0080_challenge_review_uniqueness;
mod m0081_build_record_lifecycle;
mod m0082_ad_service_snapshots;
mod m0083_koth_api_observers;
mod m0084_koth_api_arena;
mod m0085_constant_leaderboard_scoring;
mod m0086_koth_api_event_tokens;
mod m0087_asset_download_indexes;
mod m0088_koth_api_wave_scoring;
mod m0089_cheat_evidence_ledger;
mod m0090_identity_observations;
mod m0091_suspicion_score_integrity;
mod m0092_event_vpn_policy;
mod m0093_bounded_anticheat_telemetry;
mod m0094_challenge_variants_and_receipts;
mod m0095_player_field_bounds;
mod m0096_container_storage_bounds;
mod m0097_container_network_modes;
mod m0098_build_image_retention;
mod m0099_variant_generator_builds;
mod m0100_container_policy_bounds;
mod m0101_blood_bonus_default;
mod m0102_discord_webhook_outbox;
mod m0103_recent_games_candidates;
mod m0104_post_feed_order;
mod m0105_manager_autocomplete_indexes;
mod m0106_submission_idempotency;
mod m0107_monitor_history_indexes;
mod m0108_koth_observer_rotation_operations;
mod m0109_operator_console_latest_rows;
mod m0110_participation_review_indexes;
mod m0111_game_event_feed_cursor;
mod m0112_koth_target_reporters;
mod m0113_koth_reporter_routing_revision;
mod m0114_submission_feed_cursor;
mod m0115_flag_egress_feed_cursor;
mod m0116_game_event_feed_pending;
mod m0242_participation_provision_jobs;
mod m0250_team_signature_key_index;
mod m0251_koth_referee_retry;
mod m0252_player_credential_operations;
mod m0260_ad_control_revisions;
mod m0261_control_plane_jobs;
mod m0262_challenge_import_jobs;
mod m0263_control_job_cancellation;
mod m0264_blob_staging_operations;
mod m0265_game_notice_delivery;
mod m0270_worker_workload_quarantine;
mod m0271_worker_enrollment_operations;
mod m0272_event_sensor_batches;
mod m0273_receipt_variant_lifecycle;
mod m0280_traffic_capture_inventory;
mod m0281_anticheat_read_bounds;
mod m0282_docker_image_cleanup_jobs;
mod m0283_incremental_anticheat_reconciliation;
mod m0284_anticheat_dirty_outboxes;
mod m0285_honeypot_telemetry_buckets;
mod m0286_docker_image_cleanup_order;
mod m0290_distributed_proxy_admission;
mod m0300_game_clone_operations;
mod m0301_admin_credential_jobs;
mod m0302_credential_mutation_recovery;
mod m0303_mail_outbox;
mod m0304_platform_settings_operations;
mod m0305_event_vpn_override_operations;
mod m0306_bulk_challenge_mutations;
mod m0307_division_revision_operations;
mod m0308_team_invite_rotation;
mod m0309_flag_import_operations;
mod m0330_mail_preparation_slots;
mod m0331_username_scoreboard_invalidation;
mod m0333_account_mail_consumption;
mod m0334_flag_import_staging;

#[cfg(test)]
pub(crate) use m0103_recent_games_candidates::UP_SQL as RECENT_GAMES_INDEX_SQL;
#[cfg(test)]
pub(crate) use m0107_monitor_history_indexes::UP_SQL as MONITOR_HISTORY_INDEX_SQL;
#[cfg(test)]
pub(crate) use m0108_koth_observer_rotation_operations::UP_SQL as KOTH_OBSERVER_ROTATION_SQL;
#[cfg(test)]
pub(crate) use m0109_operator_console_latest_rows::UP_SQL as OPERATOR_LATEST_INDEX_SQL;
#[cfg(test)]
pub(crate) use m0110_participation_review_indexes::UP_SQL as PARTICIPATION_REVIEW_INDEX_SQL;
#[cfg(test)]
pub(crate) use m0111_game_event_feed_cursor::UP_SQL as GAME_EVENT_FEED_CURSOR_SQL;
#[cfg(test)]
pub(crate) use m0114_submission_feed_cursor::UP_SQL as SUBMISSION_FEED_CURSOR_SQL;
#[cfg(test)]
pub(crate) use m0115_flag_egress_feed_cursor::UP_SQL as FLAG_EGRESS_FEED_CURSOR_SQL;
#[cfg(test)]
pub(crate) use m0116_game_event_feed_pending::UP_SQL as GAME_EVENT_FEED_PENDING_SQL;
#[cfg(test)]
pub(crate) use m0242_participation_provision_jobs::UP_SQL as PARTICIPATION_PROVISION_JOBS_SQL;
#[cfg(test)]
pub(crate) use m0280_traffic_capture_inventory::UP_SQL as TRAFFIC_CAPTURE_INVENTORY_SQL;
#[cfg(test)]
pub(crate) use m0286_docker_image_cleanup_order::UP_SQL as IMAGE_CLEANUP_ORDER_INDEX_SQL;

pub struct Migrator;

const EXCLUSIVE_CUTOVER_MIGRATIONS: [&str; 3] = [
    "m0089_cheat_evidence_ledger",
    "m0090_identity_observations",
    "m0091_suspicion_score_integrity",
];

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
            Box::new(m0076_build_image_ownership::Migration),
            Box::new(m0077_ad_inspector_ownership::Migration),
            Box::new(m0078_game_manager_lookup_index::Migration),
            Box::new(m0079_game_configuration_integrity::Migration),
            Box::new(m0080_challenge_review_uniqueness::Migration),
            Box::new(m0081_build_record_lifecycle::Migration),
            Box::new(m0082_ad_service_snapshots::Migration),
            Box::new(m0083_koth_api_observers::Migration),
            Box::new(m0084_koth_api_arena::Migration),
            Box::new(m0085_constant_leaderboard_scoring::Migration),
            Box::new(m0086_koth_api_event_tokens::Migration),
            Box::new(m0087_asset_download_indexes::Migration),
            Box::new(m0088_koth_api_wave_scoring::Migration),
            Box::new(m0089_cheat_evidence_ledger::Migration),
            Box::new(m0090_identity_observations::Migration),
            Box::new(m0091_suspicion_score_integrity::Migration),
            Box::new(m0092_event_vpn_policy::Migration),
            Box::new(m0093_bounded_anticheat_telemetry::Migration),
            Box::new(m0094_challenge_variants_and_receipts::Migration),
            Box::new(m0095_player_field_bounds::Migration),
            Box::new(m0096_container_storage_bounds::Migration),
            Box::new(m0097_container_network_modes::Migration),
            Box::new(m0098_build_image_retention::Migration),
            Box::new(m0099_variant_generator_builds::Migration),
            Box::new(m0100_container_policy_bounds::Migration),
            Box::new(m0101_blood_bonus_default::Migration),
            Box::new(m0102_discord_webhook_outbox::Migration),
            Box::new(m0103_recent_games_candidates::Migration),
            Box::new(m0104_post_feed_order::Migration),
            Box::new(m0105_manager_autocomplete_indexes::Migration),
            Box::new(m0106_submission_idempotency::Migration),
            Box::new(m0107_monitor_history_indexes::Migration),
            Box::new(m0108_koth_observer_rotation_operations::Migration),
            Box::new(m0109_operator_console_latest_rows::Migration),
            Box::new(m0110_participation_review_indexes::Migration),
            Box::new(m0111_game_event_feed_cursor::Migration),
            Box::new(m0112_koth_target_reporters::Migration),
            Box::new(m0113_koth_reporter_routing_revision::Migration),
            Box::new(m0114_submission_feed_cursor::Migration),
            Box::new(m0115_flag_egress_feed_cursor::Migration),
            Box::new(m0116_game_event_feed_pending::Migration),
            Box::new(m0242_participation_provision_jobs::Migration),
            Box::new(m0250_team_signature_key_index::Migration),
            Box::new(m0251_koth_referee_retry::Migration),
            Box::new(m0252_player_credential_operations::Migration),
            Box::new(m0260_ad_control_revisions::Migration),
            Box::new(m0261_control_plane_jobs::Migration),
            Box::new(m0262_challenge_import_jobs::Migration),
            Box::new(m0263_control_job_cancellation::Migration),
            Box::new(m0264_blob_staging_operations::Migration),
            Box::new(m0265_game_notice_delivery::Migration),
            Box::new(m0270_worker_workload_quarantine::Migration),
            Box::new(m0271_worker_enrollment_operations::Migration),
            Box::new(m0272_event_sensor_batches::Migration),
            Box::new(m0273_receipt_variant_lifecycle::Migration),
            Box::new(m0280_traffic_capture_inventory::Migration),
            Box::new(m0281_anticheat_read_bounds::Migration),
            Box::new(m0282_docker_image_cleanup_jobs::Migration),
            Box::new(m0283_incremental_anticheat_reconciliation::Migration),
            Box::new(m0284_anticheat_dirty_outboxes::Migration),
            Box::new(m0285_honeypot_telemetry_buckets::Migration),
            Box::new(m0286_docker_image_cleanup_order::Migration),
            Box::new(m0290_distributed_proxy_admission::Migration),
            Box::new(m0300_game_clone_operations::Migration),
            Box::new(m0301_admin_credential_jobs::Migration),
            Box::new(m0302_credential_mutation_recovery::Migration),
            Box::new(m0303_mail_outbox::Migration),
            Box::new(m0304_platform_settings_operations::Migration),
            Box::new(m0305_event_vpn_override_operations::Migration),
            Box::new(m0306_bulk_challenge_mutations::Migration),
            Box::new(m0307_division_revision_operations::Migration),
            Box::new(m0308_team_invite_rotation::Migration),
            Box::new(m0309_flag_import_operations::Migration),
            Box::new(m0330_mail_preparation_slots::Migration),
            Box::new(m0331_username_scoreboard_invalidation::Migration),
            Box::new(m0333_account_mail_consumption::Migration),
            Box::new(m0334_flag_import_staging::Migration),
        ]
    }
}

async fn exclusive_cutover_is_pending(db: &DatabaseConnection) -> anyhow::Result<bool> {
    let pool = db.get_postgres_connection_pool();
    let ledger_exists: bool =
        sqlx::query_scalar("SELECT to_regclass('seaql_migrations') IS NOT NULL")
            .fetch_one(pool)
            .await?;
    if !ledger_exists {
        return Ok(true);
    }
    let applied = sqlx::query_scalar::<_, String>("SELECT version FROM seaql_migrations")
        .fetch_all(pool)
        .await?
        .into_iter()
        .collect::<HashSet<_>>();
    Ok(EXCLUSIVE_CUTOVER_MIGRATIONS
        .iter()
        .any(|migration| !applied.contains(*migration)))
}

async fn ensure_no_other_database_clients(pool: &sqlx::PgPool) -> anyhow::Result<()> {
    let mut connection = pool.acquire().await?;
    let application_name: String = sqlx::query_scalar("SELECT current_setting('application_name')")
        .fetch_one(&mut *connection)
        .await?;
    if !application_name.starts_with("rsctf:") {
        anyhow::bail!(
            "exclusive migration cutover requires rsctf's process-unique PostgreSQL application_name"
        );
    }
    let other_clients: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)::bigint
             FROM pg_stat_activity other
            WHERE other.datid = (
                    SELECT oid FROM pg_database
                     WHERE datname = current_database()
                  )
              AND other.pid <> pg_backend_pid()
              AND other.usesysid IS NOT NULL
              AND other.application_name IS DISTINCT FROM $1"#,
    )
    .bind(&application_name)
    .fetch_one(&mut *connection)
    .await?;
    if other_clients != 0 {
        anyhow::bail!(
            "exclusive schema cutover refused: found {other_clients} other PostgreSQL client session(s) in this database; scale every rsctf runtime role to zero and drain PgBouncer, monitors, and administrative sessions before retrying the migration job"
        );
    }
    Ok(())
}

/// Enforce the stop-the-world boundary required by m0089..m0091 before the
/// migrator issues any DDL. Migration-job retries force the check even after
/// the ledger committed because keyed post-migration bootstrap still belongs
/// to the same cutover. Deployment prevents reconnects after this point.
pub async fn ensure_exclusive_cutover_ready(
    db: &DatabaseConnection,
    force: bool,
) -> anyhow::Result<()> {
    if force || exclusive_cutover_is_pending(db).await? {
        ensure_no_other_database_clients(db.get_postgres_connection_pool()).await?;
    }
    Ok(())
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
    use std::{str::FromStr, time::Duration};

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use sqlx::{ConnectOptions as _, Connection as _};

    use super::{ensure_no_other_database_clients, migration_ledger_diff, Migrator, MigratorTrait};

    #[test]
    fn recent_migration_identities_preserve_shipped_order() {
        let names = Migrator::migrations()
            .into_iter()
            .map(|migration| migration.name().to_owned())
            .collect::<Vec<_>>();

        assert_eq!(
            &names[names.len() - 48..],
            [
                "m0103_recent_games_candidates",
                "m0104_post_feed_order",
                "m0105_manager_autocomplete_indexes",
                "m0106_submission_idempotency",
                "m0107_monitor_history_indexes",
                "m0108_koth_observer_rotation_operations",
                "m0109_operator_console_latest_rows",
                "m0110_participation_review_indexes",
                "m0111_game_event_feed_cursor",
                "m0112_koth_target_reporters",
                "m0113_koth_reporter_routing_revision",
                "m0114_submission_feed_cursor",
                "m0115_flag_egress_feed_cursor",
                "m0116_game_event_feed_pending",
                "m0242_participation_provision_jobs",
                "m0250_team_signature_key_index",
                "m0251_koth_referee_retry",
                "m0252_player_credential_operations",
                "m0260_ad_control_revisions",
                "m0261_control_plane_jobs",
                "m0262_challenge_import_jobs",
                "m0263_control_job_cancellation",
                "m0264_blob_staging_operations",
                "m0265_game_notice_delivery",
                "m0270_worker_workload_quarantine",
                "m0271_worker_enrollment_operations",
                "m0272_event_sensor_batches",
                "m0273_receipt_variant_lifecycle",
                "m0280_traffic_capture_inventory",
                "m0281_anticheat_read_bounds",
                "m0282_docker_image_cleanup_jobs",
                "m0283_incremental_anticheat_reconciliation",
                "m0284_anticheat_dirty_outboxes",
                "m0285_honeypot_telemetry_buckets",
                "m0286_docker_image_cleanup_order",
                "m0290_distributed_proxy_admission",
                "m0300_game_clone_operations",
                "m0301_admin_credential_jobs",
                "m0302_credential_mutation_recovery",
                "m0303_mail_outbox",
                "m0304_platform_settings_operations",
                "m0305_event_vpn_override_operations",
                "m0306_bulk_challenge_mutations",
                "m0307_division_revision_operations",
                "m0308_team_invite_rotation",
                "m0309_flag_import_operations",
                "m0330_mail_preparation_slots",
                "m0331_username_scoreboard_invalidation",
            ]
        );
    }

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

    #[tokio::test]
    #[ignore = "requires PostgreSQL administrator via RSCTF_TEST_DATABASE_URL"]
    async fn exclusive_cutover_allows_own_pool_but_rejects_an_old_client() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let role_name = format!("rsctf_cutover_{suffix}");
        let database_name = role_name.clone();
        let password = uuid::Uuid::new_v4().simple().to_string();
        assert!(role_name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'));

        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        sqlx::query(&format!(
            r#"CREATE ROLE "{role_name}" LOGIN PASSWORD '{password}' NOSUPERUSER"#
        ))
        .execute(&admin)
        .await
        .unwrap();
        sqlx::query(&format!(
            r#"CREATE DATABASE "{database_name}" OWNER "{role_name}""#
        ))
        .execute(&admin)
        .await
        .unwrap();

        let process_application_name = format!("rsctf:migrate:test:{suffix}");
        let process_options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .database(&database_name)
            .username(&role_name)
            .password(&password)
            .application_name(&process_application_name)
            .disable_statement_logging();
        let process_pool = PgPoolOptions::new()
            .min_connections(2)
            .max_connections(2)
            .connect_with(process_options)
            .await
            .unwrap();
        let is_superuser: bool =
            sqlx::query_scalar("SELECT rolsuper FROM pg_roles WHERE rolname = current_user")
                .fetch_one(&process_pool)
                .await
                .unwrap();
        assert!(!is_superuser);
        ensure_no_other_database_clients(&process_pool)
            .await
            .expect("both own baseline connections share one process identity");

        let own_sessions: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*)::bigint FROM pg_stat_activity
                WHERE datname = current_database() AND application_name = $1"#,
        )
        .bind(&process_application_name)
        .fetch_one(&process_pool)
        .await
        .unwrap();
        assert_eq!(own_sessions, 2);

        let old_options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .database(&database_name)
            .username(&role_name)
            .password(&password)
            .application_name("legacy-rsctf-web")
            .disable_statement_logging();
        let old_connection = sqlx::PgConnection::connect_with(&old_options)
            .await
            .unwrap();
        let blocked = ensure_no_other_database_clients(&process_pool)
            .await
            .expect_err("a different application name in the same database must block");
        assert!(blocked
            .to_string()
            .contains("found 1 other PostgreSQL client"));
        old_connection.close().await.unwrap();
        ensure_no_other_database_clients(&process_pool)
            .await
            .expect("disconnecting the old client clears the cutover fence");

        process_pool.close().await;
        let mut remaining_sessions = 1_i64;
        for _ in 0..40 {
            remaining_sessions = sqlx::query_scalar(
                "SELECT COUNT(*)::bigint FROM pg_stat_activity WHERE datname = $1",
            )
            .bind(&database_name)
            .fetch_one(&admin)
            .await
            .unwrap();
            if remaining_sessions == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(remaining_sessions, 0);
        sqlx::query(&format!(r#"DROP DATABASE "{database_name}""#))
            .execute(&admin)
            .await
            .unwrap();
        sqlx::query(&format!(r#"DROP ROLE "{role_name}""#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
