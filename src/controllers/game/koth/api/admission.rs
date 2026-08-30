//! Small row/byte-weighted admission gates for trusted referee work.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use dashmap::mapref::entry::Entry;
use dashmap::DashMap;

use crate::utils::error::AppError;

pub(super) fn referee_database_error(
    error: sqlx::Error,
    retryable_message: &'static str,
) -> AppError {
    let sqlstate = error
        .as_database_error()
        .and_then(|database_error| database_error.code());
    let retryable = matches!(
        &error,
        sqlx::Error::Io(_)
            | sqlx::Error::Tls(_)
            | sqlx::Error::PoolTimedOut
            | sqlx::Error::PoolClosed
            | sqlx::Error::WorkerCrashed
    ) || matches!(
        sqlstate.as_deref(),
        Some("40001" | "40P01" | "55P03" | "57014" | "57P01" | "57P02" | "57P03" | "53300")
    ) || sqlstate
        .as_deref()
        .is_some_and(|code| code.starts_with("08"));
    if retryable {
        tracing::warn!(
            sqlstate = sqlstate.as_deref().unwrap_or("none"),
            error = %error,
            "KotH referee database work is temporarily unavailable"
        );
        AppError::unavailable(retryable_message)
    } else {
        AppError::internal(error.to_string())
    }
}

#[derive(Clone)]
pub(super) struct WeightedAdmission {
    inner: Arc<Inner>,
}

struct Inner {
    global: AtomicUsize,
    global_limit: usize,
    scopes: DashMap<String, Arc<AtomicUsize>>,
}

pub(super) struct WeightedPermit {
    admission: WeightedAdmission,
    scope: String,
    scope_counter: Arc<AtomicUsize>,
    weight: usize,
}

impl WeightedAdmission {
    pub(super) fn new(global_limit: usize) -> Self {
        assert!(global_limit > 0);
        Self {
            inner: Arc::new(Inner {
                global: AtomicUsize::new(0),
                global_limit,
                scopes: DashMap::new(),
            }),
        }
    }

    pub(super) fn try_acquire(
        &self,
        scope: String,
        weight: usize,
        scope_limit: usize,
    ) -> Option<WeightedPermit> {
        if weight == 0 || weight > scope_limit || weight > self.inner.global_limit {
            return None;
        }
        let scope_counter = self
            .inner
            .scopes
            .entry(scope.clone())
            .or_insert_with(|| Arc::new(AtomicUsize::new(0)))
            .clone();
        if scope_counter
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active
                    .checked_add(weight)
                    .filter(|next| *next <= scope_limit)
            })
            .is_err()
        {
            remove_idle_scope(&self.inner.scopes, &scope, &scope_counter);
            return None;
        }
        if self
            .inner
            .global
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active
                    .checked_add(weight)
                    .filter(|next| *next <= self.inner.global_limit)
            })
            .is_err()
        {
            scope_counter.fetch_sub(weight, Ordering::AcqRel);
            remove_idle_scope(&self.inner.scopes, &scope, &scope_counter);
            return None;
        }
        Some(WeightedPermit {
            admission: self.clone(),
            scope,
            scope_counter,
            weight,
        })
    }
}

impl Drop for WeightedPermit {
    fn drop(&mut self) {
        self.admission
            .inner
            .global
            .fetch_sub(self.weight, Ordering::AcqRel);
        self.scope_counter.fetch_sub(self.weight, Ordering::AcqRel);
        remove_idle_scope(
            &self.admission.inner.scopes,
            &self.scope,
            &self.scope_counter,
        );
    }
}

fn remove_idle_scope(
    scopes: &DashMap<String, Arc<AtomicUsize>>,
    scope: &str,
    counter: &Arc<AtomicUsize>,
) {
    if counter.load(Ordering::Acquire) != 0 {
        return;
    }
    if let Entry::Occupied(entry) = scopes.entry(scope.to_string()) {
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
    fn admission_bounds_weight_per_scope_and_releases_idle_keys() {
        let admission = WeightedAdmission::new(8);
        let first = admission.try_acquire("challenge:7:9".into(), 3, 4).unwrap();
        assert!(admission
            .try_acquire("challenge:7:9".into(), 2, 4)
            .is_none());
        let other = admission
            .try_acquire("challenge:7:10".into(), 4, 4)
            .unwrap();
        assert!(admission
            .try_acquire("challenge:7:11".into(), 2, 4)
            .is_none());
        drop((first, other));
        assert!(admission.inner.scopes.is_empty());
        assert_eq!(admission.inner.global.load(Ordering::Acquire), 0);
    }
}
