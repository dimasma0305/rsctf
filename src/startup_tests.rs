use super::*;

#[test]
fn installs_an_explicit_tls_crypto_provider() {
    install_tls_crypto_provider().unwrap();
    assert!(tokio_rustls::rustls::crypto::CryptoProvider::get_default().is_some());
}

#[test]
fn every_supported_control_topology_owns_suspicion_reconciliation() {
    assert!(owns_suspicion_reconciliation(RuntimeRole::All));
    assert!(owns_suspicion_reconciliation(RuntimeRole::Control));
    assert!(owns_suspicion_reconciliation(RuntimeRole::Engine));
    assert!(!owns_suspicion_reconciliation(RuntimeRole::Web));
    assert!(owns_suspicion_reconciliation(RuntimeRole::Development));
    assert!(!owns_suspicion_reconciliation(RuntimeRole::Network));
    assert!(!owns_suspicion_reconciliation(RuntimeRole::Migrate));
}

#[test]
fn feed_reconciliation_runs_only_on_durable_control_roles() {
    assert!(owns_feed_reconciliation(RuntimeRole::All));
    assert!(owns_feed_reconciliation(RuntimeRole::Control));
    assert!(owns_feed_reconciliation(RuntimeRole::Engine));
    assert!(owns_feed_reconciliation(RuntimeRole::Development));
    assert!(!owns_feed_reconciliation(RuntimeRole::Web));
    assert!(!owns_feed_reconciliation(RuntimeRole::Network));
    assert!(!owns_feed_reconciliation(RuntimeRole::Migrate));
}

#[test]
fn mail_reconciliation_runs_only_on_the_external_control_owner() {
    assert!(owns_mail_reconciliation(RuntimeRole::All));
    assert!(owns_mail_reconciliation(RuntimeRole::Control));
    assert!(owns_mail_reconciliation(RuntimeRole::Development));
    assert!(!owns_mail_reconciliation(RuntimeRole::Web));
    assert!(!owns_mail_reconciliation(RuntimeRole::Engine));
    assert!(!owns_mail_reconciliation(RuntimeRole::Network));
    assert!(!owns_mail_reconciliation(RuntimeRole::Migrate));
}

#[test]
fn combined_role_with_migrations_disabled_takes_schema_verification_path() {
    assert!(!should_run_migrations(RuntimeRole::All, true));
    assert!(should_run_migrations(RuntimeRole::All, false));
    assert!(should_run_migrations(RuntimeRole::Migrate, true));
    assert!(should_run_migrations(RuntimeRole::Development, true));
    assert!(!should_run_migrations(RuntimeRole::Web, false));
}

#[test]
fn repository_workers_are_required_in_control_and_development_topologies() {
    let source = include_str!("main.rs");
    assert_eq!(
        source.matches("\"repository binding scheduler\"").count(),
        2
    );
    assert_eq!(source.matches("\"repository push worker\"").count(), 2);
    assert!(source.contains("services::repo_binding_scheduler::start"));
    assert!(source.contains("start_repo_push_worker"));
}
