use std::sync::{Arc, LazyLock};
use std::time::Instant;

use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

const MAX_EXEC_CONNECTIONS: usize = 128;
const MAX_EXEC_CONNECTIONS_PER_USER: usize = 4;
const MAX_ACTIVE_EXEC_SESSIONS: usize = 256;
const OPEN_BUDGET_CAPACITY: f64 = 16.0;
const OPEN_BUDGET_REFILL_PER_SEC: f64 = 1.0;

static EXEC_CONNECTIONS: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(MAX_EXEC_CONNECTIONS)));
static USER_CONNECTIONS: LazyLock<Arc<DashMap<Uuid, usize>>> =
    LazyLock::new(|| Arc::new(DashMap::new()));
static ACTIVE_EXEC_SESSIONS: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(MAX_ACTIVE_EXEC_SESSIONS)));

fn try_permit(pool: &Arc<Semaphore>) -> Option<OwnedSemaphorePermit> {
    Arc::clone(pool).try_acquire_owned().ok()
}

pub(super) struct ConnectionPermit {
    #[allow(dead_code)]
    global: OwnedSemaphorePermit,
    #[allow(dead_code)]
    user: UserConnectionPermit,
}

struct UserConnectionPermit {
    user_id: Uuid,
    counts: Arc<DashMap<Uuid, usize>>,
}

impl Drop for UserConnectionPermit {
    fn drop(&mut self) {
        if let Entry::Occupied(mut entry) = self.counts.entry(self.user_id) {
            if *entry.get() <= 1 {
                entry.remove();
            } else {
                *entry.get_mut() -= 1;
            }
        }
    }
}

fn try_connection_permit_with(
    user_id: Uuid,
    global: &Arc<Semaphore>,
    counts: &Arc<DashMap<Uuid, usize>>,
) -> Option<ConnectionPermit> {
    let global = try_permit(global)?;
    match counts.entry(user_id) {
        Entry::Occupied(mut entry) if *entry.get() < MAX_EXEC_CONNECTIONS_PER_USER => {
            *entry.get_mut() += 1;
        }
        Entry::Vacant(entry) => {
            entry.insert(1);
        }
        Entry::Occupied(_) => return None,
    }
    Some(ConnectionPermit {
        global,
        user: UserConnectionPermit {
            user_id,
            counts: Arc::clone(counts),
        },
    })
}

pub(super) fn try_connection_permit(user_id: Uuid) -> Option<ConnectionPermit> {
    try_connection_permit_with(user_id, &EXEC_CONNECTIONS, &USER_CONNECTIONS)
}

pub(super) fn try_session_permit() -> Option<OwnedSemaphorePermit> {
    try_permit(&ACTIVE_EXEC_SESSIONS)
}

pub(super) struct OpenBudget {
    tokens: f64,
    last: Instant,
}

impl OpenBudget {
    pub(super) fn new() -> Self {
        Self {
            tokens: OPEN_BUDGET_CAPACITY,
            last: Instant::now(),
        }
    }

    pub(super) fn try_take(&mut self) -> bool {
        let now = Instant::now();
        self.tokens = (self.tokens
            + now.duration_since(self.last).as_secs_f64() * OPEN_BUDGET_REFILL_PER_SEC)
            .min(OPEN_BUDGET_CAPACITY);
        self.last = now;
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn connection_permits_are_released_when_the_guard_drops() {
        let pool = Arc::new(Semaphore::new(10));
        let counts = Arc::new(DashMap::new());
        let user = Uuid::new_v4();
        let other = Uuid::new_v4();
        let permits = (0..MAX_EXEC_CONNECTIONS_PER_USER)
            .map(|_| {
                try_connection_permit_with(user, &pool, &counts)
                    .expect("connection below the per-user ceiling")
            })
            .collect::<Vec<_>>();
        assert!(try_connection_permit_with(user, &pool, &counts).is_none());
        assert!(try_connection_permit_with(other, &pool, &counts).is_some());
        drop(permits);
        assert!(!counts.contains_key(&user));
        assert!(try_connection_permit_with(user, &pool, &counts).is_some());
    }

    #[test]
    fn open_budget_has_a_bounded_burst_and_refills() {
        let mut budget = OpenBudget::new();
        for _ in 0..OPEN_BUDGET_CAPACITY as usize {
            assert!(budget.try_take());
        }
        assert!(!budget.try_take());
        budget.last -= Duration::from_secs(1);
        assert!(budget.try_take());
        assert!(!budget.try_take());
    }
}
