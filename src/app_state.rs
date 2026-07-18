//! Shared application state — the composition root that RSCTF expressed via
//! ASP.NET dependency injection. Every router is `Router<SharedState>` and
//! handlers pull dependencies from here via `State<SharedState>`.

use std::sync::Arc;

use crate::models::internal::configs::AppConfig;
use crate::services::cache::Cache;
use crate::services::container::ContainerManager;
use crate::services::event_bus::EventBus;
use crate::services::health::ReadinessProbe;
use crate::services::runtime_topology::RuntimeTopologyProbe;
use crate::services::token::TokenService;
use crate::storage::BlobStorage;
use sea_orm::DatabaseConnection;

pub struct AppState {
    pub db: DatabaseConnection,
    pub config: Arc<AppConfig>,
    pub cache: Arc<dyn Cache>,
    pub storage: Arc<dyn BlobStorage>,
    pub token: TokenService,
    pub containers: Arc<dyn ContainerManager>,
    /// Bounded, short-lived dependency readiness cache for `/healthz`.
    pub readiness: ReadinessProbe,
    /// In-memory snapshot of fresh cooperating runtime roles. The background
    /// heartbeat task updates it; health checks never query PostgreSQL twice.
    pub topology: RuntimeTopologyProbe,
    /// Real-time event bus — the SignalR-hub replacement. Handlers publish
    /// [`HubEvent`]s tagged with a client hub method (`target`) and an optional
    /// game id; each WebSocket hub subscribes and forwards only the targets it
    /// serves, filtered to its connection's game (see `hubs::signalr::serve`).
    pub events: EventBus,
    /// Live BYOC agent tunnels, keyed by `(participation, challenge)` — the
    /// in-process replacement for RSCTF's relay container.
    pub byoc: Arc<crate::services::byoc_tunnel::Registry>,
    /// Optional CA signer used only by the one-time trusted-worker enrollment
    /// endpoint. Ordinary API/web replicas never need the private key.
    pub worker_issuer: Option<Arc<crate::services::worker_pki::WorkerIssuer>>,
    /// Live worker control/data sessions on the singleton network owner.
    /// PostgreSQL remains authoritative; this registry is never replicated.
    pub workers: Option<Arc<crate::services::worker::WorkerService>>,
    /// Durable worker identities, sessions, placements and desired/observed
    /// workload state. Cheap clones share the application's PostgreSQL pool.
    pub worker_store: crate::services::worker_store::WorkerStore,
    /// Sharded, process-local fairness limits for long-lived player proxies.
    pub(crate) proxy_admission: crate::services::proxy_admission::ProxyAdmission,
    /// Bounded handoff to the per-process best-effort user-activity writer.
    /// Requests only `try_send`; the worker owns all PostgreSQL interaction.
    pub(crate) user_activity: crate::middlewares::user_activity::ActivityQueue,
}

/// One real-time message: which client hub method to invoke, which game it
/// belongs to (for per-connection filtering; `None` = broadcast to all games),
/// and the already-shaped JSON payload that becomes the invocation argument.
#[derive(Clone, Debug)]
pub struct HubEvent {
    pub target: &'static str,
    pub game_id: Option<i32>,
    pub payload: String,
}

impl AppState {
    /// Publish a real-time event to a specific client hub method, scoped to a
    /// game (best-effort; drops when no subscribers). `target` is the SignalR
    /// method the client subscribed to (e.g. `"ReceivedGameNotice"`,
    /// `"ReceivedSubmissions"`).
    pub fn publish_event(
        &self,
        target: &'static str,
        game_id: Option<i32>,
        payload: impl Into<String>,
    ) {
        self.events.publish(HubEvent {
            target,
            game_id,
            payload: payload.into(),
        });
    }
}

/// The `Clone`-able handle threaded through the router as axum state.
pub type SharedState = Arc<AppState>;

impl AppState {
    pub fn new(
        db: DatabaseConnection,
        config: Arc<AppConfig>,
        cache: Arc<dyn Cache>,
        storage: Arc<dyn BlobStorage>,
        token: TokenService,
        containers: Arc<dyn ContainerManager>,
    ) -> SharedState {
        Self::new_with_events(
            db,
            config,
            cache,
            storage,
            token,
            containers,
            EventBus::local(),
        )
    }

    /// Construct application state with an explicitly selected real-time bus.
    /// Single-node startup keeps using [`Self::new`]; replica-aware startup can
    /// pass [`EventBus::distributed`] without changing any handler or hub code.
    pub fn new_with_events(
        db: DatabaseConnection,
        config: Arc<AppConfig>,
        cache: Arc<dyn Cache>,
        storage: Arc<dyn BlobStorage>,
        token: TokenService,
        containers: Arc<dyn ContainerManager>,
        events: EventBus,
    ) -> SharedState {
        Self::new_with_events_and_worker_issuer(
            db, containers, config, cache, storage, token, events, None, None,
        )
    }

    /// Composition entry used by the server binary when worker enrollment is
    /// configured. Keeping the older constructor preserves lightweight tests
    /// and deployments that do not enable the worker plane.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_events_and_worker_issuer(
        db: DatabaseConnection,
        containers: Arc<dyn ContainerManager>,
        config: Arc<AppConfig>,
        cache: Arc<dyn Cache>,
        storage: Arc<dyn BlobStorage>,
        token: TokenService,
        events: EventBus,
        worker_issuer: Option<Arc<crate::services::worker_pki::WorkerIssuer>>,
        workers: Option<Arc<crate::services::worker::WorkerService>>,
    ) -> SharedState {
        let topology =
            RuntimeTopologyProbe::new(config.runtime_role, crate::services::ad_vpn::enabled());
        let worker_store = crate::services::worker_store::WorkerStore::new(
            db.get_postgres_connection_pool().clone(),
        );
        Arc::new(Self {
            db,
            config,
            cache,
            storage,
            token,
            containers,
            readiness: ReadinessProbe::new(),
            topology,
            byoc: Arc::new(crate::services::byoc_tunnel::Registry::new(events.clone())),
            worker_issuer,
            workers,
            worker_store,
            proxy_admission: crate::services::proxy_admission::ProxyAdmission::new(),
            events,
            user_activity: crate::middlewares::user_activity::ActivityQueue::new(),
        })
    }

    /// The underlying sqlx [`PgPool`](sqlx::PgPool) that sea-orm runs on — the
    /// entry point for hand-written raw SQL on the few hot/heavy queries where
    /// skipping the ORM's query-build + full-entity mapping (and, more
    /// importantly, over-fetch) is worth it. Same pool ⇒ the same connections and
    /// prepared-statement cache sea-orm uses; raw SQL and the ORM coexist.
    pub fn pg(&self) -> &sqlx::PgPool {
        self.db.get_postgres_connection_pool()
    }
}
