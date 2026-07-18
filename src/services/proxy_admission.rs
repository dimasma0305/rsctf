use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use uuid::Uuid;

const MAX_PER_USER: usize = 4;
const MAX_PER_PARTICIPATION: usize = 16;
const MAX_PER_WORKLOAD: usize = 64;

#[derive(Clone)]
pub struct ProxyAdmission {
    inner: Arc<Inner>,
}

struct Inner {
    users: DashMap<Uuid, Arc<AtomicUsize>>,
    scopes: DashMap<AdmissionScope, Arc<AtomicUsize>>,
    workloads: DashMap<Uuid, Arc<AtomicUsize>>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum AdmissionScope {
    Participation(i32),
    Exercise(i32),
}

pub struct ProxyPermit {
    admission: ProxyAdmission,
    user: (Uuid, Arc<AtomicUsize>),
    scope: (AdmissionScope, Arc<AtomicUsize>),
    workload: (Uuid, Arc<AtomicUsize>),
}

impl ProxyAdmission {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                users: DashMap::new(),
                scopes: DashMap::new(),
                workloads: DashMap::new(),
            }),
        }
    }

    pub fn try_acquire(
        &self,
        user_id: Uuid,
        participation_id: i32,
        workload_id: Uuid,
    ) -> Option<ProxyPermit> {
        self.try_acquire_scope(
            user_id,
            AdmissionScope::Participation(participation_id),
            workload_id,
        )
    }

    pub fn try_acquire_exercise(
        &self,
        user_id: Uuid,
        exercise_instance_id: i32,
        workload_id: Uuid,
    ) -> Option<ProxyPermit> {
        self.try_acquire_scope(
            user_id,
            AdmissionScope::Exercise(exercise_instance_id),
            workload_id,
        )
    }

    fn try_acquire_scope(
        &self,
        user_id: Uuid,
        scope: AdmissionScope,
        workload_id: Uuid,
    ) -> Option<ProxyPermit> {
        let user = increment(&self.inner.users, user_id, MAX_PER_USER)?;
        let scope_counter = match increment(&self.inner.scopes, scope, MAX_PER_PARTICIPATION) {
            Some(counter) => counter,
            None => {
                release(&self.inner.users, user_id, &user);
                return None;
            }
        };
        let workload = match increment(&self.inner.workloads, workload_id, MAX_PER_WORKLOAD) {
            Some(counter) => counter,
            None => {
                release(&self.inner.scopes, scope, &scope_counter);
                release(&self.inner.users, user_id, &user);
                return None;
            }
        };
        Some(ProxyPermit {
            admission: self.clone(),
            user: (user_id, user),
            scope: (scope, scope_counter),
            workload: (workload_id, workload),
        })
    }
}

impl Default for ProxyAdmission {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ProxyPermit {
    fn drop(&mut self) {
        release(
            &self.admission.inner.workloads,
            self.workload.0,
            &self.workload.1,
        );
        release(&self.admission.inner.scopes, self.scope.0, &self.scope.1);
        release(&self.admission.inner.users, self.user.0, &self.user.1);
    }
}

fn increment<K>(
    map: &DashMap<K, Arc<AtomicUsize>>,
    key: K,
    limit: usize,
) -> Option<Arc<AtomicUsize>>
where
    K: Eq + std::hash::Hash + Copy,
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
    K: Eq + std::hash::Hash + Copy,
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
    fn user_limit_releases_with_the_session() {
        let admission = ProxyAdmission::new();
        let user = Uuid::new_v4();
        let workload = Uuid::new_v4();
        let permits = (0..MAX_PER_USER)
            .map(|_| admission.try_acquire(user, 1, workload).unwrap())
            .collect::<Vec<_>>();
        assert!(admission.try_acquire(user, 1, workload).is_none());
        drop(permits);
        assert!(admission.try_acquire(user, 1, workload).is_some());
    }

    #[test]
    fn participation_limit_spans_users_and_workloads() {
        let admission = ProxyAdmission::new();
        let permits = (0..MAX_PER_PARTICIPATION)
            .map(|_| {
                admission
                    .try_acquire(Uuid::new_v4(), 7, Uuid::new_v4())
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(admission
            .try_acquire(Uuid::new_v4(), 7, Uuid::new_v4())
            .is_none());
        drop(permits);
    }

    #[test]
    fn exercise_and_participation_scopes_with_the_same_id_are_independent() {
        let admission = ProxyAdmission::new();
        let participation_permits = (0..MAX_PER_PARTICIPATION)
            .map(|_| {
                admission
                    .try_acquire(Uuid::new_v4(), 7, Uuid::new_v4())
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(admission
            .try_acquire(Uuid::new_v4(), 7, Uuid::new_v4())
            .is_none());
        assert!(admission
            .try_acquire_exercise(Uuid::new_v4(), 7, Uuid::new_v4())
            .is_some());
        drop(participation_permits);
    }
}
