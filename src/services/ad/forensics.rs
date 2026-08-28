//! Admission, deadlines, and response bounds for live A&D filesystem forensics.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use sqlx::{Postgres, Transaction};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::services::container::FileChange;
use crate::utils::error::{AppError, AppResult};

pub(crate) const MAX_CHANGE_ENTRIES: usize = 4_096;
pub(crate) const MAX_CHANGE_PATH_BYTES: usize = 4 * 1_024;
pub(crate) const MAX_CHANGE_RESPONSE_BYTES: usize = 448 * 1_024;
pub(crate) const MAX_FILE_PREVIEW_BYTES: usize = 240 * 1_024;
pub(crate) const CHANGE_DEADLINE: Duration = Duration::from_secs(10);
pub(crate) const FILE_DEADLINE: Duration = Duration::from_secs(8);
const ADMISSION_DEADLINE: Duration = Duration::from_secs(2);
const CACHE_TTL: Duration = Duration::from_secs(3);
const MAX_CACHE_IDENTITIES: usize = 32;
const GLOBAL_WEIGHT: usize = 4;
const CHANGES_WEIGHT: u32 = 2;
const FILE_WEIGHT: u32 = 1;
const RETRY_AFTER_SECONDS: u64 = 2;

#[derive(Debug)]
pub(crate) struct BoundedChanges {
    pub(crate) changes: Vec<FileChange>,
    pub(crate) observed: usize,
    pub(crate) truncated: bool,
}

#[derive(Clone)]
struct CachedChanges {
    stored_at: Instant,
    value: Arc<BoundedChanges>,
}

#[derive(Clone, Copy)]
pub(crate) enum ForensicsWork {
    Changes,
    File,
}

impl ForensicsWork {
    fn weight(self) -> u32 {
        match self {
            Self::Changes => CHANGES_WEIGHT,
            Self::File => FILE_WEIGHT,
        }
    }
}

pub(crate) struct ForensicsPermit<'a> {
    _local: OwnedSemaphorePermit,
    // Transaction-scoped advisory locks make the operation limit effective
    // across web replicas. Cancellation drops the transaction and releases all
    // locks instead of leaving a session-level lease in the pool.
    _distributed: Transaction<'a, Postgres>,
}

struct ForensicsAdmission {
    local: Arc<Semaphore>,
    cache: Mutex<HashMap<String, CachedChanges>>,
}

impl ForensicsAdmission {
    fn new() -> Self {
        Self {
            local: Arc::new(Semaphore::new(GLOBAL_WEIGHT)),
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn cached(&self, container_id: &str) -> Option<Arc<BoundedChanges>> {
        let now = Instant::now();
        let mut cache = self.cache.lock().unwrap_or_else(|error| error.into_inner());
        cache.retain(|_, entry| now.duration_since(entry.stored_at) <= CACHE_TTL);
        cache.get(container_id).map(|entry| entry.value.clone())
    }

    fn store(&self, container_id: &str, value: Arc<BoundedChanges>) {
        let now = Instant::now();
        let mut cache = self.cache.lock().unwrap_or_else(|error| error.into_inner());
        cache.retain(|_, entry| now.duration_since(entry.stored_at) <= CACHE_TTL);
        if cache.len() >= MAX_CACHE_IDENTITIES && !cache.contains_key(container_id) {
            if let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, entry)| entry.stored_at)
                .map(|(key, _)| key.clone())
            {
                cache.remove(&oldest);
            }
        }
        cache.insert(
            container_id.to_owned(),
            CachedChanges {
                stored_at: now,
                value,
            },
        );
    }

    async fn acquire<'a>(
        &'a self,
        pool: &'a sqlx::PgPool,
        container_id: &str,
        work: ForensicsWork,
    ) -> AppResult<ForensicsPermit<'a>> {
        let weight = work.weight();
        let local = Arc::clone(&self.local)
            .try_acquire_many_owned(weight)
            .map_err(|_| retryable("Live filesystem inspection is busy; retry shortly"))?;

        let mut tx = tokio::time::timeout(Duration::from_secs(1), pool.begin())
            .await
            .map_err(|_| retryable("Live filesystem inspection admission timed out"))?
            .map_err(|error| AppError::internal(error.to_string()))?;
        let container_lock: bool = sqlx::query_scalar(
            "SELECT pg_try_advisory_xact_lock(hashtextextended('rsctf:ad-forensics:container:' || $1, 0))",
        )
        .bind(container_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if !container_lock {
            return Err(retryable(
                "This container already has a live filesystem inspection in progress",
            ));
        }

        let mut acquired = 0u32;
        for slot in 0..GLOBAL_WEIGHT {
            let slot_key = format!("rsctf:ad-forensics:slot:{slot}");
            let locked: bool =
                sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1, 0))")
                    .bind(slot_key)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|error| AppError::internal(error.to_string()))?;
            acquired += u32::from(locked);
            if acquired == weight {
                return Ok(ForensicsPermit {
                    _local: local,
                    _distributed: tx,
                });
            }
        }
        Err(retryable(
            "Live filesystem inspection capacity is busy; retry shortly",
        ))
    }
}

static ADMISSION: LazyLock<ForensicsAdmission> = LazyLock::new(ForensicsAdmission::new);

pub(crate) fn cached_changes(container_id: &str) -> Option<Arc<BoundedChanges>> {
    ADMISSION.cached(container_id)
}

pub(crate) fn cache_changes(container_id: &str, changes: Arc<BoundedChanges>) {
    ADMISSION.store(container_id, changes);
}

pub(crate) async fn acquire<'a>(
    pool: &'a sqlx::PgPool,
    container_id: &str,
    work: ForensicsWork,
) -> AppResult<ForensicsPermit<'a>> {
    tokio::time::timeout(
        ADMISSION_DEADLINE,
        ADMISSION.acquire(pool, container_id, work),
    )
    .await
    .map_err(|_| retryable("Live filesystem inspection admission timed out"))?
}

fn retryable(message: &str) -> AppError {
    AppError::retryable_unavailable(message, RETRY_AFTER_SECONDS)
}

pub(crate) fn timeout_error(operation: &str) -> AppError {
    retryable(&format!("Live filesystem {operation} timed out"))
}

pub(crate) fn validate_path(path: &str) -> AppResult<()> {
    if path.is_empty()
        || path.len() > MAX_CHANGE_PATH_BYTES
        || !path.starts_with('/')
        || path
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
        || path
            .split('/')
            .any(|component| component == "." || component == "..")
    {
        return Err(AppError::bad_request(
            "File path must be an absolute, normalized container path of at most 4096 bytes",
        ));
    }
    Ok(())
}

pub(crate) fn bound_changes(changes: Vec<FileChange>) -> BoundedChanges {
    let observed = changes.len();
    let mut stored = Vec::with_capacity(observed.min(MAX_CHANGE_ENTRIES));
    let mut encoded_bytes = 2usize;
    let mut truncated = false;

    for change in changes {
        if validate_path(&change.path).is_err() || stored.len() >= MAX_CHANGE_ENTRIES {
            truncated = true;
            continue;
        }
        let Ok(encoded) = serde_json::to_vec(&change) else {
            truncated = true;
            continue;
        };
        let next = encoded_bytes
            .saturating_add(usize::from(!stored.is_empty()))
            .saturating_add(encoded.len());
        if next > MAX_CHANGE_RESPONSE_BYTES {
            truncated = true;
            continue;
        }
        encoded_bytes = next;
        stored.push(change);
    }

    BoundedChanges {
        truncated: truncated || stored.len() < observed,
        observed,
        changes: stored,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(path: impl Into<String>) -> FileChange {
        FileChange {
            path: path.into(),
            kind: "Modified".into(),
        }
    }

    #[test]
    fn huge_change_sets_and_unsafe_paths_are_bounded() {
        let mut changes = vec![change("relative"), change("/ok/../secret")];
        changes
            .extend((0..10_000).map(|index| change(format!("/safe/{index}/{}", "x".repeat(96)))));
        let bounded = bound_changes(changes);

        assert!(bounded.truncated);
        assert_eq!(bounded.observed, 10_002);
        assert!(bounded.changes.len() <= MAX_CHANGE_ENTRIES);
        assert!(bounded
            .changes
            .iter()
            .all(|change| validate_path(&change.path).is_ok()));
        assert!(serde_json::to_vec(&bounded.changes).unwrap().len() <= MAX_CHANGE_RESPONSE_BYTES);
    }

    #[test]
    fn file_paths_require_an_absolute_normalized_bounded_value() {
        assert!(validate_path("/srv/app/main.py").is_ok());
        for invalid in ["", "relative", "/a/../flag", "/a/./b", "/tmp/\nsecret"] {
            assert!(validate_path(invalid).is_err(), "accepted {invalid:?}");
        }
        assert!(validate_path(&format!("/{}", "x".repeat(MAX_CHANGE_PATH_BYTES))).is_err());
    }

    #[test]
    fn cache_is_keyed_by_exact_backend_identity_and_bounded() {
        let admission = ForensicsAdmission::new();
        let value = Arc::new(bound_changes(vec![change("/one")]));
        admission.store("container-generation-one", value);
        assert!(admission.cached("container-generation-one").is_some());
        assert!(admission.cached("container-generation-two").is_none());

        for index in 0..(MAX_CACHE_IDENTITIES + 5) {
            admission.store(
                &format!("container-{index}"),
                Arc::new(bound_changes(vec![])),
            );
        }
        assert!(
            admission
                .cache
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .len()
                <= MAX_CACHE_IDENTITIES
        );
    }

    #[test]
    fn weighted_local_admission_rejects_excess_work_immediately() {
        let admission = ForensicsAdmission::new();
        let first = Arc::clone(&admission.local)
            .try_acquire_many_owned(CHANGES_WEIGHT)
            .unwrap();
        let second = Arc::clone(&admission.local)
            .try_acquire_many_owned(CHANGES_WEIGHT)
            .unwrap();
        assert!(Arc::clone(&admission.local)
            .try_acquire_many_owned(FILE_WEIGHT)
            .is_err());
        drop(first);
        assert!(Arc::clone(&admission.local)
            .try_acquire_many_owned(FILE_WEIGHT)
            .is_ok());
        drop(second);
    }

    #[test]
    fn admission_and_runtime_deadlines_are_absolute_and_small() {
        assert!(ADMISSION_DEADLINE <= Duration::from_secs(2));
        assert!(FILE_DEADLINE <= Duration::from_secs(8));
        assert!(CHANGE_DEADLINE <= Duration::from_secs(10));
    }

    #[tokio::test]
    #[ignore = "requires RSCTF_TEST_DATABASE_URL"]
    async fn postgres_advisory_locks_bound_distinct_replicas_and_one_container() {
        let url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL is required for this ignored test");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(8)
            .connect(&url)
            .await
            .unwrap();
        let replica_one = ForensicsAdmission::new();
        let replica_two = ForensicsAdmission::new();

        let first = replica_one
            .acquire(&pool, "same-container", ForensicsWork::File)
            .await
            .unwrap();
        assert!(matches!(
            replica_two
                .acquire(&pool, "same-container", ForensicsWork::File)
                .await,
            Err(AppError::RetryableUnavailable { .. })
        ));
        drop(first);
        assert!(replica_two
            .acquire(&pool, "same-container", ForensicsWork::File)
            .await
            .is_ok());

        let changes_one = replica_one
            .acquire(&pool, "changes-one", ForensicsWork::Changes)
            .await
            .unwrap();
        let changes_two = replica_two
            .acquire(&pool, "changes-two", ForensicsWork::Changes)
            .await
            .unwrap();
        let third_replica = ForensicsAdmission::new();
        assert!(matches!(
            third_replica
                .acquire(&pool, "excess-file", ForensicsWork::File)
                .await,
            Err(AppError::RetryableUnavailable { .. })
        ));
        drop((changes_one, changes_two));
    }
}
