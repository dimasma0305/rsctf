//! One live authorization poll owner per exact proxy lease generation.

use std::future::Future;
use std::hash::Hash;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dashmap::mapref::entry::Entry;
use dashmap::DashMap;

struct Generation {
    valid: tokio::sync::watch::Sender<bool>,
    subscribers: AtomicUsize,
    idle: tokio::sync::Notify,
}

pub(in crate::controllers::proxy) struct LeaseGenerationCache<K> {
    entries: DashMap<K, Arc<Generation>>,
}

pub(in crate::controllers::proxy) struct LeaseSubscription<K>
where
    K: Clone + Eq + Hash,
{
    cache: Arc<LeaseGenerationCache<K>>,
    key: K,
    generation: Arc<Generation>,
    receiver: tokio::sync::watch::Receiver<bool>,
}

pub(in crate::controllers::proxy) struct LeaseGenerationOwner<K>
where
    K: Clone + Eq + Hash,
{
    cache: Arc<LeaseGenerationCache<K>>,
    key: K,
    generation: Arc<Generation>,
}

impl<K> LeaseGenerationCache<K>
where
    K: Clone + Eq + Hash,
{
    pub(in crate::controllers::proxy) fn new() -> Arc<Self> {
        Arc::new(Self {
            entries: DashMap::new(),
        })
    }

    /// Subscribe to one exact mutable authorization snapshot. `owner` is true
    /// only for the caller that must start the shared authoritative poll loop.
    pub(in crate::controllers::proxy) fn subscribe(
        self: &Arc<Self>,
        key: K,
    ) -> (LeaseSubscription<K>, Option<LeaseGenerationOwner<K>>) {
        let (generation, owner) = match self.entries.entry(key.clone()) {
            Entry::Occupied(entry) => {
                let generation = Arc::clone(entry.get());
                generation.subscribers.fetch_add(1, Ordering::AcqRel);
                (generation, false)
            }
            Entry::Vacant(entry) => {
                let (valid, _) = tokio::sync::watch::channel(true);
                let generation = Arc::new(Generation {
                    valid,
                    subscribers: AtomicUsize::new(1),
                    idle: tokio::sync::Notify::new(),
                });
                entry.insert(Arc::clone(&generation));
                (generation, true)
            }
        };
        let receiver = generation.valid.subscribe();
        let owner = owner.then(|| LeaseGenerationOwner {
            cache: Arc::clone(self),
            key: key.clone(),
            generation: Arc::clone(&generation),
        });
        (
            LeaseSubscription {
                cache: Arc::clone(self),
                key,
                generation,
                receiver,
            },
            owner,
        )
    }
}

impl<K> LeaseGenerationOwner<K>
where
    K: Clone + Eq + Hash,
{
    pub(in crate::controllers::proxy) async fn drive<F, Fut>(self, period: Duration, mut check: F)
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = bool>,
    {
        loop {
            tokio::time::sleep(period).await;
            if self.generation.subscribers.load(Ordering::Acquire) == 0 {
                if self
                    .cache
                    .entries
                    .remove_if(&self.key, |_, current| {
                        Arc::ptr_eq(current, &self.generation)
                            && self.generation.subscribers.load(Ordering::Acquire) == 0
                    })
                    .is_some()
                {
                    return;
                }
                continue;
            }
            if !check().await {
                self.generation.valid.send_replace(false);
                loop {
                    let idle = self.generation.idle.notified();
                    if self.generation.subscribers.load(Ordering::Acquire) == 0 {
                        break;
                    }
                    idle.await;
                }
                break;
            }
        }
        self.cache.entries.remove_if(&self.key, |_, current| {
            Arc::ptr_eq(current, &self.generation)
                && self.generation.subscribers.load(Ordering::Acquire) == 0
        });
    }
}

impl<K> LeaseSubscription<K>
where
    K: Clone + Eq + Hash,
{
    pub(in crate::controllers::proxy) async fn invalidated(&mut self) {
        if !*self.receiver.borrow() {
            return;
        }
        let _ = self.receiver.wait_for(|valid| !*valid).await;
    }
}

impl<K> Drop for LeaseSubscription<K>
where
    K: Clone + Eq + Hash,
{
    fn drop(&mut self) {
        if self.generation.subscribers.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.generation.idle.notify_one();
            self.cache.entries.remove_if(&self.key, |_, current| {
                Arc::ptr_eq(current, &self.generation)
                    && self.generation.subscribers.load(Ordering::Acquire) == 0
                    && !*self.generation.valid.borrow()
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[tokio::test]
    async fn one_generation_owner_invalidates_all_subscribers_once() {
        let cache = LeaseGenerationCache::new();
        let mut subscriptions = Vec::new();
        let mut owner = None;
        for _ in 0..16 {
            let (subscription, candidate) = cache.subscribe(7);
            subscriptions.push(subscription);
            if candidate.is_some() {
                assert!(owner.is_none());
                owner = candidate;
            }
        }
        let owner = owner.expect("one shared generation owner");

        let calls = Arc::new(AtomicUsize::new(0));
        let drive_calls = Arc::clone(&calls);
        let owner = tokio::spawn(owner.drive(Duration::from_millis(1), move || {
            let calls = Arc::clone(&drive_calls);
            async move {
                calls.fetch_add(1, Ordering::Relaxed);
                false
            }
        }));
        for subscription in &mut subscriptions {
            tokio::time::timeout(Duration::from_secs(1), subscription.invalidated())
                .await
                .unwrap();
        }
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        drop(subscriptions);
        owner.await.unwrap();
        assert!(cache.entries.is_empty());
    }
}
