//! Ported from RSCTF `Program.cs` — the rsctf entry point. Builds the
//! composition root (`AppState`), runs migrations, and serves the API.

// argon2 (password hashing) is memory-hard — ~19 MiB per hash. glibc malloc keeps those
// large freed chunks in its arenas instead of returning them to the OS, so a register/
// login flood ratchets RSS up unboundedly (1.2 → 8+ GiB observed, never released).
// mimalloc returns freed memory to the OS, keeping the hash memory bounded.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::sync::Arc;
use std::time::Duration;

use tracing_subscriber::EnvFilter;

use rsctf::app_state::{AppState, SharedState};
use rsctf::extensions;
use rsctf::migrations::{Migrator, MigratorTrait};
use rsctf::models::internal::configs::{AppConfig, RuntimeRole};
use rsctf::server;
use rsctf::services::cache::{Cache, InMemoryCache, RedisCache, TieredCache};
use rsctf::services::event_bus::EventBus;
use rsctf::services::token::TokenService;

const HTTP_DEREGISTRATION_DELAY: Duration = Duration::from_secs(5);
const HTTP_DRAIN_TIMEOUT: Duration = Duration::from_secs(25);

fn install_tls_crypto_provider() -> anyhow::Result<()> {
    tokio_rustls::rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("failed to install the process-wide AWS-LC TLS provider"))
}

fn main() -> anyhow::Result<()> {
    // Sandbox-launcher dispatch — MUST be the very first thing, before any thread
    // exists: when rsctf re-execs itself to run a checker (`__checker_sandbox …`),
    // this confines the process + execs the checker and never returns. Running it
    // pre-runtime keeps the Landlock/seccomp rule-building single-threaded/safe.
    if std::env::args().nth(1).as_deref() == Some(rsctf::services::ad_engine::sandbox::LAUNCH_ARG) {
        rsctf::services::ad_engine::sandbox::launcher_main();
    }
    if std::env::args().nth(1).as_deref()
        == Some(rsctf::services::ad_engine::sandbox::PREFLIGHT_ARG)
    {
        rsctf::services::ad_engine::sandbox::confinement_probe_main();
    }
    install_tls_crypto_provider()?;
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async_main())
}

#[cfg(test)]
mod startup_tests {
    use super::*;

    #[test]
    fn installs_an_explicit_tls_crypto_provider() {
        install_tls_crypto_provider().unwrap();
        assert!(tokio_rustls::rustls::crypto::CryptoProvider::get_default().is_some());
    }
}

async fn async_main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config = Arc::new(AppConfig::from_env());
    config.validate_runtime_role()?;
    let role = config.runtime_role;
    let capabilities = role.capabilities();
    if role != RuntimeRole::Migrate {
        config.validate()?;
        rsctf::services::anti_cheat::validate_trusted_proxy_config()?;
    }
    tracing::info!(
        bind = %config.bind_addr,
        %role,
        capabilities = role.capability_header(),
        "starting rsctf"
    );

    // --- database ---
    let db = extensions::database::connect(&config.database_url).await?;
    let run_migrations = role == RuntimeRole::Migrate
        || (role == RuntimeRole::All && std::env::var("RSCTF_MIGRATE").as_deref() != Ok("0"));
    if run_migrations {
        tracing::info!("applying migrations");
        Migrator::up(&db, None).await?;
    } else if role != RuntimeRole::All {
        // Split roles are never migration owners. Refuse to start against a
        // stale or newer ledger so they cannot become ready with code/schema
        // skew while an operator has skipped (or is still running) migrate.
        rsctf::migrations::ensure_schema_current(&db).await?;
    }
    // Bootstrap repairs are owned by one combined/control process or the
    // migration job. Active-active engine replicas must not race legacy
    // read-then-insert backfills during simultaneous startup.
    if matches!(
        role,
        RuntimeRole::All | RuntimeRole::Control | RuntimeRole::Migrate
    ) {
        let _ = rsctf::services::suspicion::seed_default_rules(&db).await;
        let _ = rsctf::controllers::edit::backfill_build_records(&db).await;
    }
    if role == RuntimeRole::Migrate {
        tracing::info!("migration role completed");
        return Ok(());
    }

    let worker_store =
        rsctf::services::worker_store::WorkerStore::new(db.get_postgres_connection_pool().clone());
    let worker_plane = if capabilities.network {
        rsctf::services::worker::bind_from_env(worker_store.clone()).await?
    } else {
        None
    };

    // Narrow runtime roles are intended to cooperate as replicas. Requiring
    // Redis prevents them from silently falling back to process-local cache,
    // event fanout, leader leases, or rate limits.
    let replica_mode = role != RuntimeRole::All;
    if replica_mode && config.redis_url.is_none() {
        anyhow::bail!("RSCTF_REDIS_URL is required when RSCTF_ROLE={role}");
    }
    if replica_mode
        && !std::env::var("RSCTF_SHARED_STORAGE")
            .map(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false)
    {
        anyhow::bail!(
            "RSCTF_SHARED_STORAGE=true is required when RSCTF_ROLE={role}; every replica must mount the same RSCTF_STORAGE_ROOT for repositories, checkers, and captures"
        );
    }

    // --- cache ---
    let cache: Arc<dyn Cache> = match &config.redis_url {
        Some(url) => match RedisCache::connect(url).await {
            Ok(c) => {
                tracing::info!("connected to redis");
                // In-process L1 (1s) over Redis: hot cached reads (the scoreboard is
                // polled thousands of times/sec) are served from memory instead of a
                // Redis round-trip, matching RSCTF's in-process cache tier. 1s bounds
                // staleness well under the scoreboard's own 5s cache TTL.
                Arc::new(TieredCache::new(
                    Arc::new(c),
                    std::time::Duration::from_secs(1),
                ))
            }
            Err(e) if replica_mode => {
                return Err(anyhow::anyhow!(
                    "Redis is required for RSCTF_ROLE={role}, but startup failed: {e}"
                ));
            }
            Err(e) => {
                tracing::warn!(error = %e, "redis unavailable; readiness will remain down while reconnecting");
                Arc::new(TieredCache::new(
                    Arc::new(RedisCache::disconnected(url)?),
                    std::time::Duration::from_secs(1),
                ))
            }
        },
        None => Arc::new(InMemoryCache::new()),
    };

    // Multi-node API roles always use a shared limiter: allowing a silent local
    // fallback would multiply every quota by the number of replicas. `all`
    // keeps the historical opt-in switch and local fast path.
    let distributed_rate_limit = capabilities.api
        && (replica_mode
            || std::env::var("RSCTF_DISTRIBUTED_RATELIMIT")
                .map(|v| matches!(v.as_str(), "1" | "true" | "yes"))
                .unwrap_or(false));
    if distributed_rate_limit {
        match &config.redis_url {
            Some(url) => {
                if replica_mode {
                    rsctf::middlewares::rate_limiter::init_distributed(url)
                        .await
                        .map_err(|error| {
                            anyhow::anyhow!("distributed rate limiter startup failed: {error}")
                        })?;
                } else if let Err(e) =
                    rsctf::middlewares::rate_limiter::init_distributed(url).await
                {
                    tracing::warn!(error = %e, "distributed rate limiter unavailable, using in-process");
                }
            }
            None => tracing::warn!(
                "RSCTF_DISTRIBUTED_RATELIMIT set but RSCTF_REDIS_URL is unset — using in-process limiter"
            ),
        }
    }

    // --- storage & auth ---
    let storage = rsctf::storage::from_env(config.storage_root.clone())?;
    tokio::time::timeout(Duration::from_secs(5), storage.health())
        .await
        .map_err(|_| anyhow::anyhow!("storage backend health check timed out"))?
        .map_err(|error| anyhow::anyhow!("storage backend is unavailable: {error}"))?;
    let token = TokenService::new(&config.jwt_secret, config.jwt_ttl_secs);

    // Resolve the backend once. VPN deployments must choose explicitly because
    // silently falling back would make service routes disagree with placement.
    let backend_mode = std::env::var("RSCTF_CONTAINER_BACKEND")
        .unwrap_or_else(|_| "auto".to_string())
        .trim()
        .to_ascii_lowercase();
    if rsctf::services::ad_vpn::enabled() && backend_mode == "auto" {
        return Err(anyhow::anyhow!(
            "RSCTF_CONTAINER_BACKEND=docker or kubernetes is required when RSCTF_AD_VPN_ENABLED=true"
        ));
    }
    let containers: Arc<dyn rsctf::services::container::ContainerManager> = match backend_mode
        .as_str()
    {
        "docker" => rsctf::services::container::from_env_required()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?,
        "kubernetes" => rsctf::services::k8s::from_env().ok_or_else(|| {
            anyhow::anyhow!(
                "RSCTF_CONTAINER_BACKEND=kubernetes but the Kubernetes API is unreachable"
            )
        })?,
        "worker" => {
            let worker = rsctf::services::worker::WorkerContainerManager::new(worker_store.clone());
            let local_mode = std::env::var("RSCTF_WORKER_LOCAL_BACKEND")
                .unwrap_or_else(|_| "none".to_string())
                .trim()
                .to_ascii_lowercase();
            let local: Option<
                Arc<dyn rsctf::services::container::ContainerManager>,
            > = match local_mode.as_str() {
                "none" => None,
                "docker" => Some(
                    rsctf::services::container::from_env_required()
                        .map_err(|error| anyhow::anyhow!(error.to_string()))?,
                ),
                "kubernetes" => Some(rsctf::services::k8s::from_env().ok_or_else(|| {
                    anyhow::anyhow!(
                        "RSCTF_WORKER_LOCAL_BACKEND=kubernetes but the Kubernetes API is unreachable"
                    )
                })?),
                "auto" => {
                    let candidate = rsctf::services::k8s::from_env()
                        .unwrap_or_else(rsctf::services::container::from_env);
                    (candidate.backend_kind()
                        != rsctf::services::container::ContainerBackendKind::None)
                        .then_some(candidate)
                }
                value => {
                    return Err(anyhow::anyhow!(
                        "invalid RSCTF_WORKER_LOCAL_BACKEND {value:?}; expected none, auto, docker, or kubernetes"
                    ));
                }
            };
            match local {
                Some(local) => Arc::new(
                    rsctf::services::worker::HybridWorkerContainerManager::new(local, worker)
                        .map_err(|error| anyhow::anyhow!(error.to_string()))?,
                ),
                None => Arc::new(worker),
            }
        }
        "none" => Arc::new(rsctf::services::container::NoopContainerManager),
        "auto" => {
            rsctf::services::k8s::from_env().unwrap_or_else(rsctf::services::container::from_env)
        }
        value => {
            return Err(anyhow::anyhow!(
                "invalid RSCTF_CONTAINER_BACKEND {value:?}; expected auto, docker, kubernetes, worker, or none"
            ));
        }
    };
    rsctf::services::ad_vpn::initialize_backend(containers.backend_kind())
        .map_err(|error| anyhow::anyhow!("A&D VPN backend configuration failed: {error}"))?;
    if capabilities.network {
        // Network ownership is intentionally single-active for now, including
        // BYOC when the optional WireGuard VPN is disabled. A second owner
        // fails startup instead of serving a divergent in-memory tunnel registry.
        rsctf::services::ad_vpn::acquire_instance_lease(&db)
            .await
            .map_err(|error| anyhow::anyhow!("network singleton lease failed: {error}"))?;
        rsctf::services::ad_vpn::start_instance_lease_monitor();
    }

    let events = if replica_mode {
        EventBus::distributed(
            config
                .redis_url
                .as_deref()
                .expect("replica roles require Redis"),
        )?
    } else {
        EventBus::local()
    };
    let worker_issuer = if capabilities.network {
        rsctf::services::worker_pki::WorkerIssuer::from_env()?
    } else {
        None
    };
    if capabilities.network && worker_plane.is_some() != worker_issuer.is_some() {
        return Err(anyhow::anyhow!(
            "the trusted worker listener and enrollment issuer must be configured together"
        ));
    }
    if capabilities.network && containers.supports_worker_workloads() && worker_plane.is_none() {
        return Err(anyhow::anyhow!(
            "RSCTF_CONTAINER_BACKEND=worker requires the trusted worker listener and PKI configuration on the network owner"
        ));
    }
    let workers = worker_plane.as_ref().map(|plane| plane.service.clone());
    let state: SharedState = AppState::new_with_events_and_worker_issuer(
        db,
        containers,
        config.clone(),
        cache,
        storage,
        token,
        events,
        worker_issuer,
        workers,
    );

    // Start the background scheduler (container reaping, scoreboard flush, A&D
    // round advancement) — RSCTF's hosted cron services.
    // A&D VPN: stand up the services network + bring the wg0 hub up with the
    // persisted peer set on boot. This is fail-closed: serving with an active
    // tunnel but without its forwarding/input firewall would expose the control
    // plane through the app's other network interfaces.
    // Run before the cron so the checker can join the network on its first tick.
    if capabilities.network {
        use rsctf::services::ad_vpn;
        if state.containers.backend_kind()
            == rsctf::services::container::ContainerBackendKind::Docker
        {
            state
                .containers
                .ensure_network(&ad_vpn::services_network(), &ad_vpn::services_cidr())
                .await
                .map_err(|e| anyhow::anyhow!("A&D services network isolation check failed: {e}"))?;
        }
        // If no capture owner holds its advisory session, invalidate any
        // crash-left durable health before this process builds its first VPN
        // policy. A genuinely independent live owner is left untouched.
        rsctf::services::traffic::fence_unowned_capture_owner(state.pg())
            .await
            .map_err(|error| anyhow::anyhow!("traffic capture startup fence failed: {error}"))?;
        let cleared_relays = ad_vpn::clear_stale_local_relays(&state.db).await?;
        if cleared_relays > 0 {
            tracing::info!(
                cleared_relays,
                "cleared stale BYOC relay endpoints from a prior process"
            );
        }
        ad_vpn::reconcile_for_deployment(&state.db)
            .await
            .map_err(|error| anyhow::anyhow!("A&D VPN security initialization failed: {error}"))?;
    }

    // Older repository imports predate automatic `provide:`/`dist/` packaging.
    // Run this potentially unbounded backfill only after stale VPN/firewall
    // capabilities have been revoked during security initialization.
    if capabilities.maintenance {
        match rsctf::services::git_sync::repair_missing_attachments(&state).await {
            Ok(count) if count > 0 => {
                tracing::info!(count, "repaired missing repository challenge attachments")
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "repository attachment repair failed"),
        }
    }

    // Deny-by-default egress for the sandboxed-checker uid (see ad_engine::sandbox).
    if capabilities.round_engine {
        rsctf::services::ad_engine::sandbox::setup_checker_egress()
            .map_err(|error| anyhow::anyhow!("checker egress isolation failed: {error}"))?;
        rsctf::services::ad_engine::sandbox::preflight_checker_confinement()
            .await
            .map_err(|error| anyhow::anyhow!("checker process confinement failed: {error}"))?;
    }

    let app = match role {
        RuntimeRole::All => server::build_router(state.clone()),
        RuntimeRole::Web => server::build_web_router(state.clone()),
        RuntimeRole::Control | RuntimeRole::Network => server::build_stateful_router(state.clone()),
        RuntimeRole::Engine | RuntimeRole::Migrate => server::build_health_router(state.clone()),
    };

    // Secure the listen socket before registering topology heartbeats or
    // starting any worker. If bind fails, this replica never advertises itself
    // and no detached background task can briefly claim work for a process
    // that is about to exit.
    let listener = tokio::net::TcpListener::bind(&config.bind_addr).await?;
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let mut background = start_background_services(&state, role, shutdown_rx.clone(), worker_plane);

    state.readiness.mark_ready();
    tracing::info!(%role, "listening on {}", config.bind_addr);
    let mut server_shutdown = shutdown_rx;
    let mut server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            while !*server_shutdown.borrow() {
                if server_shutdown.changed().await.is_err() {
                    break;
                }
            }
            // Readiness was dropped before the watch update. Keep accepting
            // during a short endpoint-propagation window so an ingress with a
            // stale endpoint can still complete its request, while workers have
            // already stopped claiming new background work.
            tokio::time::sleep(HTTP_DEREGISTRATION_DELAY).await;
        })
        .await
    });

    let stop_reason = tokio::select! {
        result = &mut server => StopReason::Server(result),
        _ = wait_for_shutdown_signal() => {
            StopReason::Signal
        }
        failure = background.failures.recv() => {
            StopReason::RequiredWorker(failure.unwrap_or_else(|| {
                "required worker supervision ended unexpectedly".to_string()
            }))
        }
    };

    // Also stop workers if the listener exits for a reason other than an OS
    // signal. Long-running round work is fenced durably and can be recovered by
    // another replica if the bounded drain expires.
    state.readiness.begin_shutdown();
    let _ = shutdown_tx.send(true);
    let (server_finished, worker_failure) = match stop_reason {
        StopReason::Server(result) => (Some(result), None),
        StopReason::Signal => {
            tracing::info!("shutdown signal received; draining HTTP and background workers");
            (None, None)
        }
        StopReason::RequiredWorker(failure) => {
            tracing::error!(%failure, "required background worker exited; terminating replica");
            (None, Some(failure))
        }
    };
    let serve_result = match server_finished {
        Some(result) => result,
        None => match tokio::time::timeout(HTTP_DRAIN_TIMEOUT, &mut server).await {
            Ok(result) => result,
            Err(_) => {
                // SignalR and BYOC WebSockets may be intentionally long-lived.
                // Do not let them prevent a replica from leaving the topology.
                tracing::warn!(
                    "HTTP deregistration/drain timed out; closing remaining connections"
                );
                server.abort();
                let _ = server.await;
                Ok(Ok(()))
            }
        },
    };
    drain_background_services(background, role).await;
    match rsctf::services::ad_engine::abandon_process_round_finishes(&state.db).await {
        Ok(released) if released > 0 => tracing::info!(
            released,
            "released this replica's interrupted round pipeline lease(s)"
        ),
        Ok(_) => {}
        Err(error) => tracing::warn!(
            %error,
            "failed to release this replica's interrupted round pipeline leases"
        ),
    }
    if capabilities.network {
        if let Err(error) = rsctf::services::ad_vpn::release_instance_lease().await {
            tracing::warn!(%error, "network singleton lease release was not clean");
        }
    }
    serve_result.map_err(|error| anyhow::anyhow!("HTTP server task failed: {error}"))??;
    if let Some(failure) = worker_failure {
        anyhow::bail!(failure);
    }
    Ok(())
}

enum StopReason {
    Server(Result<Result<(), std::io::Error>, tokio::task::JoinError>),
    Signal,
    RequiredWorker(String),
}

enum RequiredTask {
    Unit(&'static str, tokio::task::JoinHandle<()>),
    Fallible(&'static str, tokio::task::JoinHandle<anyhow::Result<()>>),
}

/// A supervised worker must not become detached when its supervisor is
/// cancelled during a bounded drain. Tokio normally detaches a task when its
/// `JoinHandle` is dropped; this guard propagates cancellation to the worker.
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> AbortOnDrop<T> {
    async fn join(&mut self) -> Result<T, tokio::task::JoinError> {
        (&mut self.0).await
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct BackgroundServices {
    required: Vec<tokio::task::JoinHandle<()>>,
    optional: Vec<tokio::task::JoinHandle<()>>,
    failures: tokio::sync::mpsc::UnboundedReceiver<String>,
}

fn start_background_services(
    state: &SharedState,
    role: RuntimeRole,
    shutdown: tokio::sync::watch::Receiver<bool>,
    worker_plane: Option<rsctf::services::worker::BoundWorkerPlane>,
) -> BackgroundServices {
    use rsctf::services::cron::{self, RoundSchedulerScope};

    let mut required = Vec::new();
    if let Some(worker_plane) = worker_plane {
        let service = worker_plane.service.clone();
        required.push(RequiredTask::Fallible(
            "trusted worker listener",
            worker_plane.start(shutdown.clone()),
        ));
        required.push(RequiredTask::Unit(
            "trusted worker reconciler",
            rsctf::services::worker::start_reconciler(
                state.worker_store.clone(),
                service,
                shutdown.clone(),
            ),
        ));
    }
    match role {
        RuntimeRole::All | RuntimeRole::Control => {
            if rsctf::services::ad_vpn::enabled() {
                required.push(RequiredTask::Unit(
                    "A&D network reconcile",
                    cron::start_network_reconcile(state.clone(), shutdown.clone()),
                ));
            }
            required.push(RequiredTask::Unit(
                "traffic-capture-reconciler",
                rsctf::services::traffic::start_capture_reconciler(state.clone(), shutdown.clone()),
            ));
            required.push(RequiredTask::Unit(
                "maintenance scheduler",
                cron::start_maintenance(state.clone(), shutdown.clone()),
            ));
            required.push(RequiredTask::Unit(
                "round scheduler",
                cron::start_round_scheduler(
                    state.clone(),
                    RoundSchedulerScope::All,
                    shutdown.clone(),
                ),
            ));
            required.push(RequiredTask::Unit(
                "BYOC control listener",
                rsctf::services::byoc_tunnel::start_control_listener(
                    state.clone(),
                    shutdown.clone(),
                ),
            ));
        }
        RuntimeRole::Engine => {
            required.push(RequiredTask::Unit(
                "maintenance scheduler",
                cron::start_maintenance(state.clone(), shutdown.clone()),
            ));
            required.push(RequiredTask::Unit(
                "managed round scheduler",
                cron::start_round_scheduler(
                    state.clone(),
                    RoundSchedulerScope::ManagedOnly,
                    shutdown.clone(),
                ),
            ));
        }
        RuntimeRole::Network => {
            if rsctf::services::ad_vpn::enabled() {
                required.push(RequiredTask::Unit(
                    "A&D network reconcile",
                    cron::start_network_reconcile(state.clone(), shutdown.clone()),
                ));
            }
            required.push(RequiredTask::Unit(
                "traffic-capture-reconciler",
                rsctf::services::traffic::start_capture_reconciler(state.clone(), shutdown.clone()),
            ));
            required.push(RequiredTask::Unit(
                "network-bound round scheduler",
                cron::start_round_scheduler(
                    state.clone(),
                    RoundSchedulerScope::NetworkBoundOnly,
                    shutdown.clone(),
                ),
            ));
            required.push(RequiredTask::Unit(
                "BYOC control listener",
                rsctf::services::byoc_tunnel::start_control_listener(
                    state.clone(),
                    shutdown.clone(),
                ),
            ));
        }
        RuntimeRole::Web | RuntimeRole::Migrate => {}
    }

    if let Some(topology) = rsctf::services::runtime_topology::spawn(
        state.pg().clone(),
        role,
        state.topology.clone(),
        shutdown.clone(),
    ) {
        required.push(RequiredTask::Fallible(
            "runtime topology heartbeat",
            topology,
        ));
    }

    let mut optional = Vec::new();
    if role.capabilities().api {
        optional.push(rsctf::middlewares::user_activity::start_writer(
            state,
            shutdown.clone(),
        ));
    }
    if role.capabilities().network {
        optional.extend(rsctf::services::honeypot_listener::start(
            state.clone(),
            shutdown.clone(),
        ));
        optional.push(rsctf::services::ad_ssh::start(
            state.clone(),
            shutdown.clone(),
        ));
    }

    supervise_background_tasks(required, optional, shutdown)
}

fn supervise_background_tasks(
    required: Vec<RequiredTask>,
    optional: Vec<tokio::task::JoinHandle<()>>,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> BackgroundServices {
    let (failure_tx, failures) = tokio::sync::mpsc::unbounded_channel();
    let tasks = required
        .into_iter()
        .map(|task| {
            let failure_tx = failure_tx.clone();
            let shutdown = shutdown.clone();
            tokio::spawn(async move {
                let (name, outcome) = match task {
                    RequiredTask::Unit(name, task) => {
                        let mut task = AbortOnDrop(task);
                        let outcome = task
                            .join()
                            .await
                            .map(|()| "exited unexpectedly".to_string())
                            .unwrap_or_else(|error| format!("task failed: {error}"));
                        (name, outcome)
                    }
                    RequiredTask::Fallible(name, task) => {
                        let mut task = AbortOnDrop(task);
                        let outcome = match task.join().await {
                            Ok(Ok(())) => "exited unexpectedly".to_string(),
                            Ok(Err(error)) => format!("failed: {error}"),
                            Err(error) => format!("task failed: {error}"),
                        };
                        (name, outcome)
                    }
                };
                if !*shutdown.borrow() {
                    let _ = failure_tx.send(format!("{name} {outcome}"));
                }
            })
        })
        .collect();
    drop(failure_tx);
    BackgroundServices {
        required: tasks,
        optional,
        failures,
    }
}

async fn drain_background_services(mut background: BackgroundServices, role: RuntimeRole) {
    // A round pipeline may legitimately own work for up to 240 seconds. Stop
    // new claims immediately through the watch channel, then let the in-flight
    // owner finish before another replica needs to recover its durable lease.
    let drain_timeout = match role {
        // A managed checker pass can legitimately consume the full 240-second
        // budget. The monolith keeps its historical graceful drain as well.
        RuntimeRole::All | RuntimeRole::Engine => Duration::from_secs(250),
        // A split network owner must release its singleton lease promptly so a
        // replacement can take over. Durable round leases fence aborted work.
        RuntimeRole::Web | RuntimeRole::Control | RuntimeRole::Network | RuntimeRole::Migrate => {
            Duration::from_secs(30)
        }
    };

    let drain = async {
        futures::future::join_all(background.optional.iter_mut()).await;
        futures::future::join_all(background.required.iter_mut()).await;
    };
    if tokio::time::timeout(drain_timeout, drain).await.is_err() {
        tracing::warn!("background drain timed out; aborting remaining workers");
        for task in background.optional.iter().chain(background.required.iter()) {
            task.abort();
        }
        // `abort` schedules cancellation. Await every handle so listener
        // sockets and supervised inner tasks are actually dropped before the
        // network singleton lease is explicitly released by the caller.
        futures::future::join_all(background.optional).await;
        futures::future::join_all(background.required).await;
    }
}

async fn wait_for_shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "failed to install Ctrl-C handler");
        }
    };

    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let terminate = async {
            match signal(SignalKind::terminate()) {
                Ok(mut signal) => {
                    signal.recv().await;
                }
                Err(error) => {
                    tracing::error!(%error, "failed to install SIGTERM handler");
                    std::future::pending::<()>().await;
                }
            }
        };
        tokio::select! {
            _ = ctrl_c => {}
            _ = terminate => {}
        }
    }

    #[cfg(not(unix))]
    ctrl_c.await;
}
