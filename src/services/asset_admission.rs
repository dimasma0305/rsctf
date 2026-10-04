use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use uuid::Uuid;

// 128 KiB per active local stream makes the global worst-case buffer budget
// 256 MiB per replica. Ordinary browsers use one stream; common segmented
// downloaders remain comfortably below the per-user ceiling.
const MAX_GLOBAL_STREAMS: usize = 2_048;
const MAX_PER_USER_STREAMS: usize = 16;
const MAX_PER_HASH_STREAMS: usize = 1_536;
const MAX_ACTIVE_REQUESTS: usize = 512;
const MAX_PER_SOURCE_REQUESTS: usize = 32;
const MAX_PER_HASH_REQUESTS: usize = 64;
const MAX_DISTINCT_REQUEST_HASHES: usize = 256;

#[derive(Clone)]
pub struct AssetDownloadAdmission {
    inner: Arc<Inner>,
}

struct Inner {
    global: AtomicUsize,
    users: DashMap<Uuid, Arc<AtomicUsize>>,
    hashes: DashMap<String, Arc<AtomicUsize>>,
    requests: AtomicUsize,
    sources: DashMap<String, Arc<AtomicUsize>>,
    request_hashes: DashMap<String, Arc<AtomicUsize>>,
    request_hash_count: AtomicUsize,
}

pub struct AssetDownloadPermit {
    admission: AssetDownloadAdmission,
    user: Option<(Uuid, Arc<AtomicUsize>)>,
    hash: (String, Arc<AtomicUsize>),
}

/// Cheap request-work admission acquired before authorization, cache, SQL, or
/// storage. It intentionally has a lower ceiling than byte streaming: a
/// rotating unknown-hash flood should fail before it can allocate a cache-fill
/// key or check out a PostgreSQL connection.
pub struct AssetRequestPermit {
    admission: AssetDownloadAdmission,
    source: (String, Arc<AtomicUsize>),
    hash: (String, Arc<AtomicUsize>),
}

impl AssetDownloadAdmission {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                global: AtomicUsize::new(0),
                users: DashMap::new(),
                hashes: DashMap::new(),
                requests: AtomicUsize::new(0),
                sources: DashMap::new(),
                request_hashes: DashMap::new(),
                request_hash_count: AtomicUsize::new(0),
            }),
        }
    }

    pub fn try_acquire_request(&self, source: &str, hash: &str) -> Option<AssetRequestPermit> {
        // Take the constant-cardinality deployment ceiling first. A rotating
        // source/hash flood must fail before it can allocate caller-controlled
        // entries in either DashMap.
        if self
            .inner
            .requests
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAX_ACTIVE_REQUESTS).then_some(active + 1)
            })
            .is_err()
        {
            return None;
        }
        let source_key = source.to_string();
        let source_counter = match increment(
            &self.inner.sources,
            source_key.clone(),
            MAX_PER_SOURCE_REQUESTS,
        ) {
            Some(counter) => counter,
            None => {
                self.inner.requests.fetch_sub(1, Ordering::AcqRel);
                return None;
            }
        };
        let hash_key = hash.to_string();
        let hash_counter = match increment_distinct_bounded(
            &self.inner.request_hashes,
            hash_key.clone(),
            MAX_PER_HASH_REQUESTS,
            MAX_DISTINCT_REQUEST_HASHES,
            &self.inner.request_hash_count,
        ) {
            Some(counter) => counter,
            None => {
                release(&self.inner.sources, source_key, &source_counter);
                self.inner.requests.fetch_sub(1, Ordering::AcqRel);
                return None;
            }
        };
        Some(AssetRequestPermit {
            admission: self.clone(),
            source: (source_key, source_counter),
            hash: (hash_key, hash_counter),
        })
    }

    pub fn try_acquire(&self, user_id: Option<Uuid>, hash: &str) -> Option<AssetDownloadPermit> {
        let user = match user_id {
            Some(user_id) => {
                let counter = increment(&self.inner.users, user_id, MAX_PER_USER_STREAMS)?;
                Some((user_id, counter))
            }
            None => None,
        };
        let hash_key = hash.to_string();
        let hash_counter =
            match increment(&self.inner.hashes, hash_key.clone(), MAX_PER_HASH_STREAMS) {
                Some(counter) => counter,
                None => {
                    if let Some((user_id, counter)) = &user {
                        release(&self.inner.users, *user_id, counter);
                    }
                    return None;
                }
            };
        if self
            .inner
            .global
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAX_GLOBAL_STREAMS).then_some(active + 1)
            })
            .is_err()
        {
            release(&self.inner.hashes, hash_key, &hash_counter);
            if let Some((user_id, counter)) = &user {
                release(&self.inner.users, *user_id, counter);
            }
            return None;
        }

        Some(AssetDownloadPermit {
            admission: self.clone(),
            user,
            hash: (hash_key, hash_counter),
        })
    }
}

impl Default for AssetDownloadAdmission {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for AssetDownloadPermit {
    fn drop(&mut self) {
        self.admission.inner.global.fetch_sub(1, Ordering::AcqRel);
        release(
            &self.admission.inner.hashes,
            self.hash.0.clone(),
            &self.hash.1,
        );
        if let Some((user_id, counter)) = &self.user {
            release(&self.admission.inner.users, *user_id, counter);
        }
    }
}

impl Drop for AssetRequestPermit {
    fn drop(&mut self) {
        self.admission.inner.requests.fetch_sub(1, Ordering::AcqRel);
        release_distinct(
            &self.admission.inner.request_hashes,
            self.hash.0.clone(),
            &self.hash.1,
            &self.admission.inner.request_hash_count,
        );
        release(
            &self.admission.inner.sources,
            self.source.0.clone(),
            &self.source.1,
        );
    }
}

fn increment<K>(
    map: &DashMap<K, Arc<AtomicUsize>>,
    key: K,
    limit: usize,
) -> Option<Arc<AtomicUsize>>
where
    K: Eq + std::hash::Hash + Clone,
{
    let counter = map
        .entry(key)
        .or_insert_with(|| Arc::new(AtomicUsize::new(0)))
        .clone();
    counter
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
            (value < limit).then_some(value + 1)
        })
        .ok()
        .map(|_| counter)
}

fn increment_distinct_bounded<K>(
    map: &DashMap<K, Arc<AtomicUsize>>,
    key: K,
    per_key_limit: usize,
    distinct_limit: usize,
    distinct_count: &AtomicUsize,
) -> Option<Arc<AtomicUsize>>
where
    K: Eq + std::hash::Hash + Clone,
{
    match map.entry(key) {
        Entry::Occupied(entry) => {
            let counter = entry.get().clone();
            counter
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                    (value < per_key_limit).then_some(value + 1)
                })
                .ok()
                .map(|_| counter)
        }
        Entry::Vacant(entry) => {
            distinct_count
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                    (value < distinct_limit).then_some(value + 1)
                })
                .ok()?;
            let counter = Arc::new(AtomicUsize::new(1));
            entry.insert(counter.clone());
            Some(counter)
        }
    }
}

fn release_distinct<K>(
    map: &DashMap<K, Arc<AtomicUsize>>,
    key: K,
    counter: &Arc<AtomicUsize>,
    distinct_count: &AtomicUsize,
) where
    K: Eq + std::hash::Hash + Clone,
{
    if counter.fetch_sub(1, Ordering::AcqRel) != 1 {
        return;
    }
    if let Entry::Occupied(entry) = map.entry(key) {
        if Arc::ptr_eq(entry.get(), counter)
            && counter.load(Ordering::Acquire) == 0
            && Arc::strong_count(counter) == 2
        {
            entry.remove();
            distinct_count.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

fn release<K>(map: &DashMap<K, Arc<AtomicUsize>>, key: K, counter: &Arc<AtomicUsize>)
where
    K: Eq + std::hash::Hash + Clone,
{
    if counter.fetch_sub(1, Ordering::AcqRel) != 1 {
        return;
    }
    if let Entry::Occupied(entry) = map.entry(key) {
        if Arc::ptr_eq(entry.get(), counter)
            && counter.load(Ordering::Acquire) == 0
            && Arc::strong_count(counter) == 2
        {
            entry.remove();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segmented_download_limit_releases_after_stream_completion() {
        let admission = AssetDownloadAdmission::new();
        let user = Uuid::new_v4();
        let hash = "a".repeat(64);
        let permits = (0..MAX_PER_USER_STREAMS)
            .map(|_| admission.try_acquire(Some(user), &hash).unwrap())
            .collect::<Vec<_>>();
        assert!(admission.try_acquire(Some(user), &hash).is_none());
        drop(permits);
        assert!(admission.try_acquire(Some(user), &hash).is_some());
    }

    #[test]
    fn anonymous_and_authenticated_streams_share_the_hash_ceiling() {
        let admission = AssetDownloadAdmission::new();
        let hash = "b".repeat(64);
        let permits = (0..MAX_PER_HASH_STREAMS)
            .map(|index| {
                let user = (index % 2 == 0).then(Uuid::new_v4);
                admission.try_acquire(user, &hash).unwrap()
            })
            .collect::<Vec<_>>();
        assert!(admission.try_acquire(None, &hash).is_none());
        drop(permits);
    }

    #[test]
    fn rotating_hashes_and_one_source_are_bounded_and_release() {
        let admission = AssetDownloadAdmission::new();
        let source = "203.0.113.9";
        let permits = (0..MAX_PER_SOURCE_REQUESTS)
            .map(|index| {
                admission
                    .try_acquire_request(source, &format!("{index:064x}"))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(admission
            .try_acquire_request(source, &"f".repeat(64))
            .is_none());
        drop(permits);
        assert!(admission
            .try_acquire_request(source, &"f".repeat(64))
            .is_some());
    }

    #[test]
    fn distinct_hash_ceiling_does_not_reject_an_existing_key() {
        let admission = AssetDownloadAdmission::new();
        let permits = (0..MAX_DISTINCT_REQUEST_HASHES)
            .map(|index| {
                admission
                    .try_acquire_request(&format!("source-{index}"), &format!("{index:064x}"))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(admission
            .try_acquire_request("new-source", &"f".repeat(64))
            .is_none());
        assert!(admission
            .try_acquire_request("another-source", &format!("{:064x}", 0))
            .is_some());
        assert_eq!(
            admission.inner.request_hash_count.load(Ordering::Acquire),
            MAX_DISTINCT_REQUEST_HASHES
        );
        drop(permits);
        assert_eq!(
            admission.inner.request_hash_count.load(Ordering::Acquire),
            0
        );
        assert!(admission.inner.request_hashes.is_empty());
    }

    #[test]
    fn global_request_ceiling_rejects_before_allocating_new_source_or_hash_keys() {
        let admission = AssetDownloadAdmission::new();
        let permits = (0..MAX_ACTIVE_REQUESTS)
            .map(|index| {
                admission
                    .try_acquire_request(
                        &format!("source-{index}"),
                        &format!("{:064x}", index % MAX_DISTINCT_REQUEST_HASHES),
                    )
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let sources = admission.inner.sources.len();
        let hashes = admission.inner.request_hashes.len();
        assert!(admission
            .try_acquire_request("attacker-new-source", &"f".repeat(64))
            .is_none());
        assert_eq!(admission.inner.sources.len(), sources);
        assert_eq!(admission.inner.request_hashes.len(), hashes);
        drop(permits);
        assert!(admission.inner.sources.is_empty());
        assert!(admission.inner.request_hashes.is_empty());
    }
}
