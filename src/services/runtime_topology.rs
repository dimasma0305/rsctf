//! PostgreSQL-backed presence for cooperating single-binary runtime roles.
//!
//! This is a readiness signal, not a work-ownership mechanism. Round engines,
//! maintenance, and host networking retain their existing durable leases and
//! generation fences. Each long-running split role writes one small row every
//! five seconds; readiness consumes an in-memory snapshot refreshed by the same
//! task, so `/healthz` adds no database query of its own.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sqlx::PgPool;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::models::internal::configs::RuntimeRole;

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const DATABASE_IO_TIMEOUT: Duration = Duration::from_secs(4);
const FRESHNESS_SECONDS: i64 = 15;
const PRUNE_AFTER_SECONDS: i64 = 300;
const PRUNE_BATCH: i64 = 128;
const PRUNE_EVERY_TICKS: u8 = 12;
const UNREGISTER_TIMEOUT: Duration = Duration::from_secs(2);

const CONTROL_PRESENT: u8 = 1 << 0;
const ENGINE_PRESENT: u8 = 1 << 1;
const NETWORK_PRESENT: u8 = 1 << 2;
const INCOMPATIBLE_CONTROL_PRESENT: u8 = 1 << 3;
const INCOMPATIBLE_ENGINE_PRESENT: u8 = 1 << 4;
const INCOMPATIBLE_NETWORK_PRESENT: u8 = 1 << 5;

/// Exact source build plus the explicit role-coordination protocol revision.
///
/// Every architecture built from the same source tree has the same value. A
/// source or protocol change produces a different value, preventing a split
/// deployment from treating an older role as a compatible dependency.
pub const BUILD_PROTOCOL_FINGERPRINT: &str =
    concat!("runtime-role-v1:", env!("RSCTF_BUILD_SOURCE_SHA256"));

/// Cheap role-presence snapshot consumed by readiness.
///
/// An empty observation is intentionally the initial state. Roles which depend
/// on peers therefore cannot briefly become ready before their first successful
/// database observation.
#[derive(Clone)]
pub struct RuntimeTopologyProbe {
    inner: Arc<ProbeInner>,
}

struct ProbeInner {
    role: RuntimeRole,
    vpn_enabled: bool,
    observed: AtomicU8,
}

impl RuntimeTopologyProbe {
    pub fn new(role: RuntimeRole, vpn_enabled: bool) -> Self {
        Self {
            inner: Arc::new(ProbeInner {
                role,
                vpn_enabled,
                observed: AtomicU8::new(0),
            }),
        }
    }

    /// Whether every peer clause required by this role is currently present.
    /// `all`, `migrate`, `control`, and `network` are self-contained.
    pub fn is_ready(&self) -> bool {
        missing_requirement(
            self.inner.role,
            self.inner.vpn_enabled,
            self.inner.observed.load(Ordering::Acquire),
        )
        .is_none()
    }

    /// Stable operator-facing reason for a failed topology gate.
    pub fn unavailable_reason(&self) -> Option<&'static str> {
        missing_requirement(
            self.inner.role,
            self.inner.vpn_enabled,
            self.inner.observed.load(Ordering::Acquire),
        )
    }

    fn requires_peer_observation(&self) -> bool {
        self.inner.role == RuntimeRole::Web
            || (self.inner.role == RuntimeRole::Engine && self.inner.vpn_enabled)
    }

    fn observe(&self, presence: RolePresence) {
        self.inner
            .observed
            .store(presence.as_bits(), Ordering::Release);
    }

    fn observe_failure(&self) {
        // Never retain a peer indefinitely through a database error. The normal
        // dependency probe will also report PostgreSQL unavailable, while this
        // fail-closed snapshot covers role-query-specific failures.
        self.inner.observed.store(0, Ordering::Release);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, sqlx::FromRow)]
struct RolePresence {
    control_present: bool,
    engine_present: bool,
    network_present: bool,
    incompatible_control_present: bool,
    incompatible_engine_present: bool,
    incompatible_network_present: bool,
}

impl RolePresence {
    const fn as_bits(self) -> u8 {
        (if self.control_present {
            CONTROL_PRESENT
        } else {
            0
        }) | (if self.engine_present {
            ENGINE_PRESENT
        } else {
            0
        }) | (if self.network_present {
            NETWORK_PRESENT
        } else {
            0
        }) | (if self.incompatible_control_present {
            INCOMPATIBLE_CONTROL_PRESENT
        } else {
            0
        }) | (if self.incompatible_engine_present {
            INCOMPATIBLE_ENGINE_PRESENT
        } else {
            0
        }) | (if self.incompatible_network_present {
            INCOMPATIBLE_NETWORK_PRESENT
        } else {
            0
        })
    }
}

fn missing_requirement(role: RuntimeRole, vpn_enabled: bool, presence: u8) -> Option<&'static str> {
    let control = presence & CONTROL_PRESENT != 0;
    let engine = presence & ENGINE_PRESENT != 0;
    let network = presence & NETWORK_PRESENT != 0;
    let incompatible_control = presence & INCOMPATIBLE_CONTROL_PRESENT != 0;
    let incompatible_engine = presence & INCOMPATIBLE_ENGINE_PRESENT != 0;
    let incompatible_network = presence & INCOMPATIBLE_NETWORK_PRESENT != 0;

    match role {
        RuntimeRole::Web
            if !control && !engine && (incompatible_control || incompatible_engine) =>
        {
            Some("runtime role build mismatch")
        }
        RuntimeRole::Web if !control && !engine => Some("control or engine role unavailable"),
        RuntimeRole::Web
            if !control && !network && (incompatible_control || incompatible_network) =>
        {
            Some("runtime role build mismatch")
        }
        RuntimeRole::Web if !control && !network => Some("control or network role unavailable"),
        RuntimeRole::Engine
            if vpn_enabled
                && !control
                && !network
                && (incompatible_control || incompatible_network) =>
        {
            Some("runtime role build mismatch")
        }
        RuntimeRole::Engine if vpn_enabled && !control && !network => {
            Some("control or network role unavailable")
        }
        RuntimeRole::All
        | RuntimeRole::Web
        | RuntimeRole::Control
        | RuntimeRole::Engine
        | RuntimeRole::Network
        | RuntimeRole::Migrate => None,
    }
}

/// Spawn presence maintenance for a long-running split role.
///
/// `all` owns every capability in one process and `migrate` is one-shot, so
/// neither writes a heartbeat or has a topology task to supervise.
pub fn spawn(
    pool: PgPool,
    role: RuntimeRole,
    probe: RuntimeTopologyProbe,
    shutdown: watch::Receiver<bool>,
) -> Option<JoinHandle<anyhow::Result<()>>> {
    matches!(
        role,
        RuntimeRole::Web | RuntimeRole::Control | RuntimeRole::Engine | RuntimeRole::Network
    )
    .then(|| {
        tokio::spawn(async move {
            monitor(
                &pool,
                Uuid::new_v4(),
                role,
                &probe,
                shutdown,
                HEARTBEAT_INTERVAL,
            )
            .await
        })
    })
}

async fn monitor(
    pool: &PgPool,
    instance_id: Uuid,
    role: RuntimeRole,
    probe: &RuntimeTopologyProbe,
    mut shutdown: watch::Receiver<bool>,
    heartbeat_interval: Duration,
) -> anyhow::Result<()> {
    let initial = tokio::time::timeout(
        DATABASE_IO_TIMEOUT,
        heartbeat_and_observe(pool, instance_id, role, probe),
    )
    .await;
    let initial = match initial {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!("initial runtime role heartbeat timed out")),
    };
    if let Err(error) = initial {
        probe.observe_failure();
        unregister_bounded(pool, instance_id, role).await;
        return Err(error);
    }
    tracing::info!(%instance_id, %role, "registered runtime role heartbeat");

    let mut ticker = tokio::time::interval(heartbeat_interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // The initial registration above already performed this tick's write.
    ticker.reset();
    let mut ticks_until_prune = PRUNE_EVERY_TICKS;

    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    unregister_bounded(pool, instance_id, role).await;
                    return Ok(());
                }
            }
            _ = ticker.tick() => {
                match tokio::time::timeout(
                    DATABASE_IO_TIMEOUT,
                    heartbeat_and_observe(pool, instance_id, role, probe),
                ).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        probe.observe_failure();
                        tracing::warn!(%instance_id, %role, %error, "runtime role heartbeat failed");
                    }
                    Err(_) => {
                        probe.observe_failure();
                        tracing::warn!(
                            %instance_id,
                            %role,
                            "runtime role heartbeat timed out"
                        );
                    }
                }
                ticks_until_prune = ticks_until_prune.saturating_sub(1);
                if ticks_until_prune == 0 {
                    ticks_until_prune = PRUNE_EVERY_TICKS;
                    match tokio::time::timeout(DATABASE_IO_TIMEOUT, prune_stale(pool)).await {
                        Ok(Ok(_)) => {}
                        Ok(Err(error)) => {
                            tracing::warn!(%error, "stale runtime role heartbeat pruning failed");
                        }
                        Err(_) => {
                            tracing::warn!("stale runtime role heartbeat pruning timed out");
                        }
                    }
                }
            }
        }
    }
}

async fn heartbeat_and_observe(
    pool: &PgPool,
    instance_id: Uuid,
    role: RuntimeRole,
    probe: &RuntimeTopologyProbe,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO "RuntimeRoleHeartbeats"
            (instance_id, role, build_fingerprint, started_at_utc, heartbeat_at_utc)
        VALUES ($1, $2, $3, clock_timestamp(), clock_timestamp())
        ON CONFLICT (instance_id) DO UPDATE
          SET role = EXCLUDED.role,
              build_fingerprint = EXCLUDED.build_fingerprint,
              heartbeat_at_utc = clock_timestamp()
        "#,
    )
    .bind(instance_id)
    .bind(role.as_str())
    .bind(BUILD_PROTOCOL_FINGERPRINT)
    .execute(pool)
    .await?;

    if probe.requires_peer_observation() {
        let presence = load_presence(pool, FRESHNESS_SECONDS, BUILD_PROTOCOL_FINGERPRINT).await?;
        probe.observe(presence);
    }
    Ok(())
}

async fn load_presence(
    pool: &PgPool,
    freshness_seconds: i64,
    build_fingerprint: &str,
) -> Result<RolePresence, sqlx::Error> {
    sqlx::query_as::<_, RolePresence>(
        r#"
        SELECT COALESCE(bool_or(
                   role = 'control' AND build_fingerprint = $2
               ), FALSE) AS control_present,
               COALESCE(bool_or(
                   role = 'engine' AND build_fingerprint = $2
               ), FALSE) AS engine_present,
               COALESCE(bool_or(
                   role = 'network' AND build_fingerprint = $2
               ), FALSE) AS network_present,
               COALESCE(bool_or(
                   role = 'control' AND build_fingerprint <> $2
               ), FALSE) AS incompatible_control_present,
               COALESCE(bool_or(
                   role = 'engine' AND build_fingerprint <> $2
               ), FALSE) AS incompatible_engine_present,
               COALESCE(bool_or(
                   role = 'network' AND build_fingerprint <> $2
               ), FALSE) AS incompatible_network_present
          FROM "RuntimeRoleHeartbeats"
         WHERE heartbeat_at_utc >=
               clock_timestamp() - ($1 * interval '1 second')
        "#,
    )
    .bind(freshness_seconds)
    .bind(build_fingerprint)
    .fetch_one(pool)
    .await
}

async fn prune_stale(pool: &PgPool) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        r#"
        DELETE FROM "RuntimeRoleHeartbeats"
         WHERE instance_id IN (
             SELECT instance_id
               FROM "RuntimeRoleHeartbeats"
              WHERE heartbeat_at_utc <
                    clock_timestamp() - ($1 * interval '1 second')
              ORDER BY heartbeat_at_utc
              LIMIT $2
         )
        "#,
    )
    .bind(PRUNE_AFTER_SECONDS)
    .bind(PRUNE_BATCH)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

async fn unregister_bounded(pool: &PgPool, instance_id: Uuid, role: RuntimeRole) {
    let unregister = sqlx::query(r#"DELETE FROM "RuntimeRoleHeartbeats" WHERE instance_id = $1"#)
        .bind(instance_id)
        .execute(pool);
    match tokio::time::timeout(UNREGISTER_TIMEOUT, unregister).await {
        Ok(Ok(_)) => tracing::info!(%instance_id, %role, "unregistered runtime role heartbeat"),
        Ok(Err(error)) => {
            tracing::warn!(%instance_id, %role, %error, "runtime role unregister failed; freshness expiry will remove it")
        }
        Err(_) => tracing::warn!(
            %instance_id,
            %role,
            "runtime role unregister timed out; freshness expiry will remove it"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::PgPoolOptions;

    fn bits(control: bool, engine: bool, network: bool) -> u8 {
        RolePresence {
            control_present: control,
            engine_present: engine,
            network_present: network,
            ..RolePresence::default()
        }
        .as_bits()
    }

    fn incompatible_bits(control: bool, engine: bool, network: bool) -> u8 {
        RolePresence {
            incompatible_control_present: control,
            incompatible_engine_present: engine,
            incompatible_network_present: network,
            ..RolePresence::default()
        }
        .as_bits()
    }

    #[test]
    fn web_always_requires_compute_and_network_owners() {
        assert_eq!(
            missing_requirement(RuntimeRole::Web, false, 0),
            Some("control or engine role unavailable")
        );
        assert_eq!(
            missing_requirement(RuntimeRole::Web, false, bits(false, true, false)),
            Some("control or network role unavailable")
        );
        assert!(missing_requirement(RuntimeRole::Web, false, bits(false, true, true)).is_none());
        assert!(missing_requirement(RuntimeRole::Web, false, bits(true, false, false)).is_none());
        assert!(missing_requirement(RuntimeRole::Web, true, bits(false, true, false)).is_some());
        assert!(missing_requirement(RuntimeRole::Web, true, bits(false, true, true)).is_none());
        assert!(missing_requirement(RuntimeRole::Web, true, bits(true, false, false)).is_none());
    }

    #[test]
    fn engine_only_needs_a_vpn_owner_when_vpn_is_enabled() {
        assert!(missing_requirement(RuntimeRole::Engine, false, 0).is_none());
        assert!(missing_requirement(RuntimeRole::Engine, true, 0).is_some());
        assert!(missing_requirement(RuntimeRole::Engine, true, bits(true, false, false)).is_none());
        assert!(missing_requirement(RuntimeRole::Engine, true, bits(false, false, true)).is_none());
    }

    #[test]
    fn incompatible_required_roles_have_a_distinct_readiness_reason() {
        assert_eq!(
            missing_requirement(
                RuntimeRole::Web,
                false,
                incompatible_bits(true, false, false)
            ),
            Some("runtime role build mismatch")
        );
        assert_eq!(
            missing_requirement(
                RuntimeRole::Web,
                false,
                bits(false, true, false) | incompatible_bits(false, false, true)
            ),
            Some("runtime role build mismatch")
        );
        assert_eq!(
            missing_requirement(
                RuntimeRole::Engine,
                true,
                incompatible_bits(false, false, true)
            ),
            Some("runtime role build mismatch")
        );
    }

    #[test]
    fn build_protocol_fingerprint_is_content_addressed() {
        let (protocol, source_hash) = BUILD_PROTOCOL_FINGERPRINT
            .split_once(':')
            .expect("protocol and source hash");
        assert_eq!(protocol, "runtime-role-v1");
        assert_eq!(source_hash.len(), 64);
        assert!(source_hash.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn combined_and_owner_roles_are_self_contained() {
        for role in [
            RuntimeRole::All,
            RuntimeRole::Control,
            RuntimeRole::Network,
            RuntimeRole::Migrate,
        ] {
            assert!(missing_requirement(role, true, 0).is_none());
        }
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn heartbeats_expire_and_unregister_without_unbounded_cleanup() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("connect test database");
        let schema = format!("runtime_topology_{}", Uuid::new_v4().simple());
        let setup = format!(
            r#"
            CREATE SCHEMA "{schema}";
            CREATE TABLE "{schema}"."RuntimeRoleHeartbeats" (
                instance_id UUID PRIMARY KEY,
                role TEXT NOT NULL,
                build_fingerprint TEXT NOT NULL DEFAULT 'legacy',
                started_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                heartbeat_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
            );
            "#
        );
        sqlx::raw_sql(&setup).execute(&admin).await.unwrap();

        let search_path_schema = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .after_connect(move |connection, _metadata| {
                let statement = format!(r#"SET search_path TO "{search_path_schema}""#);
                Box::pin(async move {
                    sqlx::query(&statement).execute(connection).await?;
                    Ok(())
                })
            })
            .connect(&database_url)
            .await
            .unwrap();

        let incompatible_control_id = Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO "RuntimeRoleHeartbeats"
                 (instance_id, role, build_fingerprint)
               VALUES ($1, 'control', 'legacy')"#,
        )
        .bind(incompatible_control_id)
        .execute(&pool)
        .await
        .unwrap();

        let web_probe = RuntimeTopologyProbe::new(RuntimeRole::Web, true);
        let web_id = Uuid::new_v4();
        heartbeat_and_observe(&pool, web_id, RuntimeRole::Web, &web_probe)
            .await
            .unwrap();
        assert_eq!(
            web_probe.unavailable_reason(),
            Some("runtime role build mismatch")
        );

        let compatible_control_id = Uuid::new_v4();
        let control_probe = RuntimeTopologyProbe::new(RuntimeRole::Control, true);
        heartbeat_and_observe(
            &pool,
            compatible_control_id,
            RuntimeRole::Control,
            &control_probe,
        )
        .await
        .unwrap();
        heartbeat_and_observe(&pool, web_id, RuntimeRole::Web, &web_probe)
            .await
            .unwrap();
        assert!(web_probe.is_ready());

        sqlx::query(
            r#"UPDATE "RuntimeRoleHeartbeats"
                  SET heartbeat_at_utc = clock_timestamp() - interval '1 hour'
                WHERE instance_id = $1"#,
        )
        .bind(compatible_control_id)
        .execute(&pool)
        .await
        .unwrap();
        web_probe.observe(
            load_presence(&pool, FRESHNESS_SECONDS, BUILD_PROTOCOL_FINGERPRINT)
                .await
                .unwrap(),
        );
        assert!(!web_probe.is_ready());
        assert_eq!(
            web_probe.unavailable_reason(),
            Some("runtime role build mismatch")
        );

        sqlx::query(
            r#"UPDATE "RuntimeRoleHeartbeats"
                  SET heartbeat_at_utc = clock_timestamp() - interval '1 hour'
                WHERE instance_id = $1"#,
        )
        .bind(incompatible_control_id)
        .execute(&pool)
        .await
        .unwrap();
        web_probe.observe(
            load_presence(&pool, FRESHNESS_SECONDS, BUILD_PROTOCOL_FINGERPRINT)
                .await
                .unwrap(),
        );
        assert_eq!(
            web_probe.unavailable_reason(),
            Some("control or engine role unavailable")
        );
        assert_eq!(prune_stale(&pool).await.unwrap(), 2);

        unregister_bounded(&pool, web_id, RuntimeRole::Web).await;
        let remaining: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM "RuntimeRoleHeartbeats""#)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(remaining, 0);

        pool.close().await;
        let cleanup = format!(r#"DROP SCHEMA "{schema}" CASCADE"#);
        sqlx::query(&cleanup).execute(&admin).await.unwrap();
    }
}
