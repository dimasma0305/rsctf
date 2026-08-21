//! Database connection helper.

use std::time::Duration;

use sea_orm::{DatabaseConnection, SqlxPostgresConnector};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{ConnectOptions as _, Connection as _};

use crate::models::internal::configs::RuntimeRole;

const IDLE_CONNECTION_TIMEOUT: Duration = Duration::from_secs(60);
const POSTGRES_APPLICATION_NAME_MAX_BYTES: usize = 63;
// The singleton suspicion reconciler retains one advisory-lock transaction
// while its closure barrier or detector uses one nested checkout.
const SUSPICION_RECONCILER_CONNECTIONS: usize = 2;

fn process_application_name(role: RuntimeRole) -> String {
    let prefix = format!("rsctf:{role}:");
    let suffix = format!(":{}", uuid::Uuid::new_v4().simple());
    let available_version_bytes = POSTGRES_APPLICATION_NAME_MAX_BYTES
        .saturating_sub(prefix.len())
        .saturating_sub(suffix.len());
    let version = env!("CARGO_PKG_VERSION")
        .chars()
        .take(available_version_bytes)
        .collect::<String>();
    format!("{prefix}{version}{suffix}")
}

fn pool_options(max_connections: u32) -> PgPoolOptions {
    PgPoolOptions::new()
        .max_connections(max_connections)
        .min_connections(2)
        .acquire_timeout(Duration::from_secs(10))
        // Release burst-only backends promptly so a rolling replica increase
        // does not retain the prior traffic spike's entire connection budget.
        // `min_connections` keeps the steady hot path warm.
        .idle_timeout(Some(IDLE_CONNECTION_TIMEOUT))
        // Transaction starts are completed in detached tasks at the call
        // sites, so a request cancellation cannot strand an untracked BEGIN.
        // Avoid a probe on every checkout; SQLx still performs its mandatory
        // on-release protocol drain and we probe sockets after meaningful idle.
        .test_before_acquire(false)
        .before_acquire(|connection, metadata| {
            Box::pin(async move {
                // Hot connections were just drained on release. Probe only
                // after a meaningful idle interval so a database/network
                // restart cannot hand a stale socket to the next request.
                if metadata.idle_for >= Duration::from_secs(30) {
                    connection.ping().await?;
                }
                Ok(true)
            })
        })
}

pub async fn connect(url: &str) -> anyhow::Result<DatabaseConnection> {
    // 32 active query connections is the sweet spot when app + Postgres share a
    // host. The 33rd default slot is reserved for the singleton reconciler's
    // long-held fence, preserving that measured active-work ceiling. Raising the
    // active budget to 64 regressed throughput ~16% on the load-test host.
    let max_conns = std::env::var("RSCTF_DB_MAX_CONNECTIONS")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(33);
    let repo_scan_concurrency = std::env::var("RSCTF_REPO_SCAN_CONCURRENCY")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| (1..=4).contains(value))
        .unwrap_or(1);
    let vpn_enabled = std::env::var("RSCTF_AD_VPN_ENABLED")
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false);
    let provisioning_concurrency = std::env::var("RSCTF_PROVISIONING_CONCURRENCY")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(4);
    let role = std::env::var("RSCTF_ROLE")
        .ok()
        .and_then(|value| value.parse::<RuntimeRole>().ok())
        .unwrap_or_default();
    let required = required_pool_connections(
        repo_scan_concurrency,
        provisioning_concurrency,
        vpn_enabled,
        role,
    );
    if (max_conns as usize) < required {
        anyhow::bail!(
            "RSCTF_DB_MAX_CONNECTIONS must be at least {required} for RSCTF_ROLE={role} with RSCTF_REPO_SCAN_CONCURRENCY={repo_scan_concurrency}, RSCTF_PROVISIONING_CONCURRENCY={provisioning_concurrency}, and RSCTF_AD_VPN_ENABLED={vpn_enabled}"
        );
    }
    // PostgreSQL truncates application_name at 63 bytes. Keep a compact,
    // process-unique identity on every SQLx/SeaORM pool connection so the
    // stop-the-world migration preflight can distinguish this pool's own two
    // baseline sessions from every old replica, PgBouncer, and monitor.
    let application_name = process_application_name(role);
    let connect_options = url
        .parse::<PgConnectOptions>()?
        .application_name(&application_name)
        .disable_statement_logging();
    let pool = pool_options(max_conns)
        .connect_with(connect_options)
        .await?;
    Ok(SqlxPostgresConnector::from_sqlx_postgres_pool(pool))
}

/// Conservative no-deadlock floor for operations that retain pool connections
/// while awaiting nested work.
///
/// A checker-bearing repository scan can retain checkout, game-control,
/// checker-publication, and challenge-definition locks while its model write
/// leases another connection (5R).
/// Provisioning can hold one advisory lock while issuing a query (2P). A
/// network owner always retains the singleton BYOC ownership lease. When VPN
/// is enabled it also retains a PgListener and needs room for nested kernel
/// reconciliation. All/Development/Control/Engine run one suspicion reconciler
/// that retains its advisory transaction while one nested closure/detector
/// checkout progresses.
/// The one-shot migration role opens none of these paths and needs only the
/// pool's two baseline connections.
fn required_pool_connections(
    repo_scan_concurrency: usize,
    provisioning_concurrency: usize,
    vpn_enabled: bool,
    role: RuntimeRole,
) -> usize {
    if role == RuntimeRole::Migrate {
        return 2;
    }
    let scans = repo_scan_concurrency.saturating_mul(5);
    let provisioning = provisioning_concurrency.saturating_mul(2);
    let owner_connections = match (role.capabilities().network, vpn_enabled) {
        (true, true) => 6,
        // Network/BYOC ownership and traffic-capture ownership each retain a
        // session; keep one more checkout available for forward progress.
        (true, false) => 3,
        (false, _) => 1,
    };
    // Credential issuance or fail-closed roster teardown retains one roster
    // transaction. VPN/BYOC work can additionally hold its allocator/reconciler
    // transaction while issuing one query, so reserve three connections per
    // admitted operation. SSH and team-token mutations use less, but share the
    // same admission budget.
    // Only the monolith and scalable web role mount the ordinary account,
    // team, and player A&D controllers that acquire these guards. The
    // control/network role's deliberately narrow stateful router must not be
    // charged for controller paths it cannot serve.
    let serves_player_api = matches!(
        role,
        RuntimeRole::All | RuntimeRole::Development | RuntimeRole::Web
    );
    let roster_access = serves_player_api
        .then_some(crate::utils::single_flight::ROSTER_ACCESS_CONCURRENCY.saturating_mul(3));
    // An admin account update/deletion retains one session-level lifecycle
    // lease while its registration or roster transaction uses another
    // connection. Admission is independently bounded to avoid nested-gate
    // deadlocks with roster teardown.
    let account_lifecycle = serves_player_api
        .then_some(crate::utils::single_flight::ACCOUNT_LIFECYCLE_CONCURRENCY.saturating_mul(2));
    // A runtime eligibility transition can retain its outer transition, game,
    // and definition transactions while one final query or model write makes
    // progress. Its independent one-at-a-time admission gate therefore needs
    // four connections only on roles that expose editor controllers.
    let runtime_transition = serves_player_api.then_some(4usize);
    let suspicion_reconciler = matches!(
        role,
        RuntimeRole::All | RuntimeRole::Development | RuntimeRole::Control | RuntimeRole::Engine
    )
    .then_some(SUSPICION_RECONCILER_CONNECTIONS);
    scans
        .saturating_add(provisioning)
        .saturating_add(owner_connections)
        .saturating_add(roster_access.unwrap_or_default())
        .saturating_add(account_lifecycle.unwrap_or_default())
        .saturating_add(runtime_transition.unwrap_or_default())
        .saturating_add(suspicion_reconciler.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{pool_options, process_application_name, required_pool_connections};
    use crate::models::internal::configs::RuntimeRole;

    #[test]
    fn connection_floor_accounts_for_nested_scan_provisioning_and_owner_work() {
        assert_eq!(required_pool_connections(1, 4, false, RuntimeRole::Web), 26);
        assert_eq!(
            required_pool_connections(1, 4, false, RuntimeRole::Development),
            28
        );
        assert_eq!(
            required_pool_connections(4, 4, false, RuntimeRole::Engine),
            31
        );
        assert_eq!(
            required_pool_connections(1, 4, false, RuntimeRole::Engine),
            16
        );
        assert_eq!(
            required_pool_connections(1, 4, false, RuntimeRole::Control),
            18
        );
        assert_eq!(required_pool_connections(1, 4, true, RuntimeRole::Web), 26);
        assert_eq!(
            required_pool_connections(1, 4, true, RuntimeRole::Control),
            21
        );
        assert_eq!(required_pool_connections(1, 4, false, RuntimeRole::All), 30);
        assert_eq!(required_pool_connections(1, 4, true, RuntimeRole::All), 33);
        assert_eq!(
            required_pool_connections(1, 4, false, RuntimeRole::Network),
            16
        );
        assert_eq!(
            required_pool_connections(1, 4, true, RuntimeRole::Network),
            19
        );
        assert_eq!(
            required_pool_connections(4, 16, true, RuntimeRole::Migrate),
            2
        );
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn reconciler_nested_checkout_progresses_at_its_exact_reserve() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let pool = pool_options(super::SUSPICION_RECONCILER_CONNECTIONS as u32)
            .connect(&database_url)
            .await
            .unwrap();
        let mut fence = pool.begin().await.unwrap();
        sqlx::query("SELECT pg_advisory_xact_lock(9089)")
            .execute(&mut *fence)
            .await
            .unwrap();
        let nested = tokio::time::timeout(
            Duration::from_secs(2),
            sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&pool),
        )
        .await
        .expect("the reserved nested checkout must not deadlock")
        .unwrap();
        assert_eq!(nested, 1);
        fence.rollback().await.unwrap();
        pool.close().await;
    }

    #[test]
    fn process_database_identity_is_bounded_versioned_and_unique() {
        let first = process_application_name(RuntimeRole::Migrate);
        let second = process_application_name(RuntimeRole::Migrate);
        assert_ne!(first, second);
        assert!(first.starts_with(&format!("rsctf:migrate:{}:", env!("CARGO_PKG_VERSION"))));
        assert!(first.len() <= super::POSTGRES_APPLICATION_NAME_MAX_BYTES);
    }
}
