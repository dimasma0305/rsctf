//! Stable, process-local TCP endpoints for BYOC relay sessions.
//!
//! The listener belongs to the `(participation, challenge)` identity rather
//! than to one WebSocket. A normal three-second agent reconnect therefore
//! swaps only the yamux session and keeps the address opponents already read.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::sync::{watch, RwLock, Semaphore};

use super::flag::{FlagRetention, FlagRetentionError, FlagState, RetainedFlag};
use super::{pipe, ActiveGuard, TunnelHandle, MAX_SERVICE_STREAMS};

static NEXT_ENDPOINT_ID: AtomicU64 = AtomicU64::new(1);

pub(super) struct RelayEndpoint {
    id: u64,
    host: String,
    port: i32,
    state: RwLock<EndpointState>,
    idle_epoch: AtomicU64,
    retired: AtomicBool,
    activation: Arc<Semaphore>,
    shutdown: tokio::sync::Notify,
    closed: watch::Receiver<bool>,
}

struct EndpointState {
    session: Option<TunnelHandle>,
    /// One bounded current flag per authorized `(participation, challenge)`
    /// endpoint. It survives ordinary WebSocket replacement but is cleared on
    /// endpoint retirement/revocation.
    flag: FlagState,
    readiness: SessionReadiness,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionReadiness {
    Pending,
    NoFlag,
    Flag(u64),
}

impl EndpointState {
    fn ready_handle(&self) -> Option<TunnelHandle> {
        let ready = match self.flag.current() {
            Some(flag) => self.readiness == SessionReadiness::Flag(flag.sequence()),
            None => self.readiness == SessionReadiness::NoFlag,
        };
        ready.then(|| self.session.clone()).flatten()
    }
}

impl RelayEndpoint {
    pub(super) async fn bind(host: String) -> std::io::Result<Arc<Self>> {
        let listener = TcpListener::bind((host.as_str(), 0)).await?;
        let port = i32::from(listener.local_addr()?.port());
        let (closed_tx, closed_rx) = watch::channel(false);
        let endpoint = Arc::new(Self {
            id: NEXT_ENDPOINT_ID.fetch_add(1, Ordering::SeqCst),
            host,
            port,
            state: RwLock::new(EndpointState {
                session: None,
                flag: FlagState::default(),
                readiness: SessionReadiness::Pending,
            }),
            idle_epoch: AtomicU64::new(1),
            retired: AtomicBool::new(false),
            activation: Arc::new(Semaphore::new(1)),
            shutdown: tokio::sync::Notify::new(),
            closed: closed_rx,
        });
        tokio::spawn(run_acceptor(endpoint.clone(), listener, closed_tx));
        Ok(endpoint)
    }

    pub(super) fn id(&self) -> u64 {
        self.id
    }

    pub(super) fn host(&self) -> &str {
        &self.host
    }

    pub(super) fn port(&self) -> i32 {
        self.port
    }

    /// Claim this endpoint for a pending activation. Advancing the epoch while
    /// it is still in the registry fences an already-scheduled idle reaper.
    pub(super) fn try_activation(
        &self,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, tokio::sync::TryAcquireError> {
        self.activation.clone().try_acquire_owned()
    }

    pub(super) async fn claim(&self) -> Option<u64> {
        // Serialize the retirement check with `retire_if_idle`. Otherwise a
        // reaper could validate an old epoch, then retire immediately after a
        // reconnect believed it had successfully claimed the endpoint.
        let _state = self.state.read().await;
        if self.retired.load(Ordering::SeqCst) {
            return None;
        }
        let epoch = self.idle_epoch.fetch_add(1, Ordering::SeqCst) + 1;
        (!self.retired.load(Ordering::SeqCst)).then_some(epoch)
    }

    pub(super) async fn is_idle_at(&self, epoch: u64) -> bool {
        let state = self.state.read().await;
        !self.retired.load(Ordering::SeqCst)
            && self.idle_epoch.load(Ordering::SeqCst) == epoch
            && state.session.is_none()
    }

    pub(super) async fn current(&self) -> Option<TunnelHandle> {
        if self.retired.load(Ordering::SeqCst) {
            return None;
        }
        let state = self.state.read().await;
        if self.retired.load(Ordering::SeqCst) {
            return None;
        }
        state.ready_handle()
    }

    /// The raw current session is deliberately private to control-plane flag
    /// delivery. Service and exec forwarding must use [`Self::current`], which
    /// remains unavailable until the retained flag is acknowledged.
    pub(super) async fn raw_current(&self) -> Option<TunnelHandle> {
        if self.retired.load(Ordering::SeqCst) {
            return None;
        }
        let state = self.state.read().await;
        (!self.retired.load(Ordering::SeqCst))
            .then(|| state.session.clone())
            .flatten()
    }

    pub(super) async fn retain_flag(
        &self,
        sequence: u64,
        value: &str,
    ) -> Result<FlagRetention, FlagRetentionError> {
        if self.retired.load(Ordering::SeqCst) {
            return Err(FlagRetentionError::Retired);
        }
        let mut state = self.state.write().await;
        let retained = state.flag.retain(sequence, value)?;
        if self.retired.load(Ordering::SeqCst) {
            state.flag.clear();
            state.readiness = SessionReadiness::Pending;
            return Err(FlagRetentionError::Retired);
        }
        if let FlagRetention::Accepted(flag) = &retained {
            if state.readiness != SessionReadiness::Flag(flag.sequence()) {
                state.readiness = SessionReadiness::Pending;
            }
        }
        Ok(retained)
    }

    pub(super) async fn retained_flag(&self) -> Option<RetainedFlag> {
        if self.retired.load(Ordering::SeqCst) {
            return None;
        }
        let retained = self.state.read().await.flag.current();
        (!self.retired.load(Ordering::SeqCst))
            .then_some(retained)
            .flatten()
    }

    /// Publish a new session and wake the replaced one. The listener itself is
    /// unchanged, so host/port never become a session-generation identifier.
    pub(super) async fn attach(&self, handle: TunnelHandle) -> Result<(), TunnelHandle> {
        let mut state = self.state.write().await;
        if self.retired.load(Ordering::SeqCst) {
            return Err(handle);
        }
        self.idle_epoch.fetch_add(1, Ordering::SeqCst);
        state.readiness = if state.flag.current().is_some() {
            SessionReadiness::Pending
        } else {
            SessionReadiness::NoFlag
        };
        if let Some(old) = state.session.replace(handle) {
            old.shutdown.notify_one();
        }
        Ok(())
    }

    /// Mark one exact retained flag ready only if the acknowledging session is
    /// still current. A replacement, revocation, or newer retained value fences
    /// the late ACK and leaves forwarding unavailable.
    pub(super) async fn mark_flag_ready(&self, connection_id: u64, flag: &RetainedFlag) -> bool {
        let mut state = self.state.write().await;
        if self.retired.load(Ordering::SeqCst)
            || state
                .session
                .as_ref()
                .is_none_or(|handle| handle.id != connection_id)
            || state
                .flag
                .current()
                .is_none_or(|current| current.sequence() != flag.sequence())
        {
            return false;
        }
        state.readiness = SessionReadiness::Flag(flag.sequence());
        true
    }

    /// Detach only the session that still owns this endpoint. This connection
    /// id fence is mandatory once old and new sessions share the same address.
    pub(super) async fn detach_if(&self, connection_id: u64) -> Option<u64> {
        let mut state = self.state.write().await;
        if state
            .session
            .as_ref()
            .is_none_or(|handle| handle.id != connection_id)
        {
            return None;
        }
        state.session.take();
        state.readiness = SessionReadiness::Pending;
        Some(self.idle_epoch.fetch_add(1, Ordering::SeqCst) + 1)
    }

    /// Atomically turn an unchanged idle reservation into a retired endpoint.
    /// An attach racing this call either wins first or observes `retired`.
    pub(super) async fn retire_if_idle(&self, epoch: u64) -> bool {
        let mut state = self.state.write().await;
        if self.retired.load(Ordering::SeqCst)
            || self.idle_epoch.load(Ordering::SeqCst) != epoch
            || state.session.is_some()
        {
            return false;
        }
        self.retired.store(true, Ordering::SeqCst);
        self.idle_epoch.fetch_add(1, Ordering::SeqCst);
        state.flag.clear();
        state.readiness = SessionReadiness::Pending;
        true
    }

    /// Revoke this complete endpoint, returning the session whose owning task
    /// must be awaited. The acceptor stops before the bound socket is released.
    pub(super) async fn revoke(&self) -> Option<TunnelHandle> {
        self.retired.store(true, Ordering::SeqCst);
        self.idle_epoch.fetch_add(1, Ordering::SeqCst);
        let mut state = self.state.write().await;
        let handle = state.session.take();
        state.flag.clear();
        state.readiness = SessionReadiness::Pending;
        if let Some(handle) = &handle {
            handle.shutdown.notify_one();
        }
        self.shutdown.notify_one();
        handle
    }

    pub(super) fn stop_acceptor(&self) {
        self.shutdown.notify_one();
    }

    pub(super) async fn wait_closed(&self) {
        let mut closed = self.closed.clone();
        if !*closed.borrow() {
            let _ = closed.changed().await;
        }
    }
}

async fn run_acceptor(
    endpoint: Arc<RelayEndpoint>,
    listener: TcpListener,
    closed: watch::Sender<bool>,
) {
    let service_slots = Arc::new(Semaphore::new(MAX_SERVICE_STREAMS));
    let minimum_backoff = std::time::Duration::from_millis(25);
    let maximum_backoff = std::time::Duration::from_secs(1);
    let mut accept_backoff = minimum_backoff;
    loop {
        let accepted = tokio::select! {
            _ = endpoint.shutdown.notified() => break,
            accepted = listener.accept() => accepted,
        };
        let (client, _) = match accepted {
            Ok(connection) => {
                accept_backoff = minimum_backoff;
                connection
            }
            Err(error) => {
                tracing::warn!(%error, ?accept_backoff, "byoc: stable service listener accept failed; retrying");
                tokio::select! {
                    _ = endpoint.shutdown.notified() => break,
                    _ = tokio::time::sleep(accept_backoff) => {}
                }
                accept_backoff = std::cmp::min(accept_backoff.saturating_mul(2), maximum_backoff);
                continue;
            }
        };
        let Some(handle) = endpoint.current().await else {
            // The address stays reserved briefly for reconnect, but an offline
            // endpoint must not queue traffic for a future agent session.
            drop(client);
            continue;
        };
        let Ok(service_slot) = service_slots.clone().try_acquire_owned() else {
            drop(client);
            continue;
        };
        tokio::spawn(async move {
            let _service_slot = service_slot;
            handle.active.fetch_add(1, Ordering::Relaxed);
            let _active = ActiveGuard(handle.active.clone());
            if let Some(stream) = handle.open_stream().await {
                let _ = pipe(client, stream).await;
            }
        });
    }
    let _ = closed.send(true);
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::sync::mpsc;

    use super::*;

    fn handle(id: u64) -> (TunnelHandle, mpsc::Receiver<super::super::OpenReq>) {
        let (open, receiver) = mpsc::channel(4);
        let (_closed_tx, closed) = watch::channel(false);
        (
            TunnelHandle {
                id,
                open,
                shutdown: Arc::new(tokio::sync::Notify::new()),
                closed,
                active: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            },
            receiver,
        )
    }

    #[tokio::test]
    async fn reconnect_swaps_the_session_without_rotating_the_endpoint() {
        let endpoint = RelayEndpoint::bind("127.0.0.1".to_string()).await.unwrap();
        let address = (endpoint.host().to_string(), endpoint.port());
        let (first, _first_open) = handle(10);
        let first_shutdown = first.shutdown.clone();
        assert!(endpoint.attach(first).await.is_ok());

        let (second, mut second_open) = handle(11);
        assert!(endpoint.attach(second).await.is_ok());
        tokio::time::timeout(Duration::from_millis(100), first_shutdown.notified())
            .await
            .expect("superseded session was not notified");
        assert_eq!((endpoint.host().to_string(), endpoint.port()), address);
        assert!(endpoint.detach_if(10).await.is_none());

        let client = tokio::net::TcpStream::connect((address.0.as_str(), address.1 as u16))
            .await
            .unwrap();
        let request = tokio::time::timeout(Duration::from_millis(100), second_open.recv())
            .await
            .unwrap()
            .unwrap();
        let _ = request.send(Err(()));
        drop(client);

        assert!(endpoint.detach_if(11).await.is_some());
        endpoint.stop_acceptor();
        endpoint.wait_closed().await;
    }

    #[tokio::test]
    async fn stale_idle_epoch_cannot_reap_a_claimed_endpoint() {
        let endpoint = RelayEndpoint::bind("127.0.0.1".to_string()).await.unwrap();
        let (session, _open) = handle(20);
        assert!(endpoint.attach(session).await.is_ok());
        let idle_epoch = endpoint.detach_if(20).await.unwrap();
        assert!(endpoint.claim().await.is_some());
        assert!(!endpoint.retire_if_idle(idle_epoch).await);
        endpoint.stop_acceptor();
        endpoint.wait_closed().await;
    }

    #[tokio::test]
    async fn registry_reuses_a_reserved_listener_after_ordinary_loss() {
        let registry = Arc::new(super::super::Registry::new(
            crate::services::event_bus::EventBus::local(),
        ));
        let (endpoint, _, first_activation) = registry
            .reserve_endpoint(31, 41, "127.0.0.1")
            .await
            .unwrap();
        drop(first_activation);
        let address = (endpoint.host().to_string(), endpoint.port());
        let (first, _first_open) = handle(30);
        assert!(endpoint.attach(first).await.is_ok());
        let first_idle = endpoint.detach_if(30).await.unwrap();

        let (reused, _, second_activation) = registry
            .reserve_endpoint(31, 41, "127.0.0.1")
            .await
            .unwrap();
        drop(second_activation);
        assert!(Arc::ptr_eq(&endpoint, &reused));
        assert_eq!((reused.host().to_string(), reused.port()), address);
        assert!(!endpoint.retire_if_idle(first_idle).await);

        let (second, _second_open) = handle(31);
        assert!(reused.attach(second).await.is_ok());
        let second_idle = reused.detach_if(31).await.unwrap();
        registry
            .retire_idle_endpoint(31, 41, reused, second_idle)
            .await;
    }

    #[tokio::test]
    async fn restart_hydrated_replay_gates_forwarding_and_fences_superseded_ack() {
        let endpoint = RelayEndpoint::bind("127.0.0.1".to_string()).await.unwrap();
        let FlagRetention::Accepted(retained) =
            endpoint.retain_flag(70, "current-flag").await.unwrap()
        else {
            panic!("current flag was not retained")
        };
        let (first, _first_open) = handle(40);
        assert!(endpoint.attach(first).await.is_ok());
        assert_eq!(endpoint.retained_flag().await, Some(retained.clone()));
        assert!(endpoint.current().await.is_none());
        assert_eq!(endpoint.raw_current().await.unwrap().id, 40);
        assert!(endpoint.mark_flag_ready(40, &retained).await);
        assert_eq!(endpoint.current().await.unwrap().id, 40);

        let (second, _second_open) = handle(41);
        assert!(endpoint.attach(second).await.is_ok());
        assert_eq!(endpoint.retained_flag().await, Some(retained.clone()));
        assert!(endpoint.current().await.is_none());
        assert!(!endpoint.mark_flag_ready(40, &retained).await);
        assert!(endpoint.mark_flag_ready(41, &retained).await);
        assert_eq!(endpoint.current().await.unwrap().id, 41);

        assert!(endpoint.revoke().await.is_some());
        assert!(!endpoint.mark_flag_ready(41, &retained).await);
        assert!(endpoint.current().await.is_none());
        assert!(endpoint.raw_current().await.is_none());
        assert!(endpoint.retained_flag().await.is_none());
        endpoint.wait_closed().await;
    }

    #[tokio::test]
    async fn newer_retained_sequence_invalidates_an_in_flight_ack() {
        let endpoint = RelayEndpoint::bind("127.0.0.1".to_string()).await.unwrap();
        let FlagRetention::Accepted(first) = endpoint.retain_flag(80, "first").await.unwrap()
        else {
            panic!("first flag was not retained")
        };
        let (session, _open) = handle(50);
        assert!(endpoint.attach(session).await.is_ok());

        let FlagRetention::Accepted(second) = endpoint.retain_flag(81, "second").await.unwrap()
        else {
            panic!("second flag was not retained")
        };
        assert!(!endpoint.mark_flag_ready(50, &first).await);
        assert!(endpoint.current().await.is_none());
        assert!(endpoint.mark_flag_ready(50, &second).await);
        assert_eq!(endpoint.current().await.unwrap().id, 50);

        assert!(endpoint.revoke().await.is_some());
        endpoint.wait_closed().await;
    }

    #[tokio::test]
    async fn stale_hydration_cannot_downgrade_and_equal_conflict_fails_closed() {
        let endpoint = RelayEndpoint::bind("127.0.0.1".to_string()).await.unwrap();
        let FlagRetention::Accepted(current) = endpoint.retain_flag(91, "new").await.unwrap()
        else {
            panic!("current flag was not retained")
        };
        assert_eq!(
            endpoint.retain_flag(90, "old").await,
            Ok(FlagRetention::Stale(current.clone()))
        );
        assert_eq!(endpoint.retained_flag().await, Some(current));
        assert_eq!(
            endpoint.retain_flag(91, "forged").await,
            Err(FlagRetentionError::SequenceConflict)
        );
        let (session, _open) = handle(60);
        assert!(endpoint.attach(session).await.is_ok());
        assert!(endpoint.current().await.is_none());
        assert!(endpoint.revoke().await.is_some());
        endpoint.wait_closed().await;
    }
}
