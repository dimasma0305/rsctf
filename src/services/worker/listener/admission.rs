use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub(super) struct HandshakeAdmission {
    maximum: usize,
    counts: Arc<Mutex<HashMap<IpAddr, usize>>>,
}

impl HandshakeAdmission {
    pub(super) fn new(maximum: usize) -> Self {
        Self {
            maximum,
            counts: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(super) fn try_admit(&self, peer: IpAddr) -> Option<PeerHandshakePermit> {
        let mut counts = self.counts.lock().ok()?;
        let count = counts.entry(peer).or_default();
        if *count >= self.maximum {
            return None;
        }
        *count += 1;
        Some(PeerHandshakePermit {
            peer,
            counts: self.counts.clone(),
        })
    }
}

pub(super) struct PeerHandshakePermit {
    peer: IpAddr,
    counts: Arc<Mutex<HashMap<IpAddr, usize>>>,
}

impl Drop for PeerHandshakePermit {
    fn drop(&mut self) {
        let Ok(mut counts) = self.counts.lock() else {
            return;
        };
        let Some(count) = counts.get_mut(&self.peer) else {
            return;
        };
        *count = count.saturating_sub(1);
        if *count == 0 {
            counts.remove(&self.peer);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permit_holds_the_peer_slot_until_application_setup_releases_it() {
        let admission = HandshakeAdmission::new(1);
        let peer: IpAddr = "192.0.2.10".parse().unwrap();
        let permit = admission.try_admit(peer).expect("first handshake");
        assert!(admission.try_admit(peer).is_none());

        drop(permit);
        assert!(admission.try_admit(peer).is_some());
    }

    #[test]
    fn distinct_peers_have_independent_handshake_budgets() {
        let admission = HandshakeAdmission::new(1);
        let first: IpAddr = "192.0.2.10".parse().unwrap();
        let second: IpAddr = "192.0.2.11".parse().unwrap();
        let _first = admission.try_admit(first).expect("first peer");
        assert!(admission.try_admit(second).is_some());
    }
}
