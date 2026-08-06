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

#[derive(Clone)]
pub struct AssetDownloadAdmission {
    inner: Arc<Inner>,
}

struct Inner {
    global: AtomicUsize,
    users: DashMap<Uuid, Arc<AtomicUsize>>,
    hashes: DashMap<String, Arc<AtomicUsize>>,
}

pub struct AssetDownloadPermit {
    admission: AssetDownloadAdmission,
    user: Option<(Uuid, Arc<AtomicUsize>)>,
    hash: (String, Arc<AtomicUsize>),
}

impl AssetDownloadAdmission {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                global: AtomicUsize::new(0),
                users: DashMap::new(),
                hashes: DashMap::new(),
            }),
        }
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
}
