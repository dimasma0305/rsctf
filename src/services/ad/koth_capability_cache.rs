//! Bounded, revocation-versioned cache keys for player-visible KotH bearers.

use std::time::Duration;

use crate::services::cache::Cache;
use crate::utils::error::{AppError, AppResult};

/// A five-second player poll can reuse one model once, while an unreachable
/// version is removed promptly even if no later request touches it.
pub(crate) const TOKEN_MODEL_CACHE_TTL: Duration = Duration::from_secs(10);
const CAPABILITY_EPOCH_TTL: Duration = Duration::from_secs(30);
// Mutation markers cannot expire: PostgreSQL lock and commit waits have no
// matching hard deadline. A crash leaves one bounded disabled key for the game;
// the next serialized mutation replaces it and restores a cacheable epoch.
const CAPABILITY_MUTATION_MARKER_TTL: Option<Duration> = None;
const CAPABILITY_EPOCH_ENTROPY_BYTES: usize = 12;
const CAPABILITY_EPOCH_ENCODED_LEN: usize = 16;

fn game_epoch_key(game_id: i32) -> String {
    format!("koth-capability:v1:epoch:{game_id}")
}

fn decode_epoch(bytes: &[u8]) -> Option<String> {
    let value = std::str::from_utf8(bytes).ok()?;
    (value.len() == CAPABILITY_EPOCH_ENCODED_LEN
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')))
    .then(|| value.to_owned())
}

fn fresh_epoch() -> String {
    crate::utils::codec::random_token(CAPABILITY_EPOCH_ENTROPY_BYTES)
}

fn mutation_marker() -> String {
    format!("mutating:{}", fresh_epoch())
}

/// Opaque ownership ticket for one in-flight database mutation. Finalization
/// may replace only the exact marker installed by this mutation; otherwise an
/// older writer could re-enable caching while a newer writer is uncommitted.
#[derive(Debug)]
pub(crate) struct GameEpochMutation {
    marker: String,
}

/// Return the shared version pointer for one game. A missing or unavailable
/// authoritative tier disables the response cache for this request; callers
/// then read PostgreSQL instead of trusting a replica-local stale pointer.
pub(crate) async fn current_game_epoch(cache: &dyn Cache, game_id: i32) -> Option<String> {
    let key = game_epoch_key(game_id);
    if let Some(value) = cache.get_authoritative(&key).await {
        return decode_epoch(&value);
    }

    let candidate = fresh_epoch();
    if cache
        .set_if_absent_authoritative(&key, candidate.as_bytes(), Some(CAPABILITY_EPOCH_TTL))
        .await
    {
        return Some(candidate);
    }
    cache
        .get_authoritative(&key)
        .await
        .and_then(|value| decode_epoch(&value))
}

/// Make the current namespace uncacheable before a capability transaction is
/// allowed to commit. If the authoritative backend cannot verify this marker,
/// the caller must roll its database transaction back. A failed final publish
/// safely leaves this invalid marker in place, so reads continue against live
/// PostgreSQL without using local bearer models.
pub(crate) async fn begin_game_epoch_mutation(
    cache: &dyn Cache,
    game_id: i32,
) -> AppResult<GameEpochMutation> {
    let marker = mutation_marker();
    if cache
        .set_authoritative_checked(
            &game_epoch_key(game_id),
            marker.as_bytes(),
            CAPABILITY_MUTATION_MARKER_TTL,
        )
        .await
    {
        Ok(GameEpochMutation { marker })
    } else {
        Err(AppError::unavailable(
            "KotH capability cache fence is unavailable; retry this mutation",
        ))
    }
}

/// Publish the final cacheable namespace after a capability transaction, but
/// only if this mutation still owns the authoritative marker. A newer writer's
/// marker always wins. If the compare-and-set fails or Redis is unavailable,
/// readers stay on live PostgreSQL until a later serialized mutation restores
/// an enabled epoch.
pub(crate) async fn finish_game_epoch_mutation(
    cache: &dyn Cache,
    game_id: i32,
    mutation: GameEpochMutation,
) -> bool {
    let epoch = fresh_epoch();
    cache
        .compare_and_set_authoritative(
            &game_epoch_key(game_id),
            mutation.marker.as_bytes(),
            epoch.as_bytes(),
            Some(CAPABILITY_EPOCH_TTL),
        )
        .await
}

pub(crate) async fn begin_game_epoch_mutation_if(
    cache: &dyn Cache,
    game_id: i32,
    capability_changed: bool,
) -> AppResult<Option<GameEpochMutation>> {
    if capability_changed {
        Ok(Some(begin_game_epoch_mutation(cache, game_id).await?))
    } else {
        Ok(None)
    }
}

pub(crate) async fn finish_game_epoch_mutation_if_any(
    cache: &dyn Cache,
    game_id: i32,
    mutation: Option<GameEpochMutation>,
) {
    if let Some(mutation) = mutation {
        finish_game_epoch_mutation(cache, game_id, mutation).await;
    }
}

/// Commit a game-control transaction with a two-phase capability-cache fence.
/// The invalid marker is verified while the exclusive game lock is retained;
/// the final enabled epoch is published only after the database commit.
pub(crate) async fn release_game_control(
    control: crate::services::ad::engine::koth_auth::GameControlLock,
    cache: &dyn Cache,
    game_id: i32,
    capability_changed: bool,
) -> AppResult<()> {
    let mutation = begin_game_epoch_mutation_if(cache, game_id, capability_changed).await?;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    finish_game_epoch_mutation_if_any(cache, game_id, mutation).await;
    Ok(())
}

pub(crate) fn hill_token_key(
    game_id: i32,
    challenge_id: i32,
    participation_id: i32,
    round: i32,
    epoch: &str,
) -> String {
    format!("koth-capability:v1:hill:{game_id}:{challenge_id}:{participation_id}:{round}:{epoch}")
}

pub(crate) fn all_tokens_key(
    game_id: i32,
    participation_id: i32,
    round: i32,
    epoch: &str,
) -> String {
    format!("koth-capability:v1:all:{game_id}:{participation_id}:{round}:{epoch}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use crate::services::cache::{InMemoryCache, TieredCache};
    use bytes::Bytes;

    struct LimitedAuthoritativeWrites {
        inner: InMemoryCache,
        remaining: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl Cache for LimitedAuthoritativeWrites {
        async fn get(&self, key: &str) -> Option<Bytes> {
            self.inner.get(key).await
        }

        async fn get_local(&self, key: &str) -> Option<Bytes> {
            self.inner.get_local(key).await
        }

        async fn get_and_remove(&self, key: &str) -> Option<Bytes> {
            self.inner.get_and_remove(key).await
        }

        async fn compare_and_remove(&self, key: &str, expected: &[u8]) -> bool {
            self.inner.compare_and_remove(key, expected).await
        }

        async fn set_if_absent(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> bool {
            self.inner.set_if_absent(key, value, ttl).await
        }

        async fn set(&self, key: &str, value: &[u8], ttl: Option<Duration>) {
            self.inner.set(key, value, ttl).await;
        }

        async fn set_local(&self, key: &str, value: &[u8], ttl: Option<Duration>) {
            self.inner.set_local(key, value, ttl).await;
        }

        async fn set_authoritative(&self, key: &str, value: &[u8], ttl: Option<Duration>) {
            if self
                .remaining
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                self.inner.set(key, value, ttl).await;
            }
        }

        async fn compare_and_set_authoritative(
            &self,
            key: &str,
            expected: &[u8],
            value: &[u8],
            ttl: Option<Duration>,
        ) -> bool {
            if self
                .remaining
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_err()
            {
                return false;
            }
            self.inner
                .compare_and_set_authoritative(key, expected, value, ttl)
                .await
        }

        async fn remove(&self, key: &str) {
            self.inner.remove(key).await;
        }
    }

    #[tokio::test]
    async fn rotation_makes_a_filled_response_namespace_unreachable() {
        let cache = InMemoryCache::new();
        let first = current_game_epoch(&cache, 7).await.unwrap();
        let old_key = hill_token_key(7, 9, 11, 3, &first);
        cache
            .set_local(&old_key, b"old bearer", Some(TOKEN_MODEL_CACHE_TTL))
            .await;

        let mutation = begin_game_epoch_mutation(&cache, 7).await.unwrap();
        assert!(finish_game_epoch_mutation(&cache, 7, mutation).await);
        let second = current_game_epoch(&cache, 7).await.unwrap();
        assert_ne!(second, first);
        assert_ne!(hill_token_key(7, 9, 11, 3, &second), old_key);
        assert!(cache.get_local(&old_key).await.is_some());
    }

    #[tokio::test]
    async fn two_phase_rotation_rejects_a_cross_replica_racing_fill() {
        let shared: Arc<dyn Cache> = Arc::new(InMemoryCache::new());
        let reader = TieredCache::new(Arc::clone(&shared), Duration::from_secs(60));
        let writer = TieredCache::new(Arc::clone(&shared), Duration::from_secs(60));

        let before = current_game_epoch(&reader, 7).await.unwrap();
        let mutation = begin_game_epoch_mutation(&writer, 7).await.unwrap();
        assert!(current_game_epoch(&reader, 7).await.is_none());

        assert!(finish_game_epoch_mutation(&writer, 7, mutation).await);
        let after = current_game_epoch(&reader, 7).await.unwrap();
        assert_ne!(after, before);
        assert!(reader
            .get_local(&hill_token_key(7, 9, 11, 3, &after))
            .await
            .is_none());
        assert!(shared
            .get(&hill_token_key(7, 9, 11, 3, &after))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn asymmetric_authoritative_failure_aborts_or_leaves_cache_disabled() {
        let unavailable = LimitedAuthoritativeWrites {
            inner: InMemoryCache::new(),
            remaining: AtomicUsize::new(0),
        };
        let old_epoch = fresh_epoch();
        unavailable
            .inner
            .set(&game_epoch_key(7), old_epoch.as_bytes(), None)
            .await;
        assert!(begin_game_epoch_mutation(&unavailable, 7).await.is_err());
        assert_eq!(current_game_epoch(&unavailable, 7).await, Some(old_epoch));

        let post_commit_failure = LimitedAuthoritativeWrites {
            inner: InMemoryCache::new(),
            remaining: AtomicUsize::new(1),
        };
        let mutation = begin_game_epoch_mutation(&post_commit_failure, 8)
            .await
            .unwrap();
        assert!(!finish_game_epoch_mutation(&post_commit_failure, 8, mutation).await);
        assert!(current_game_epoch(&post_commit_failure, 8).await.is_none());
    }

    #[tokio::test]
    async fn stale_finalizer_cannot_overwrite_a_newer_replica_mutation_marker() {
        let shared: Arc<dyn Cache> = Arc::new(LimitedAuthoritativeWrites {
            inner: InMemoryCache::new(),
            remaining: AtomicUsize::new(2),
        });
        let first = TieredCache::new(Arc::clone(&shared), Duration::from_secs(60));
        let second = TieredCache::new(Arc::clone(&shared), Duration::from_secs(60));

        let first_mutation = begin_game_epoch_mutation(&first, 9).await.unwrap();
        let second_mutation = begin_game_epoch_mutation(&second, 9).await.unwrap();
        assert!(!finish_game_epoch_mutation(&first, 9, first_mutation).await);
        assert!(!finish_game_epoch_mutation(&second, 9, second_mutation).await);
        assert!(current_game_epoch(&first, 9).await.is_none());
        assert!(current_game_epoch(&second, 9).await.is_none());
    }

    #[tokio::test]
    async fn newest_replica_mutation_can_publish_after_a_stale_finalizer() {
        let shared: Arc<dyn Cache> = Arc::new(InMemoryCache::new());
        let first = TieredCache::new(Arc::clone(&shared), Duration::from_secs(60));
        let second = TieredCache::new(shared, Duration::from_secs(60));

        let before = current_game_epoch(&first, 10).await.unwrap();
        let first_mutation = begin_game_epoch_mutation(&first, 10).await.unwrap();
        let second_mutation = begin_game_epoch_mutation(&second, 10).await.unwrap();
        assert!(!finish_game_epoch_mutation(&first, 10, first_mutation).await);
        assert!(current_game_epoch(&first, 10).await.is_none());
        assert!(finish_game_epoch_mutation(&second, 10, second_mutation).await);
        let after = current_game_epoch(&first, 10).await.unwrap();
        assert_ne!(after, before);
    }

    #[test]
    fn cache_contract_bounds_models_and_scopes_keys() {
        assert_eq!(TOKEN_MODEL_CACHE_TTL, Duration::from_secs(10));
        assert!(CAPABILITY_EPOCH_TTL > TOKEN_MODEL_CACHE_TTL);
        assert!(CAPABILITY_MUTATION_MARKER_TTL.is_none());
        let first = hill_token_key(7, 9, 11, 3, "epoch");
        assert_ne!(first, hill_token_key(7, 9, 12, 3, "epoch"));
        assert_ne!(first, hill_token_key(7, 10, 11, 3, "epoch"));
        assert_ne!(first, hill_token_key(7, 9, 11, 4, "epoch"));
        assert_ne!(first, hill_token_key(7, 9, 11, 3, "new-epoch"));
    }
}
