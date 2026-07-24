//! Bounded admission for the read-only event-stream WebSocket hubs.
//!
//! These sockets are intentionally long-lived, so request-rate limiting alone
//! cannot bound their retained tasks, broadcast receivers, and file descriptors.
//! Hold one permit for the complete connection lifetime and partition the
//! ceilings by source and game so one client or event cannot monopolize the
//! process-wide pool.

use std::hash::Hash;
use std::net::IpAddr;
use std::sync::{Arc, LazyLock};

use axum::http::HeaderMap;
use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const MAX_CONNECTIONS: usize = 2_048;
const MAX_CONNECTIONS_PER_CLIENT: usize = 128;
const MAX_CONNECTIONS_PER_GAME: usize = 1_024;
const MAX_GLOBAL_SCOPE_CONNECTIONS: usize = 256;

static CONNECTIONS: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(MAX_CONNECTIONS)));
static CLIENT_CONNECTIONS: LazyLock<Arc<DashMap<String, usize>>> =
    LazyLock::new(|| Arc::new(DashMap::new()));
static SCOPE_CONNECTIONS: LazyLock<Arc<DashMap<Scope, usize>>> =
    LazyLock::new(|| Arc::new(DashMap::new()));

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum Scope {
    Game(i32),
    Global,
}

impl Scope {
    fn limit(self, limits: Limits) -> usize {
        match self {
            Self::Game(_) => limits.per_game,
            Self::Global => limits.global_scope,
        }
    }
}

#[derive(Clone, Copy)]
struct Limits {
    per_client: usize,
    per_game: usize,
    global_scope: usize,
}

const LIMITS: Limits = Limits {
    per_client: MAX_CONNECTIONS_PER_CLIENT,
    per_game: MAX_CONNECTIONS_PER_GAME,
    global_scope: MAX_GLOBAL_SCOPE_CONNECTIONS,
};

pub(super) struct ConnectionPermit {
    #[allow(dead_code)]
    global: OwnedSemaphorePermit,
    #[allow(dead_code)]
    client: ClientPermit,
    #[allow(dead_code)]
    scope: ScopePermit,
}

struct ClientPermit {
    key: String,
    counts: Arc<DashMap<String, usize>>,
}

impl Drop for ClientPermit {
    fn drop(&mut self) {
        release(&self.counts, self.key.clone());
    }
}

struct ScopePermit {
    key: Scope,
    counts: Arc<DashMap<Scope, usize>>,
}

impl Drop for ScopePermit {
    fn drop(&mut self) {
        release(&self.counts, self.key);
    }
}

fn increment<K>(counts: &DashMap<K, usize>, key: K, limit: usize) -> bool
where
    K: Eq + Hash,
{
    match counts.entry(key) {
        Entry::Occupied(mut entry) if *entry.get() < limit => {
            *entry.get_mut() += 1;
            true
        }
        Entry::Vacant(entry) => {
            entry.insert(1);
            true
        }
        Entry::Occupied(_) => false,
    }
}

fn release<K>(counts: &DashMap<K, usize>, key: K)
where
    K: Eq + Hash,
{
    if let Entry::Occupied(mut entry) = counts.entry(key) {
        if *entry.get() <= 1 {
            entry.remove();
        } else {
            *entry.get_mut() -= 1;
        }
    }
}

fn try_connection_permit_with(
    client_key: String,
    scope_key: Scope,
    global: &Arc<Semaphore>,
    clients: &Arc<DashMap<String, usize>>,
    scopes: &Arc<DashMap<Scope, usize>>,
    limits: Limits,
) -> Option<ConnectionPermit> {
    let global = Arc::clone(global).try_acquire_owned().ok()?;
    if !increment(clients, client_key.clone(), limits.per_client) {
        return None;
    }
    if !increment(scopes, scope_key, scope_key.limit(limits)) {
        release(clients, client_key);
        return None;
    }
    Some(ConnectionPermit {
        global,
        client: ClientPermit {
            key: client_key,
            counts: Arc::clone(clients),
        },
        scope: ScopePermit {
            key: scope_key,
            counts: Arc::clone(scopes),
        },
    })
}

pub(super) fn try_connection_permit(client_key: String, scope: Scope) -> Option<ConnectionPermit> {
    try_connection_permit_with(
        client_key,
        scope,
        &CONNECTIONS,
        &CLIENT_CONNECTIONS,
        &SCOPE_CONNECTIONS,
        LIMITS,
    )
}

pub(super) fn client_key(headers: &HeaderMap, peer: IpAddr) -> String {
    crate::services::anti_cheat::client_ip(headers, Some(peer))
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_limits() -> Limits {
        Limits {
            per_client: 2,
            per_game: 3,
            global_scope: 1,
        }
    }

    #[test]
    fn permits_bound_clients_scopes_and_the_global_pool() {
        let global = Arc::new(Semaphore::new(4));
        let clients = Arc::new(DashMap::new());
        let scopes = Arc::new(DashMap::new());
        let limits = test_limits();

        let first = try_connection_permit_with(
            "client-a".into(),
            Scope::Game(7),
            &global,
            &clients,
            &scopes,
            limits,
        )
        .unwrap();
        let second = try_connection_permit_with(
            "client-a".into(),
            Scope::Game(7),
            &global,
            &clients,
            &scopes,
            limits,
        )
        .unwrap();
        assert!(try_connection_permit_with(
            "client-a".into(),
            Scope::Game(8),
            &global,
            &clients,
            &scopes,
            limits,
        )
        .is_none());

        let third = try_connection_permit_with(
            "client-b".into(),
            Scope::Game(7),
            &global,
            &clients,
            &scopes,
            limits,
        )
        .unwrap();
        assert!(try_connection_permit_with(
            "client-c".into(),
            Scope::Game(7),
            &global,
            &clients,
            &scopes,
            limits,
        )
        .is_none());

        let global_scope = try_connection_permit_with(
            "client-c".into(),
            Scope::Global,
            &global,
            &clients,
            &scopes,
            limits,
        )
        .unwrap();
        assert!(try_connection_permit_with(
            "client-d".into(),
            Scope::Global,
            &global,
            &clients,
            &scopes,
            limits,
        )
        .is_none());
        assert!(try_connection_permit_with(
            "client-d".into(),
            Scope::Game(8),
            &global,
            &clients,
            &scopes,
            limits,
        )
        .is_none());

        drop((first, second, third, global_scope));
        assert!(clients.is_empty());
        assert!(scopes.is_empty());
        assert_eq!(global.available_permits(), 4);
    }
}
