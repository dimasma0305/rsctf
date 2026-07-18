//! Database connection helper.

use std::time::Duration;

use sea_orm::{DatabaseConnection, SqlxPostgresConnector};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{ConnectOptions as _, Connection as _};

use crate::models::internal::configs::RuntimeRole;

fn pool_options(max_connections: u32) -> PgPoolOptions {
    PgPoolOptions::new()
        .max_connections(max_connections)
        .min_connections(2)
        .acquire_timeout(Duration::from_secs(10))
        .idle_timeout(Some(Duration::from_secs(300)))
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
    // 32 is the sweet spot when the app + Postgres share a host (the load-test setup):
    // raising it to 64 pegged more Postgres backends onto the same saturated cores and
    // REGRESSED throughput ~16 % — the pool "saturating" at 32 was a symptom of a CPU-bound
    // host, not the bottleneck. A deployment with Postgres on its own box (spare DB CPU) can
    // raise it via RSCTF_DB_MAX_CONNECTIONS; the default stays conservative.
    let max_conns = std::env::var("RSCTF_DB_MAX_CONNECTIONS")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(32);
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
    let connect_options = url.parse::<PgConnectOptions>()?.disable_statement_logging();
    let pool = pool_options(max_conns)
        .connect_with(connect_options)
        .await?;
    Ok(SqlxPostgresConnector::from_sqlx_postgres_pool(pool))
}

/// Conservative no-deadlock floor for operations that retain pool connections
/// while awaiting nested work.
///
/// A checker-bearing repository scan can retain checkout, game-control, and
/// checker-publication locks while its model insert needs another checkout
/// (4R).
/// Provisioning can hold one advisory lock while issuing a query (2P). A
/// A network owner always retains the singleton BYOC ownership lease. When VPN
/// is enabled it also retains a PgListener and needs room for nested kernel
/// reconciliation. The one-shot migration role opens none of these paths and
/// needs only the pool's two baseline connections.
fn required_pool_connections(
    repo_scan_concurrency: usize,
    provisioning_concurrency: usize,
    vpn_enabled: bool,
    role: RuntimeRole,
) -> usize {
    if role == RuntimeRole::Migrate {
        return 2;
    }
    let scans = repo_scan_concurrency.saturating_mul(4);
    let provisioning = provisioning_concurrency.saturating_mul(2);
    let owner_connections = match (role.capabilities().network, vpn_enabled) {
        (true, true) => 6,
        // Network/BYOC ownership and traffic-capture ownership each retain a
        // session; keep one more checkout available for forward progress.
        (true, false) => 3,
        (false, _) => 1,
    };
    scans
        .saturating_add(provisioning)
        .saturating_add(owner_connections)
}

#[cfg(test)]
mod tests {
    use super::required_pool_connections;
    use crate::models::internal::configs::RuntimeRole;

    #[test]
    fn connection_floor_accounts_for_nested_scan_provisioning_and_owner_work() {
        assert_eq!(required_pool_connections(1, 4, false, RuntimeRole::Web), 13);
        assert_eq!(
            required_pool_connections(4, 4, false, RuntimeRole::Engine),
            25
        );
        assert_eq!(
            required_pool_connections(1, 4, false, RuntimeRole::Control),
            15
        );
        assert_eq!(required_pool_connections(1, 4, true, RuntimeRole::Web), 13);
        assert_eq!(
            required_pool_connections(1, 4, true, RuntimeRole::Control),
            18
        );
        assert_eq!(
            required_pool_connections(4, 16, true, RuntimeRole::Migrate),
            2
        );
    }
}
