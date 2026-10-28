//! Response acceleration only: no credential consumption or authoritative CAS.
//! Independent hard shard budgets keep eviction/expiry work off unrelated keys.
use super::{Cache, InMemoryCache};
use bytes::Bytes;
use std::{
    hash::{BuildHasher, RandomState},
    sync::Arc,
    time::Duration,
};

const SHARDS: usize = 16;
const RESPONSE_ENTRIES: usize = super::L1_MAX_ENTRIES;
const RESPONSE_BYTES: usize = super::L1_MAX_BYTES;

struct MemoryShards {
    shards: [InMemoryCache; SHARDS],
    hash: RandomState,
}

impl MemoryShards {
    fn shard(&self, key: &str) -> &InMemoryCache {
        &self.shards[self.hash.hash_one(key) as usize % SHARDS]
    }
}

pub(crate) struct ResponseCache {
    memory: Arc<MemoryShards>,
}

/// Separate API and namespace; shares the same hard aggregate memory/entry budget.
/// This store has no shared-tier connection and cannot publish a secret to Redis.
pub(crate) struct LocalSecrets {
    memory: Arc<MemoryShards>,
}

impl LocalSecrets {
    fn key(key: &str) -> String {
        format!("\0local-secret:{key}")
    }
    pub(crate) async fn get(&self, key: &str) -> Option<Bytes> {
        let key = Self::key(key);
        self.memory.shard(&key).get(&key).await
    }
    pub(crate) async fn set(&self, key: &str, value: &[u8], ttl: Option<Duration>) {
        let key = Self::key(key);
        self.memory.shard(&key).set(&key, value, ttl).await;
    }
    pub(crate) async fn remove(&self, key: &str) {
        let key = Self::key(key);
        self.memory.shard(&key).remove(&key).await;
    }
}

impl ResponseCache {
    pub(crate) fn new() -> Self {
        Self {
            memory: Arc::new(MemoryShards {
                shards: std::array::from_fn(|_| {
                    InMemoryCache::with_limits(RESPONSE_ENTRIES / SHARDS, RESPONSE_BYTES / SHARDS)
                }),
                hash: RandomState::new(),
            }),
        }
    }
    pub(crate) fn local_secrets(&self) -> LocalSecrets {
        LocalSecrets {
            memory: self.memory.clone(),
        }
    }
    pub(crate) async fn get(&self, key: &str) -> Option<Bytes> {
        if key.starts_with('\0') {
            return None;
        }
        self.memory.shard(key).get(key).await
    }
    pub(crate) async fn set(&self, key: &str, value: &[u8], ttl: Option<Duration>) {
        if !key.starts_with('\0') {
            self.memory.shard(key).set(key, value, ttl).await;
        }
    }
    pub(crate) fn set_shared(&self, key: &str, value: Bytes, ttl: Option<Duration>) {
        if !key.starts_with('\0') {
            self.memory.shard(key).set_shared(key, value, ttl);
        }
    }
    pub(crate) async fn remove(&self, key: &str) {
        if !key.starts_with('\0') {
            self.memory.shard(key).remove(key).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn churn_obeys_hard_limits_and_expiry_without_copying_hits() {
        let cache = ResponseCache::new();
        let data = Bytes::from(vec![1; 8192]);
        for index in 0..RESPONSE_ENTRIES * 4 {
            cache.set_shared(&format!("response:{index}"), data.clone(), None);
        }
        let mut entries = 0;
        let mut bytes = 0;
        for shard in &cache.memory.shards {
            let state = shard.state.read().unwrap();
            assert!(state.map.len() <= RESPONSE_ENTRIES / SHARDS);
            assert!(state.payload_bytes <= RESPONSE_BYTES / SHARDS);
            entries += state.map.len();
            bytes += state.payload_bytes;
        }
        assert!(entries <= RESPONSE_ENTRIES && bytes <= RESPONSE_BYTES);
        cache.set_shared("shared", data.clone(), None);
        assert_eq!(cache.get("shared").await.unwrap().as_ptr(), data.as_ptr());
        cache.set_shared("shared", data, Some(Duration::ZERO));
        assert!(cache.get("shared").await.is_none());
        cache
            .set("oversize", &vec![0; RESPONSE_BYTES / SHARDS + 1], None)
            .await;
        assert!(cache.get("oversize").await.is_none());
        let secrets = cache.local_secrets();
        secrets.set("shared", b"secret", None).await;
        assert_eq!(
            secrets.get("shared").await.as_deref(),
            Some(b"secret".as_slice())
        );
        assert!(cache.get("shared").await.is_none());
        assert!(cache.get("\0local-secret:shared").await.is_none());
        cache
            .set("\0local-secret:shared", b"overwritten", None)
            .await;
        assert_eq!(
            secrets.get("shared").await.as_deref(),
            Some(b"secret".as_slice())
        );
    }
}
