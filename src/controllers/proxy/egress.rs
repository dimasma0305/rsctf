//! Best-effort flag-egress detection for proxied game containers.

use uuid::Uuid;

use crate::app_state::SharedState;
use crate::services::flag_egress_observations::{Observation, ObservationKey, Queue};

use super::{GameAccess, InstanceAccess};

/// Context for the in-tunnel flag-egress scan. Cloneable so the bounded
/// observation queue can own a copy without retaining the proxy session.
#[derive(Clone)]
pub(super) struct EgressScan {
    queue: Queue,
    /// The owning team's current flag bytes for this challenge.
    pub(super) flag: Vec<u8>,
    game_id: i32,
    participation_id: i32,
    challenge_id: i32,
    container_id: Uuid,
    remote_ip: String,
}

/// Stream matcher retaining only the suffix that can begin a flag match in the
/// next read. Its memory use is fixed after construction and never grows with
/// the lifetime or byte volume of a proxy session.
pub(super) struct RollingFlagMatcher {
    overlap: Vec<u8>,
    max_overlap: usize,
}

impl RollingFlagMatcher {
    pub(super) fn new(flag: &[u8]) -> Self {
        let max_overlap = flag.len().saturating_sub(1);
        Self {
            overlap: Vec::with_capacity(max_overlap),
            max_overlap,
        }
    }

    /// Returns whether `chunk` completes a flag wholly within this read or
    /// across its boundary with prior reads.
    pub(super) fn contains(&mut self, flag: &[u8], chunk: &[u8]) -> bool {
        if flag.is_empty() {
            return false;
        }

        let within_chunk = chunk.windows(flag.len()).any(|window| window == flag);
        let max_left = self.overlap.len().min(flag.len().saturating_sub(1));
        let across_boundary = (1..=max_left).any(|left| {
            let right = flag.len() - left;
            right <= chunk.len()
                && self.overlap.ends_with(&flag[..left])
                && chunk.starts_with(&flag[left..])
        });

        self.retain_suffix(chunk);
        within_chunk || across_boundary
    }

    fn retain_suffix(&mut self, chunk: &[u8]) {
        if self.max_overlap == 0 {
            self.overlap.clear();
            return;
        }
        if chunk.len() >= self.max_overlap {
            self.overlap.clear();
            self.overlap
                .extend_from_slice(&chunk[chunk.len() - self.max_overlap..]);
            return;
        }

        let excess = self
            .overlap
            .len()
            .saturating_add(chunk.len())
            .saturating_sub(self.max_overlap);
        if excess > 0 {
            self.overlap.drain(..excess);
        }
        self.overlap.extend_from_slice(chunk);
    }
}

/// Load the owning team's flag for a proxied instance. `None` disables the
/// scan when there is no per-team flag or the context cannot be resolved.
pub(super) async fn build_egress_scan(
    st: &SharedState,
    access: &InstanceAccess,
    game: &GameAccess,
    remote_ip: String,
) -> Option<EgressScan> {
    let flag = sqlx::query_scalar::<_, String>(
        r#"SELECT flag.flag
             FROM "GameInstances" instance
             JOIN "FlagContexts" flag ON flag.id = instance.flag_id
            WHERE instance.participation_id = $1
              AND instance.challenge_id = $2
              AND instance.container_id = $3"#,
    )
    .bind(game.owner_participation_id)
    .bind(game.challenge_id)
    .bind(access.container_id)
    .fetch_optional(st.pg())
    .await
    .ok()??;
    if flag.is_empty() {
        return None;
    }
    Some(EgressScan {
        queue: st.flag_egress_observations.clone(),
        flag: flag.into_bytes(),
        game_id: game.game_id,
        participation_id: game.owner_participation_id,
        challenge_id: game.challenge_id,
        container_id: access.container_id,
        remote_ip,
    })
}

/// Non-blocking handoff to the single supervised batch writer.
pub(super) fn record_flag_egress(scan: &EgressScan) {
    if !scan.queue.enqueue(Observation {
        key: ObservationKey {
            game_id: scan.game_id,
            participation_id: scan.participation_id,
            challenge_id: scan.challenge_id,
            container_id: scan.container_id,
            remote_ip: scan.remote_ip.clone(),
        },
        observed_at: chrono::Utc::now(),
    }) {
        crate::services::flag_egress_observations::record_queue_drop();
    }
}

#[cfg(test)]
mod tests {
    use super::RollingFlagMatcher;

    #[test]
    fn matches_a_flag_at_every_read_boundary() {
        let flag = b"flag{split-across-tcp-reads}";
        for split in 1..flag.len() {
            let mut matcher = RollingFlagMatcher::new(flag);
            assert!(!matcher.contains(flag, &flag[..split]));
            assert!(matcher.contains(flag, &flag[split..]), "split={split}");
        }
    }

    #[test]
    fn matches_across_multiple_reads_and_keeps_only_bounded_overlap() {
        let flag = b"flag{three-reads}";
        let mut matcher = RollingFlagMatcher::new(flag);
        assert!(!matcher.contains(flag, b"noise-flag{"));
        assert!(!matcher.contains(flag, b"three-"));
        assert!(matcher.contains(flag, b"reads}-tail"));
        assert!(matcher.overlap.len() < flag.len());

        for _ in 0..100 {
            assert!(!matcher.contains(flag, b"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"));
            assert!(matcher.overlap.len() < flag.len());
        }
    }
}
