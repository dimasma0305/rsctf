use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use rsctf_worker_protocol::SessionFence;
use uuid::Uuid;

use crate::services::worker::SessionContext;

type HeartbeatKey = (Uuid, Uuid, u64);

/// Exact-session durable heartbeat coalescing. The live registry still sees
/// every heartbeat; this only bounds PostgreSQL lease-renewal writes.
#[derive(Clone)]
pub(super) struct HeartbeatWrites {
    interval: Duration,
    last_write: Arc<DashMap<HeartbeatKey, Instant>>,
}

impl HeartbeatWrites {
    pub(super) fn new(interval: Duration) -> Self {
        Self {
            interval,
            last_write: Arc::new(DashMap::new()),
        }
    }

    pub(super) fn opened(&self, worker_id: Uuid, fence: SessionFence) {
        self.opened_at(worker_id, fence, Instant::now());
    }

    pub(super) fn is_due(&self, session: &SessionContext) -> bool {
        self.is_due_at(session, Instant::now())
    }

    pub(super) fn closed(&self, session: &SessionContext) {
        self.last_write.remove(&key(session));
    }

    fn opened_at(&self, worker_id: Uuid, fence: SessionFence, now: Instant) {
        self.last_write
            .insert((worker_id, fence.session_id, fence.session_epoch), now);
    }

    fn is_due_at(&self, session: &SessionContext, now: Instant) -> bool {
        match self.last_write.entry(key(session)) {
            Entry::Occupied(mut entry) => {
                if now.saturating_duration_since(*entry.get()) < self.interval {
                    return false;
                }
                entry.insert(now);
                true
            }
            Entry::Vacant(entry) => {
                entry.insert(now);
                true
            }
        }
    }
}

fn key(session: &SessionContext) -> HeartbeatKey {
    (
        session.worker_id,
        session.fence.session_id,
        session.fence.session_epoch,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coalesces_only_the_same_exact_session() {
        let start = Instant::now();
        let writes = HeartbeatWrites::new(Duration::from_secs(10));
        let worker_id = Uuid::new_v4();
        let old = context(worker_id, 1);
        writes.opened_at(worker_id, old.fence, start);
        assert!(!writes.is_due_at(&old, start + Duration::from_secs(9)));
        assert!(writes.is_due_at(&old, start + Duration::from_secs(10)));
        assert!(!writes.is_due_at(&old, start + Duration::from_secs(11)));

        let newer = context(worker_id, 2);
        assert!(writes.is_due_at(&newer, start + Duration::from_secs(11)));
        writes.closed(&old);
        assert!(!writes.last_write.contains_key(&key(&old)));
        assert!(writes.last_write.contains_key(&key(&newer)));
    }

    fn context(worker_id: Uuid, session_epoch: u64) -> SessionContext {
        SessionContext {
            worker_id,
            boot_id: Uuid::new_v4(),
            certificate_fingerprint_sha256: [7; 32],
            fence: SessionFence {
                session_id: Uuid::new_v4(),
                session_epoch,
            },
        }
    }
}
