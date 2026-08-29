//! Best-effort flag-egress detection for proxied game containers.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::app_state::SharedState;
use crate::services::flag_egress_observations::{Observation, ObservationKey, Queue};
use crate::utils::enums::ParticipationStatus;

use super::{GameAccess, InstanceAccess};

const EGRESS_METADATA_CACHE_ENTRIES: usize = 1_024;
const EGRESS_METADATA_CACHE_TTL: Duration = Duration::from_secs(60);
const EGRESS_METADATA_QUERY_DEADLINE: Duration = Duration::from_secs(2);
const EGRESS_METADATA_FLIGHTS: usize = 32;
const EGRESS_METADATA_QUERY_CONCURRENCY: usize = 16;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct EgressMetadataRevision {
    pub(super) challenge_configuration_revision: i64,
    pub(super) flag_id: i32,
}

#[derive(sqlx::FromRow)]
pub(super) struct EgressParticipationMetadata {
    pub(super) id: i32,
    pub(super) game_id: i32,
    pub(super) team_id: i32,
    pub(super) challenge_configuration_revision: i64,
}

/// Resolve the accepted owner and the caller's exact membership in one indexed
/// read while carrying the immutable event challenge revision into the scan.
pub(super) async fn load_egress_participation(
    pool: &sqlx::PgPool,
    participation_id: i32,
    user_id: Uuid,
) -> Option<EgressParticipationMetadata> {
    sqlx::query_as::<_, EgressParticipationMetadata>(
        r#"SELECT participation.id, participation.game_id,
                  participation.team_id, game.challenge_configuration_revision
             FROM "Participations" participation
             JOIN "Games" game ON game.id = participation.game_id
             JOIN "UserParticipations" membership
               ON membership.game_id = participation.game_id
              AND membership.participation_id = participation.id
              AND membership.user_id = $2
            WHERE participation.id = $1
              AND participation.status = $3
            LIMIT 1"#,
    )
    .bind(participation_id)
    .bind(user_id)
    .bind(ParticipationStatus::Accepted as i16)
    .fetch_optional(pool)
    .await
    .ok()?
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct EgressMetadataKey {
    game_id: i32,
    participation_id: i32,
    challenge_id: i32,
    container_id: Uuid,
    revision: EgressMetadataRevision,
}

struct CachedFlag {
    loaded_at: Instant,
    flag: Arc<[u8]>,
}

struct EgressMetadataCache {
    entries: Mutex<HashMap<EgressMetadataKey, CachedFlag>>,
    maximum: usize,
    ttl: Duration,
}

impl EgressMetadataCache {
    fn new(maximum: usize, ttl: Duration) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            maximum,
            ttl,
        }
    }

    fn get(&self, key: &EgressMetadataKey) -> Option<Arc<[u8]>> {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let expired = entries
            .get(key)
            .is_some_and(|entry| entry.loaded_at.elapsed() >= self.ttl);
        if expired {
            entries.remove(key);
            return None;
        }
        entries.get(key).map(|entry| entry.flag.clone())
    }

    fn store(&self, key: EgressMetadataKey, flag: Arc<[u8]>) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Instant::now();
        entries.retain(|_, entry| now.saturating_duration_since(entry.loaded_at) < self.ttl);
        if entries.len() >= self.maximum && !entries.contains_key(&key) {
            if let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, entry)| entry.loaded_at)
                .map(|(key, _)| *key)
            {
                entries.remove(&oldest);
            }
        }
        entries.insert(
            key,
            CachedFlag {
                loaded_at: now,
                flag,
            },
        );
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }
}

static EGRESS_METADATA_CACHE: LazyLock<EgressMetadataCache> = LazyLock::new(|| {
    EgressMetadataCache::new(EGRESS_METADATA_CACHE_ENTRIES, EGRESS_METADATA_CACHE_TTL)
});
static EGRESS_METADATA_SINGLE_FLIGHT: LazyLock<
    crate::utils::single_flight::SingleFlight<Option<Arc<[u8]>>>,
> = LazyLock::new(crate::utils::single_flight::SingleFlight::new);
static EGRESS_METADATA_QUERIES: tokio::sync::Semaphore =
    tokio::sync::Semaphore::const_new(EGRESS_METADATA_QUERY_CONCURRENCY);

/// Context for the in-tunnel flag-egress scan. Cloneable so the bounded
/// observation queue can own a copy without retaining the proxy session.
#[derive(Clone)]
pub(super) struct EgressScan {
    queue: Queue,
    /// The owning team's current flag bytes for this challenge.
    pub(super) flag: Arc<[u8]>,
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
    pub(super) fn new(flag: &[u8]) -> Option<Self> {
        let flag = std::str::from_utf8(flag).ok()?;
        crate::utils::flag_policy::validate_normal(flag).ok()?;
        let max_overlap = flag.len().saturating_sub(1);
        Some(Self {
            overlap: Vec::with_capacity(max_overlap),
            max_overlap,
        })
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
    let revision = game.egress_revision?;
    let key = EgressMetadataKey {
        game_id: game.game_id,
        participation_id: game.owner_participation_id,
        challenge_id: game.challenge_id,
        container_id: access.container_id,
        revision,
    };
    let flag = match EGRESS_METADATA_CACHE.get(&key) {
        Some(flag) => flag,
        None => {
            let pool = st.pg().clone();
            let flight_key = format!(
                "{}:{}:{}:{}:{}:{}",
                key.game_id,
                key.participation_id,
                key.challenge_id,
                key.container_id,
                key.revision.challenge_configuration_revision,
                key.revision.flag_id
            );
            EGRESS_METADATA_SINGLE_FLIGHT
                .run_with_limit(
                    &flight_key,
                    EGRESS_METADATA_QUERY_DEADLINE,
                    EGRESS_METADATA_FLIGHTS,
                    move || async move {
                        if let Some(flag) = EGRESS_METADATA_CACHE.get(&key) {
                            return Some(flag);
                        }
                        let _permit = EGRESS_METADATA_QUERIES.try_acquire().ok()?;
                        // Revalidate the exact instance/flag revision at the
                        // database boundary before publishing immutable bytes.
                        let flag = sqlx::query_scalar::<_, String>(
                            r#"SELECT flag.flag
                                 FROM "GameInstances" instance
                                 JOIN "Participations" participation
                                   ON participation.id = instance.participation_id
                                 JOIN "Games" game ON game.id = participation.game_id
                                 JOIN "FlagContexts" flag ON flag.id = instance.flag_id
                                WHERE instance.participation_id = $1
                                  AND instance.challenge_id = $2
                                  AND instance.container_id = $3
                                  AND participation.game_id = $4
                                  AND game.challenge_configuration_revision = $5
                                  AND flag.id = $6
                                  AND OCTET_LENGTH(flag.flag) BETWEEN 1 AND $7
                                  AND flag.flag !~ '(^[[:space:]])|([[:space:]]$)'"#,
                        )
                        .bind(key.participation_id)
                        .bind(key.challenge_id)
                        .bind(key.container_id)
                        .bind(key.game_id)
                        .bind(key.revision.challenge_configuration_revision)
                        .bind(key.revision.flag_id)
                        .bind(
                            i32::try_from(crate::utils::flag_policy::NORMAL_FLAG_MAX_BYTES)
                                .unwrap_or(127),
                        )
                        .fetch_optional(&pool)
                        .await
                        .ok()??;
                        if crate::utils::flag_policy::validate_normal(&flag).is_err() {
                            return None;
                        }
                        let flag = Arc::<[u8]>::from(flag.into_bytes());
                        EGRESS_METADATA_CACHE.store(key, Arc::clone(&flag));
                        Some(flag)
                    },
                )
                .await?
        }
    };
    Some(EgressScan {
        queue: st.flag_egress_observations.clone(),
        flag,
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
    use super::*;

    fn cache_key(revision: i64, flag_id: i32) -> EgressMetadataKey {
        EgressMetadataKey {
            game_id: 1,
            participation_id: 2,
            challenge_id: 3,
            container_id: Uuid::nil(),
            revision: EgressMetadataRevision {
                challenge_configuration_revision: revision,
                flag_id,
            },
        }
    }

    #[test]
    fn metadata_cache_is_bounded_and_revision_keyed() {
        let cache = EgressMetadataCache::new(2, Duration::from_secs(60));
        cache.store(cache_key(7, 11), Arc::from(&b"flag{one}"[..]));
        assert_eq!(&*cache.get(&cache_key(7, 11)).unwrap(), b"flag{one}");
        assert!(cache.get(&cache_key(8, 11)).is_none());
        assert!(cache.get(&cache_key(7, 12)).is_none());

        cache.store(cache_key(8, 12), Arc::from(&b"flag{two}"[..]));
        cache.store(cache_key(9, 13), Arc::from(&b"flag{three}"[..]));
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn matches_a_flag_at_every_read_boundary() {
        let flag = b"flag{split-across-tcp-reads}";
        for split in 1..flag.len() {
            let mut matcher = RollingFlagMatcher::new(flag).unwrap();
            assert!(!matcher.contains(flag, &flag[..split]));
            assert!(matcher.contains(flag, &flag[split..]), "split={split}");
        }
    }

    #[test]
    fn matches_across_multiple_reads_and_keeps_only_bounded_overlap() {
        let flag = b"flag{three-reads}";
        let mut matcher = RollingFlagMatcher::new(flag).unwrap();
        assert!(!matcher.contains(flag, b"noise-flag{"));
        assert!(!matcher.contains(flag, b"three-"));
        assert!(matcher.contains(flag, b"reads}-tail"));
        assert!(matcher.overlap.len() < flag.len());

        for _ in 0..100 {
            assert!(!matcher.contains(flag, b"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"));
            assert!(matcher.overlap.len() < flag.len());
        }
    }
    #[test]
    fn matcher_rejects_invalid_lengths_before_reserving_overlap() {
        assert!(RollingFlagMatcher::new(&[]).is_none());
        assert!(RollingFlagMatcher::new(&[b'x'; 128]).is_none());
        assert!(RollingFlagMatcher::new(b" flag{answer}").is_none());
    }

    #[test]
    fn many_proxy_sessions_keep_a_fixed_per_session_overlap_bound() {
        let flag = vec![b'x'; crate::utils::flag_policy::NORMAL_FLAG_MAX_BYTES];
        let matchers: Vec<_> = (0..4_096)
            .map(|_| RollingFlagMatcher::new(&flag).unwrap())
            .collect();
        assert!(matchers
            .iter()
            .all(|matcher| matcher.max_overlap < crate::utils::flag_policy::NORMAL_FLAG_MAX_BYTES));
        assert_eq!(
            matchers
                .iter()
                .map(|matcher| matcher.max_overlap)
                .sum::<usize>(),
            4_096 * (crate::utils::flag_policy::NORMAL_FLAG_MAX_BYTES - 1)
        );
    }
}
