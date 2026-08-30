//! Bounded admission shared by large archive and retained-snapshot responses.
//!
//! The local semaphores are the authoritative per-replica memory/task bound.
//! Cache-backed weighted leases add a deployment-wide ceiling when replicas
//! share Redis (which replica mode requires). A lease is response-owned and is
//! released on completion, error, or disconnect; its TTL is crash recovery.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures::Stream;
use futures::StreamExt;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

use crate::services::cache::Cache;

const MIB: usize = 1024 * 1024;
const LOCAL_TASKS: usize = 2;
const LOCAL_MEMORY_MIB: usize = 256;
const DISTRIBUTED_UNIT_MIB: usize = 32;
const DISTRIBUTED_UNITS: usize = LOCAL_MEMORY_MIB / DISTRIBUTED_UNIT_MIB;
const LEASE_TTL: Duration = Duration::from_secs(60 * 60);
const LEASE_PREFIX: &str = "bulk-export:v1";
const RESPONSE_CHUNK_BYTES: usize = 64 * 1024;
pub(crate) const RETRY_AFTER_SECONDS: u64 = 3;

#[derive(Clone)]
pub(crate) struct BulkExportAdmission {
    tasks: Arc<Semaphore>,
    memory: Arc<Semaphore>,
}

pub(crate) struct BulkExportPermit {
    _task: OwnedSemaphorePermit,
    _memory: OwnedSemaphorePermit,
    lease: Option<DistributedLease>,
    deadline: tokio::time::Instant,
}

struct DistributedLease {
    cache: Arc<dyn Cache>,
    token: Vec<u8>,
    keys: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BulkExportAdmissionError {
    Busy,
}

impl BulkExportAdmission {
    pub(crate) fn new() -> Self {
        Self::with_limits(LOCAL_TASKS, LOCAL_MEMORY_MIB)
    }

    fn with_limits(tasks: usize, memory_mib: usize) -> Self {
        Self {
            tasks: Arc::new(Semaphore::new(tasks)),
            memory: Arc::new(Semaphore::new(memory_mib)),
        }
    }

    /// Reserve bounded work before any export query or blob read.
    pub(crate) async fn try_acquire(
        &self,
        cache: Arc<dyn Cache>,
        maximum_retained_bytes: usize,
    ) -> Result<BulkExportPermit, BulkExportAdmissionError> {
        let task = Arc::clone(&self.tasks)
            .try_acquire_owned()
            .map_err(|_| BulkExportAdmissionError::Busy)?;
        let memory_units = maximum_retained_bytes.max(1).div_ceil(MIB);
        let memory_units =
            u32::try_from(memory_units).map_err(|_| BulkExportAdmissionError::Busy)?;
        let memory = Arc::clone(&self.memory)
            .try_acquire_many_owned(memory_units)
            .map_err(|_| BulkExportAdmissionError::Busy)?;
        let lease_units = maximum_retained_bytes
            .max(1)
            .div_ceil(DISTRIBUTED_UNIT_MIB * MIB)
            .min(DISTRIBUTED_UNITS);
        let token = Uuid::new_v4().as_bytes().to_vec();
        let mut keys = Vec::with_capacity(lease_units);
        for slot in 0..DISTRIBUTED_UNITS {
            let key = format!("{LEASE_PREFIX}:{slot}");
            if cache.set_if_absent(&key, &token, Some(LEASE_TTL)).await {
                keys.push(key);
                if keys.len() == lease_units {
                    break;
                }
            }
        }
        if keys.len() != lease_units {
            release_keys(cache.as_ref(), &keys, &token).await;
            return Err(BulkExportAdmissionError::Busy);
        }

        Ok(BulkExportPermit {
            _task: task,
            _memory: memory,
            lease: Some(DistributedLease { cache, token, keys }),
            deadline: tokio::time::Instant::now() + LEASE_TTL,
        })
    }
}

impl Default for BulkExportAdmission {
    fn default() -> Self {
        Self::new()
    }
}

async fn release_keys(cache: &dyn Cache, keys: &[String], token: &[u8]) {
    for key in keys {
        cache.compare_and_remove(key, token).await;
    }
}

impl Drop for DistributedLease {
    fn drop(&mut self) {
        let cache = Arc::clone(&self.cache);
        let token = self.token.clone();
        let keys = std::mem::take(&mut self.keys);
        if keys.is_empty() {
            return;
        }
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                release_keys(cache.as_ref(), &keys, &token).await;
            });
        }
    }
}

impl Drop for BulkExportPermit {
    fn drop(&mut self) {
        drop(self.lease.take());
    }
}

pub(crate) fn overload_response() -> Response {
    let mut response = crate::utils::shared::MessageResponse::new(
        "Bulk download capacity is busy; retry shortly",
        StatusCode::SERVICE_UNAVAILABLE.as_u16(),
    )
    .into_response();
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from(RETRY_AFTER_SECONDS));
    response
}

/// Keep admission alive for exactly as long as Axum owns the storage stream.
pub(crate) fn permitted_stream_body<S>(stream: S, permit: Arc<BulkExportPermit>) -> Body
where
    S: Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static,
{
    let deadline = permit.deadline;
    let held = futures::stream::unfold(
        (Box::pin(stream), permit, deadline, false),
        |(mut stream, permit, deadline, timed_out)| async move {
            if timed_out {
                return None;
            }
            match tokio::time::timeout_at(deadline, stream.next()).await {
                Ok(Some(item)) => Some((item, (stream, permit, deadline, false))),
                Ok(None) => None,
                Err(_) => Some((
                    Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "bulk download exceeded its maximum duration",
                    )),
                    (stream, permit, deadline, true),
                )),
            }
        },
    );
    Body::from_stream(held)
}

/// Chunk an already-bounded archive while retaining admission through response
/// ownership. This prevents the complete `Vec` from escaping the advertised
/// two-export bound when a client consumes slowly.
pub(crate) fn permitted_bytes_body(bytes: Vec<u8>, permit: Arc<BulkExportPermit>) -> Body {
    let bytes = Bytes::from(bytes);
    let deadline = permit.deadline;
    let held = futures::stream::unfold(
        (bytes, 0usize, permit, deadline),
        |(bytes, offset, permit, deadline)| async move {
            if offset >= bytes.len() || tokio::time::Instant::now() >= deadline {
                return None;
            }
            let end = offset.saturating_add(RESPONSE_CHUNK_BYTES).min(bytes.len());
            let chunk = bytes.slice(offset..end);
            Some((
                Ok::<_, std::io::Error>(chunk),
                (bytes, end, permit, deadline),
            ))
        },
    );
    Body::from_stream(held)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::cache::InMemoryCache;

    #[tokio::test]
    async fn local_and_distributed_weight_are_released_with_the_permit() {
        let admission = BulkExportAdmission::with_limits(2, 256);
        let cache: Arc<dyn Cache> = Arc::new(InMemoryCache::new());
        let first = admission
            .try_acquire(Arc::clone(&cache), 128 * MIB)
            .await
            .unwrap();
        let second = admission
            .try_acquire(Arc::clone(&cache), 128 * MIB)
            .await
            .unwrap();
        assert_eq!(
            admission.try_acquire(Arc::clone(&cache), MIB).await.err(),
            Some(BulkExportAdmissionError::Busy)
        );
        drop(first);
        drop(second);
        tokio::task::yield_now().await;
        assert!(admission.try_acquire(cache, 128 * MIB).await.is_ok());
    }

    #[tokio::test]
    async fn failed_weighted_claim_rolls_back_partial_distributed_slots() {
        let cache: Arc<dyn Cache> = Arc::new(InMemoryCache::new());
        let competing = BulkExportAdmission::with_limits(2, 256);
        let held = competing
            .try_acquire(Arc::clone(&cache), 224 * MIB)
            .await
            .unwrap();
        let other_replica = BulkExportAdmission::with_limits(2, 256);
        assert_eq!(
            other_replica
                .try_acquire(Arc::clone(&cache), 64 * MIB)
                .await
                .err(),
            Some(BulkExportAdmissionError::Busy)
        );
        drop(held);
        tokio::task::yield_now().await;
        assert!(other_replica.try_acquire(cache, 256 * MIB).await.is_ok());
    }

    #[tokio::test]
    async fn response_body_owns_capacity_until_it_is_dropped() {
        let admission = BulkExportAdmission::with_limits(1, 1);
        let cache: Arc<dyn Cache> = Arc::new(InMemoryCache::new());
        let permit = admission
            .try_acquire(Arc::clone(&cache), MIB)
            .await
            .unwrap();
        let body = permitted_bytes_body(vec![0; 128], Arc::new(permit));
        assert_eq!(
            admission.try_acquire(Arc::clone(&cache), 1).await.err(),
            Some(BulkExportAdmissionError::Busy)
        );
        drop(body);
        tokio::task::yield_now().await;
        assert!(admission.try_acquire(cache, 1).await.is_ok());
    }

    #[test]
    fn overload_is_typed_and_retryable() {
        let response = overload_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response.headers()[header::RETRY_AFTER],
            HeaderValue::from_static("3")
        );
    }
}
