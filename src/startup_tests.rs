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
fn advanced_topology_owns_mail_and_repository_consumers() {
    use rsctf::runtime_workers::{for_role, DurableWorker};
    let engine = for_role(RuntimeRole::Engine);
    assert!(engine.contains(&DurableWorker::MailDelivery));
    assert!(engine.contains(&DurableWorker::ChallengeImport));
    assert!(engine.contains(&DurableWorker::RepositoryPush));
    assert!(engine.contains(&DurableWorker::RepositoryScan));
    assert!(for_role(RuntimeRole::Web).is_empty());
    assert!(for_role(RuntimeRole::Network).is_empty());
}

#[test]
fn combined_role_with_migrations_disabled_takes_schema_verification_path() {
    assert!(!should_run_migrations(RuntimeRole::All, true));
    assert!(should_run_migrations(RuntimeRole::All, false));
    assert!(should_run_migrations(RuntimeRole::Migrate, true));
    assert!(should_run_migrations(RuntimeRole::Development, true));
    assert!(!should_run_migrations(RuntimeRole::Web, false));
}
