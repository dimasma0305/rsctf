//! Process liveness and dependency readiness HTTP probes.
//!
//! `/livez` deliberately performs no I/O. `/healthz` checks only dependencies
//! required to serve consistent platform state: PostgreSQL, blob storage, the
//! active Redis cache backend when configured, the in-memory split-role
//! presence snapshot, and capture-owner restoration state. Docker, WireGuard,
//! and challenge containers are not probed from the request path; the capture
//! owner instead publishes its already-known local transition atomically.

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use tokio::sync::Mutex;

use crate::app_state::SharedState;
use crate::models::internal::configs::RuntimeRole;
use crate::services::cache::{Cache, CacheBackendHealth};
use crate::storage::BlobStorage;

const DEPENDENCY_TIMEOUT: Duration = Duration::from_millis(750);
const RESULT_TTL: Duration = Duration::from_secs(1);
// A short pool queue during a synchronized poll burst must not make the
// orchestrator evict an otherwise healthy replica. Reuse only a *confirmed*
// recent success when a dependency probe itself times out; explicit connection
// or protocol failures still make readiness fail immediately. The confirmed
// timestamp is never advanced by this fallback, so a real outage cannot remain
// hidden indefinitely.
const STALE_SUCCESS_GRACE: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ReadinessStatus {
    postgres: bool,
    redis: bool,
    storage: bool,
}

#[derive(Clone, Copy)]
struct CachedStatus {
    checked_at: Instant,
    status: ReadinessStatus,
    postgres_confirmed_at: Option<Instant>,
    redis_confirmed_at: Option<Instant>,
    storage_confirmed_at: Option<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProbeOutcome {
    Healthy,
    Unhealthy,
    TimedOut,
}

/// Single-flight dependency probe with a one-second result cache. This keeps
/// repeated monitoring (or a health-endpoint flood) from consuming a database
/// connection and Redis round-trip per request.
pub struct ReadinessProbe {
    cached: Mutex<Option<CachedStatus>>,
    lifecycle: AtomicU8,
    capture_restore: AtomicU8,
}

/// Process lifecycle as observed by `/healthz`. Dependency probes are skipped
/// while starting or draining so a terminating replica leaves load-balancer
/// rotation immediately instead of waiting for the one-second probe cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeReadiness {
    Starting = 0,
    Ready = 1,
    Draining = 2,
}

impl ReadinessProbe {
    pub fn new() -> Self {
        Self {
            cached: Mutex::new(None),
            lifecycle: AtomicU8::new(RuntimeReadiness::Starting as u8),
            capture_restore: AtomicU8::new(0),
        }
    }

    pub fn mark_ready(&self) {
        self.lifecycle
            .store(RuntimeReadiness::Ready as u8, Ordering::Release);
    }

    pub fn begin_shutdown(&self) {
        self.lifecycle
            .store(RuntimeReadiness::Draining as u8, Ordering::Release);
    }

    /// Gate a capture-capable replica until it owns the singleton and every
    /// desired capture has acknowledged startup. A contender that does not own
    /// the lease remains out of rotation; this prevents a sole replacement
    /// from claiming readiness during an ownership gap.
    pub fn begin_capture_restore(&self) {
        self.capture_restore.store(1, Ordering::Release);
    }

    pub fn finish_capture_restore(&self) {
        self.capture_restore.store(0, Ordering::Release);
    }

    fn capture_restore_in_progress(&self) -> bool {
        self.capture_restore.load(Ordering::Acquire) != 0
    }

    pub fn lifecycle(&self) -> RuntimeReadiness {
        match self.lifecycle.load(Ordering::Acquire) {
            value if value == RuntimeReadiness::Ready as u8 => RuntimeReadiness::Ready,
            value if value == RuntimeReadiness::Draining as u8 => RuntimeReadiness::Draining,
            _ => RuntimeReadiness::Starting,
        }
    }

    async fn check(
        &self,
        pg: &sqlx::PgPool,
        cache: &dyn Cache,
        storage: &dyn BlobStorage,
    ) -> ReadinessStatus {
        let mut cached = self.cached.lock().await;
        if let Some(entry) = *cached {
            if entry.checked_at.elapsed() < RESULT_TTL {
                return entry.status;
            }
        }

        let postgres = tokio::time::timeout(
            DEPENDENCY_TIMEOUT,
            sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(pg),
        );
        let redis = tokio::time::timeout(DEPENDENCY_TIMEOUT, cache.backend_health());
        let storage = tokio::time::timeout(DEPENDENCY_TIMEOUT, storage.health());
        let (postgres, redis, storage) = tokio::join!(postgres, redis, storage);

        let postgres = match postgres {
            Ok(Ok(1)) => ProbeOutcome::Healthy,
            Ok(Ok(_)) | Ok(Err(_)) => ProbeOutcome::Unhealthy,
            Err(_) => ProbeOutcome::TimedOut,
        };
        let redis = match redis {
            Ok(CacheBackendHealth::Local | CacheBackendHealth::Ready) => ProbeOutcome::Healthy,
            Ok(CacheBackendHealth::Unavailable) => ProbeOutcome::Unhealthy,
            Err(_) => ProbeOutcome::TimedOut,
        };
        let storage = match storage {
            Ok(Ok(())) => ProbeOutcome::Healthy,
            Ok(Err(_)) => ProbeOutcome::Unhealthy,
            Err(_) => ProbeOutcome::TimedOut,
        };
        let now = Instant::now();
        let previous = *cached;
        let (postgres, postgres_confirmed_at) = dependency_status(
            postgres,
            previous.map(|entry| (entry.status.postgres, entry.postgres_confirmed_at)),
            now,
        );
        let (redis, redis_confirmed_at) = dependency_status(
            redis,
            previous.map(|entry| (entry.status.redis, entry.redis_confirmed_at)),
            now,
        );
        let (storage, storage_confirmed_at) = dependency_status(
            storage,
            previous.map(|entry| (entry.status.storage, entry.storage_confirmed_at)),
            now,
        );
        let status = ReadinessStatus {
            postgres,
            redis,
            storage,
        };
        *cached = Some(CachedStatus {
            checked_at: now,
            status,
            postgres_confirmed_at,
            redis_confirmed_at,
            storage_confirmed_at,
        });
        status
    }
}

fn dependency_status(
    outcome: ProbeOutcome,
    previous: Option<(bool, Option<Instant>)>,
    now: Instant,
) -> (bool, Option<Instant>) {
    match outcome {
        ProbeOutcome::Healthy => (true, Some(now)),
        ProbeOutcome::Unhealthy => (false, previous.and_then(|(_, confirmed)| confirmed)),
        ProbeOutcome::TimedOut => {
            let confirmed = previous.and_then(|(_, confirmed)| confirmed);
            let recently_healthy = previous.is_some_and(|(healthy, confirmed)| {
                healthy && confirmed.is_some_and(|at| now.duration_since(at) < STALE_SUCCESS_GRACE)
            });
            (recently_healthy, confirmed)
        }
    }
}

/// Process liveness only. A successful response means the HTTP runtime can run
/// a handler; it intentionally says nothing about external dependencies.
pub async fn liveness() -> &'static str {
    "ok"
}

/// Keep a draining web replica useful during the load-balancer propagation
/// window while rejecting fresh work immediately on stateful roles. `/healthz`
/// is already unavailable in both cases, so an active load-balancer check can
/// remove the replica before Axum stops accepting connections. Requests that
/// already passed this gate continue normally.
pub async fn reject_new_work_while_draining(
    State(st): State<SharedState>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    if !accepts_new_request(st.readiness.lifecycle(), st.config.runtime_role, path) {
        return (StatusCode::SERVICE_UNAVAILABLE, "shutting down").into_response();
    }
    next.run(request).await
}

fn accepts_new_request(lifecycle: RuntimeReadiness, role: RuntimeRole, path: &str) -> bool {
    lifecycle != RuntimeReadiness::Draining
        || role == RuntimeRole::Web
        || matches!(path, "/livez" | "/healthz")
}

/// Dependency readiness. Success keeps the historical exact `ok` response;
/// failure uses 503 with a terse dependency label suitable for operators.
pub async fn readiness(State(st): State<SharedState>) -> Response {
    let lifecycle = st.readiness.lifecycle();
    let mut response = if lifecycle == RuntimeReadiness::Ready {
        let status = st
            .readiness
            .check(st.pg(), st.cache.as_ref(), st.storage.as_ref())
            .await;
        let dependencies = dependency_response(status);
        if dependencies.status() == StatusCode::OK {
            let capture = capture_response(&st.readiness);
            if capture.status() == StatusCode::OK {
                topology_response(&st.topology)
            } else {
                capture
            }
        } else {
            dependencies
        }
    } else {
        lifecycle_response(lifecycle)
    };
    add_role_headers(&mut response, st.config.runtime_role);
    response
}

fn capture_response(readiness: &ReadinessProbe) -> Response {
    if readiness.capture_restore_in_progress() {
        (StatusCode::SERVICE_UNAVAILABLE, "traffic capture restoring").into_response()
    } else {
        (StatusCode::OK, "ok").into_response()
    }
}

fn topology_response(
    topology: &crate::services::runtime_topology::RuntimeTopologyProbe,
) -> Response {
    match topology.unavailable_reason() {
        None => (StatusCode::OK, "ok").into_response(),
        Some(reason) => (StatusCode::SERVICE_UNAVAILABLE, reason).into_response(),
    }
}

fn dependency_response(status: ReadinessStatus) -> Response {
    match (status.postgres, status.redis, status.storage) {
        (true, true, true) => (StatusCode::OK, "ok").into_response(),
        (false, true, true) => {
            (StatusCode::SERVICE_UNAVAILABLE, "postgres unavailable").into_response()
        }
        (true, false, true) => {
            (StatusCode::SERVICE_UNAVAILABLE, "redis unavailable").into_response()
        }
        (true, true, false) => {
            (StatusCode::SERVICE_UNAVAILABLE, "storage unavailable").into_response()
        }
        _ => (StatusCode::SERVICE_UNAVAILABLE, "dependencies unavailable").into_response(),
    }
}

fn lifecycle_response(state: RuntimeReadiness) -> Response {
    match state {
        RuntimeReadiness::Ready => unreachable!("ready state requires dependency probes"),
        RuntimeReadiness::Starting => (StatusCode::SERVICE_UNAVAILABLE, "starting").into_response(),
        RuntimeReadiness::Draining => {
            (StatusCode::SERVICE_UNAVAILABLE, "shutting down").into_response()
        }
    }
}

fn add_role_headers(response: &mut Response, role: RuntimeRole) {
    response
        .headers_mut()
        .insert("x-rsctf-role", HeaderValue::from_static(role.as_str()));
    response.headers_mut().insert(
        "x-rsctf-capabilities",
        HeaderValue::from_static(role.capability_header()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    #[tokio::test]
    async fn readiness_response_preserves_ok_contract() {
        let response = dependency_response(ReadinessStatus {
            postgres: true,
            redis: true,
            storage: true,
        });
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            to_bytes(response.into_body(), 16).await.unwrap().as_ref(),
            b"ok"
        );
    }

    #[test]
    fn unavailable_dependency_returns_service_unavailable() {
        for status in [
            ReadinessStatus {
                postgres: false,
                redis: true,
                storage: true,
            },
            ReadinessStatus {
                postgres: true,
                redis: false,
                storage: true,
            },
            ReadinessStatus {
                postgres: false,
                redis: false,
                storage: true,
            },
            ReadinessStatus {
                postgres: true,
                redis: true,
                storage: false,
            },
        ] {
            assert_eq!(
                dependency_response(status).status(),
                StatusCode::SERVICE_UNAVAILABLE
            );
        }
    }

    #[test]
    fn lifecycle_is_not_ready_until_started_and_drops_before_drain() {
        let probe = ReadinessProbe::new();
        assert_eq!(probe.lifecycle(), RuntimeReadiness::Starting);
        assert_eq!(
            lifecycle_response(probe.lifecycle()).status(),
            StatusCode::SERVICE_UNAVAILABLE
        );

        probe.mark_ready();
        assert_eq!(probe.lifecycle(), RuntimeReadiness::Ready);

        probe.begin_shutdown();
        assert_eq!(probe.lifecycle(), RuntimeReadiness::Draining);
        assert_eq!(
            lifecycle_response(probe.lifecycle()).status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[test]
    fn capture_candidate_is_gated_until_owner_restore_finishes() {
        let probe = ReadinessProbe::new();
        assert_eq!(capture_response(&probe).status(), StatusCode::OK);

        probe.begin_capture_restore();
        assert_eq!(
            capture_response(&probe).status(),
            StatusCode::SERVICE_UNAVAILABLE
        );

        probe.finish_capture_restore();
        assert_eq!(capture_response(&probe).status(), StatusCode::OK);
    }

    #[test]
    fn draining_web_accepts_requests_during_load_balancer_deregistration() {
        assert!(accepts_new_request(
            RuntimeReadiness::Draining,
            RuntimeRole::Web,
            "/api/Game/1"
        ));
        assert!(accepts_new_request(
            RuntimeReadiness::Draining,
            RuntimeRole::Web,
            "/assets/app.js"
        ));
    }

    #[test]
    fn draining_stateful_roles_reject_new_work_but_keep_health_probes() {
        for role in [RuntimeRole::All, RuntimeRole::Control, RuntimeRole::Network] {
            assert!(accepts_new_request(
                RuntimeReadiness::Ready,
                role,
                "/api/Game/1"
            ));
            assert!(accepts_new_request(
                RuntimeReadiness::Draining,
                role,
                "/livez"
            ));
            assert!(accepts_new_request(
                RuntimeReadiness::Draining,
                role,
                "/healthz"
            ));
            assert!(!accepts_new_request(
                RuntimeReadiness::Draining,
                role,
                "/api/Game/1"
            ));
        }
    }

    #[test]
    fn readiness_metadata_describes_the_runtime_role() {
        let mut response = (StatusCode::OK, "ok").into_response();
        add_role_headers(&mut response, RuntimeRole::Engine);
        assert_eq!(response.headers()["x-rsctf-role"], "engine");
        assert_eq!(
            response.headers()["x-rsctf-capabilities"],
            "health,maintenance,round-engine"
        );
    }

    #[test]
    fn missing_runtime_role_fails_readiness_without_changing_ok_contract() {
        let missing =
            crate::services::runtime_topology::RuntimeTopologyProbe::new(RuntimeRole::Web, false);
        assert_eq!(
            topology_response(&missing).status(),
            StatusCode::SERVICE_UNAVAILABLE
        );

        let self_contained = crate::services::runtime_topology::RuntimeTopologyProbe::new(
            RuntimeRole::Control,
            true,
        );
        let response = topology_response(&self_contained);
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn timeout_reuses_only_a_recent_confirmed_success() {
        let confirmed = Instant::now();
        let within_grace = confirmed + STALE_SUCCESS_GRACE - Duration::from_millis(1);
        let after_grace = confirmed + STALE_SUCCESS_GRACE;

        assert_eq!(
            dependency_status(
                ProbeOutcome::TimedOut,
                Some((true, Some(confirmed))),
                within_grace,
            ),
            (true, Some(confirmed))
        );
        assert_eq!(
            dependency_status(
                ProbeOutcome::TimedOut,
                Some((true, Some(confirmed))),
                after_grace,
            ),
            (false, Some(confirmed))
        );
        assert_eq!(
            dependency_status(ProbeOutcome::TimedOut, None, within_grace),
            (false, None)
        );
    }

    #[test]
    fn explicit_failure_is_never_masked_by_stale_success() {
        let confirmed = Instant::now();
        assert_eq!(
            dependency_status(
                ProbeOutcome::Unhealthy,
                Some((true, Some(confirmed))),
                confirmed + Duration::from_millis(1),
            ),
            (false, Some(confirmed))
        );
    }
}
