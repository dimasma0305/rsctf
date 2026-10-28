use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

struct CountedCache {
    inner: InMemoryCache,
    reads: AtomicUsize,
}

impl CountedCache {
    fn new() -> Self {
        Self {
            inner: InMemoryCache::new(),
            reads: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl Cache for CountedCache {
    async fn get(&self, key: &str) -> Option<Bytes> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.inner.get(key).await
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
    async fn remove(&self, key: &str) {
        self.inner.remove(key).await;
    }
}

#[tokio::test]
async fn l2_fill_and_l1_hits_share_one_immutable_payload() {
    let shared = Arc::new(CountedCache::new());
    shared.set("response", &vec![0x5a; 512 * 1024], None).await;
    let tiered = TieredCache::new(shared.clone(), Duration::from_secs(60));
    let first = tiered.get("response").await.unwrap();
    let cached = tiered.get("response").await.unwrap();
    assert_eq!(
        first.as_ptr(),
        cached.as_ptr(),
        "L1 fill must retain the L2 allocation"
    );
    assert_eq!(
        shared.reads.load(Ordering::Relaxed),
        1,
        "a hot read must not access L2"
    );
    shared.remove("response").await;
    drop(first);
    assert_eq!(cached.len(), 512 * 1024);
    assert!(cached.iter().all(|byte| *byte == 0x5a));
    assert!(tiered.get_authoritative("response").await.is_none());
    assert_eq!(
        tiered.l1.get("response").await.unwrap().as_ptr(),
        cached.as_ptr()
    );
    tiered.remove("response").await;
    assert!(tiered.get("response").await.is_none());
}

#[tokio::test]
async fn shared_values_preserve_key_byte_budget_and_fifo_eviction() {
    let cache = InMemoryCache::with_limits(2, 10);
    for (key, value) in [("a", "123"), ("b", "456"), ("c", "789")] {
        cache.set_shared(key, Bytes::copy_from_slice(value.as_bytes()), None);
    }
    assert!(cache.get("a").await.is_none());
    assert_eq!(cache.get("b").await.unwrap().as_ref(), b"456");
    assert_eq!(cache.get("c").await.unwrap().as_ref(), b"789");
    {
        let state = cache.state.read().unwrap();
        assert_eq!(state.payload_bytes, 8);
        assert_eq!(state.map.len(), 2);
    }
    cache.set_shared("c", Bytes::from(vec![0; 11]), None);
    assert!(
        cache.get("c").await.is_none(),
        "oversized replacement keeps existing removal semantics"
    );
    assert_eq!(cache.get("b").await.unwrap().as_ref(), b"456");
}

#[tokio::test]
async fn borrowed_writes_still_own_a_copy_of_mutable_caller_data() {
    let cache = InMemoryCache::new();
    let mut input = b"original".to_vec();
    cache.set("borrowed", &input, None).await;
    input.fill(b'x');
    assert_eq!(cache.get("borrowed").await.unwrap().as_ref(), b"original");
}

#[tokio::test]
async fn shared_fill_does_not_extend_zero_l1_ttl() {
    let shared = Arc::new(CountedCache::new());
    shared.set("short", b"value", None).await;
    let tiered = TieredCache::new(shared.clone(), Duration::ZERO);
    assert_eq!(tiered.get("short").await.unwrap().as_ref(), b"value");
    assert!(tiered.l1.get("short").await.is_none());
    shared.remove("short").await;
    assert!(tiered.get("short").await.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shared_l1_payload_cannot_satisfy_multiple_authoritative_consumers() {
    let shared = Arc::new(InMemoryCache::new());
    shared.set("one-time", b"secret", None).await;
    let tiered = Arc::new(TieredCache::new(shared.clone(), Duration::from_secs(60)));
    tiered.get("one-time").await.unwrap();
    let mut consumers = Vec::new();
    for _ in 0..32 {
        let tiered = Arc::clone(&tiered);
        consumers.push(tokio::spawn(async move {
            tiered.get_and_remove("one-time").await
        }));
    }
    let mut consumed = 0;
    for consumer in consumers {
        consumed += usize::from(consumer.await.unwrap().is_some());
    }
    assert_eq!(consumed, 1);
    assert!(tiered.l1.get("one-time").await.is_none());
    shared.set("compare", b"old", None).await;
    tiered.get("compare").await.unwrap();
    shared.set("compare", b"new", None).await;
    assert!(!tiered.compare_and_remove("compare", b"old").await);
    assert!(tiered.l1.get("compare").await.is_none());
    assert_eq!(
        tiered.get_authoritative("compare").await.unwrap().as_ref(),
        b"new"
    );
}

#[tokio::test]
#[ignore = "requires a disposable Redis server in RSCTF_TEST_REDIS_URL"]
async fn real_redis_l1_fill_shares_payload_and_preserves_authoritative_operations() {
    let url = std::env::var("RSCTF_TEST_REDIS_URL").expect("RSCTF_TEST_REDIS_URL is required");
    let shared: Arc<dyn Cache> = Arc::new(RedisCache::connect(&url).await.unwrap());
    let key = format!("cache-shared-l1-test:{}", uuid::Uuid::new_v4());
    shared
        .set(&key, &vec![0x5a; 512 * 1024], Some(Duration::from_secs(60)))
        .await;
    let tiered = TieredCache::new(shared.clone(), Duration::from_secs(60));
    let response = tiered.get(&key).await.unwrap();
    assert_eq!(
        response.as_ptr(),
        tiered.l1.get(&key).await.unwrap().as_ptr()
    );
    assert_eq!(tiered.backend_health().await, CacheBackendHealth::Ready);
    assert_eq!(
        tiered.get_and_remove(&key).await.unwrap().len(),
        response.len()
    );
    assert!(tiered.l1.get(&key).await.is_none());
    assert!(tiered.get_authoritative(&key).await.is_none());
    assert_eq!(response.len(), 512 * 1024);
    assert!(response.iter().all(|byte| *byte == 0x5a));
}

#[tokio::test]
async fn in_memory_get_and_remove_is_single_use() {
    let cache = InMemoryCache::new();
    cache
        .set("one-time", b"value", Some(Duration::from_secs(60)))
        .await;
    assert_eq!(
        cache.get_and_remove("one-time").await.as_deref(),
        Some(b"value".as_slice())
    );
    assert!(cache.get_and_remove("one-time").await.is_none());
    assert!(cache.get("one-time").await.is_none());
}

#[tokio::test]
async fn in_memory_compare_and_remove_checks_value() {
    let cache = InMemoryCache::new();
    cache.set("current", b"new", None).await;
    assert!(!cache.compare_and_remove("current", b"old").await);
    assert!(cache.compare_and_remove("current", b"new").await);
    assert!(cache.get("current").await.is_none());
}

#[tokio::test]
async fn in_memory_set_if_absent_never_overwrites_a_live_value() {
    let cache = InMemoryCache::new();
    assert!(
        cache
            .set_if_absent("reserved", b"first", Some(Duration::from_secs(60)))
            .await
    );
    assert!(
        !cache
            .set_if_absent("reserved", b"second", Some(Duration::from_secs(60)))
            .await
    );
    assert_eq!(
        cache.get("reserved").await.as_deref(),
        Some(b"first".as_slice())
    );

    cache.set("expired", b"old", Some(Duration::ZERO)).await;
    assert!(cache.set_if_absent("expired", b"fresh", None).await);
    assert_eq!(
        cache.get("expired").await.as_deref(),
        Some(b"fresh".as_slice())
    );
}

#[tokio::test]
async fn in_memory_cache_evicts_old_entries_at_the_entry_limit() {
    let cache = InMemoryCache::with_limits(2, 1_024);
    cache.set("first", b"1", None).await;
    cache.set("second", b"2", None).await;
    cache.set("third", b"3", None).await;

    assert!(cache.get("first").await.is_none());
    assert_eq!(cache.get("second").await.as_deref(), Some(b"2".as_slice()));
    assert_eq!(cache.get("third").await.as_deref(), Some(b"3".as_slice()));
}

#[tokio::test]
async fn in_memory_cache_enforces_payload_bytes_and_sweeps_expiry() {
    let cache = InMemoryCache::with_limits(8, 10);
    cache.set("old", b"123", Some(Duration::ZERO)).await;
    cache.set("large", b"12345", None).await;

    // `large` occupies exactly ten key+value bytes. The expired entry was
    // swept before pressure eviction, so the live value remains available.
    assert!(cache.get("old").await.is_none());
    assert_eq!(
        cache.get("large").await.as_deref(),
        Some(b"12345".as_slice())
    );

    cache.set("too-large", b"12", None).await;
    assert!(cache.get("too-large").await.is_none());
}

#[tokio::test]
async fn in_memory_hits_clone_bytes_without_copying_payload() {
    let cache = InMemoryCache::new();
    cache.set("shared", b"payload", None).await;
    let first = cache.get("shared").await.unwrap();
    let second = cache.get("shared").await.unwrap();
    assert_eq!(first.as_ptr(), second.as_ptr());
}

#[tokio::test]
async fn authoritative_read_bypasses_another_replicas_stale_l1() {
    let shared: Arc<dyn Cache> = Arc::new(InMemoryCache::new());
    let first = TieredCache::new(Arc::clone(&shared), Duration::from_secs(60));
    let second = TieredCache::new(shared, Duration::from_secs(60));

    first.set("version", b"old", None).await;
    assert_eq!(
        first.get("version").await.as_deref(),
        Some(b"old".as_slice())
    );

    second.set_authoritative("version", b"new", None).await;
    assert_eq!(
        first.get("version").await.as_deref(),
        Some(b"old".as_slice())
    );
    assert_eq!(
        first.get_authoritative("version").await.as_deref(),
        Some(b"new".as_slice())
    );
}

#[tokio::test]
async fn local_only_values_never_reach_the_shared_tier_or_another_replica() {
    let shared: Arc<dyn Cache> = Arc::new(InMemoryCache::new());
    let first = TieredCache::new(Arc::clone(&shared), Duration::from_secs(60));
    let second = TieredCache::new(Arc::clone(&shared), Duration::from_secs(60));

    first
        .set_local("bearer", b"plaintext-secret", Some(Duration::from_secs(10)))
        .await;
    assert_eq!(
        first.get_local("bearer").await.as_deref(),
        Some(b"plaintext-secret".as_slice())
    );
    assert!(shared.get("bearer").await.is_none());
    assert!(second.get_local("bearer").await.is_none());
    assert!(
        first.get("bearer").await.is_none(),
        "a secret is not a response entry"
    );
    first.set("bearer", b"public-response", None).await;
    assert_eq!(
        first.get_local("bearer").await.as_deref(),
        Some(b"plaintext-secret".as_slice())
    );
    assert_eq!(
        first.get("bearer").await.as_deref(),
        Some(b"public-response".as_slice())
    );
    first.remove("bearer").await;
    assert!(first.get_local("bearer").await.is_none());
}
