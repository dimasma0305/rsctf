//! Bounded admission and deadlines for the worker's short-lived Docker calls.
//!
//! The trusted worker reconciles many workloads against one daemon. Reads
//! (`inspect`, `list`, bounded downloads, ping) and lifecycle calls (`create`,
//! `start`, `stop`, `remove`, uploads, network changes, pulls) get separate
//! concurrency bounds and per-call deadlines so a burst of status probes cannot
//! starve a workload start, and a stalled daemon cannot pin a reconciliation
//! task forever. Every wrapped future owns its response stream, so a timeout
//! drops the stream and cancels the daemon transfer. Interactive exec and
//! other long-lived streams never pass through this wrapper.

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rsctf_worker_protocol::CommandErrorCode;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::runtime::RuntimeError;

/// Warn at most once per class per window when the daemon is saturated.
const PRESSURE_LOG_WINDOW: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockerOperationClass {
    Read,
    Lifecycle,
}

impl DockerOperationClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Lifecycle => "lifecycle",
        }
    }
}

/// Validated worker defaults. Windows images and Hyper-V starts are slower
/// than Linux, so lifecycle and pull budgets are wider than the server's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DockerAdmissionLimits {
    pub read_concurrency: usize,
    pub read_deadline: Duration,
    pub read_queue_wait: Duration,
    pub lifecycle_concurrency: usize,
    pub lifecycle_deadline: Duration,
    pub lifecycle_queue_wait: Duration,
    pub pull_deadline: Duration,
}

impl Default for DockerAdmissionLimits {
    fn default() -> Self {
        Self {
            read_concurrency: 16,
            read_deadline: Duration::from_secs(10),
            read_queue_wait: Duration::from_secs(5),
            lifecycle_concurrency: 4,
            lifecycle_deadline: Duration::from_secs(60),
            lifecycle_queue_wait: Duration::from_secs(30),
            pull_deadline: Duration::from_secs(600),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DockerAdmissionError {
    Overloaded {
        class: DockerOperationClass,
        operation: &'static str,
        waited: Duration,
    },
    Timeout {
        class: DockerOperationClass,
        operation: &'static str,
        deadline: Duration,
    },
    Closed,
}

impl std::fmt::Display for DockerAdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Overloaded {
                class,
                operation,
                waited,
            } => write!(
                f,
                "Docker {} admission is saturated: {operation} waited {} ms for a slot",
                class.as_str(),
                waited.as_millis()
            ),
            Self::Timeout {
                class,
                operation,
                deadline,
            } => write!(
                f,
                "Docker {} call {operation} exceeded its {} s deadline",
                class.as_str(),
                deadline.as_secs()
            ),
            Self::Closed => write!(f, "Docker admission is closed"),
        }
    }
}

impl std::error::Error for DockerAdmissionError {}

/// Overload and deadline failures are retryable from the server's point of
/// view: the assignment stays pending and the next reconciliation retries.
impl From<DockerAdmissionError> for RuntimeError {
    fn from(error: DockerAdmissionError) -> Self {
        let code = match error {
            DockerAdmissionError::Overloaded { .. } | DockerAdmissionError::Closed => {
                CommandErrorCode::RuntimeUnavailable
            }
            DockerAdmissionError::Timeout { .. } => CommandErrorCode::Timeout,
        };
        RuntimeError::new(code, error.to_string())
    }
}

struct ClassCounters {
    in_flight: AtomicU64,
    queued: AtomicU64,
    admitted: AtomicU64,
    completed: AtomicU64,
    overloaded: AtomicU64,
    timeouts: AtomicU64,
    cancellations: AtomicU64,
    queue_wait_total_ms: AtomicU64,
    queue_wait_max_ms: AtomicU64,
    daemon_latency_total_ms: AtomicU64,
    daemon_latency_max_ms: AtomicU64,
    last_warning_ms: AtomicU64,
}

impl ClassCounters {
    const fn new() -> Self {
        Self {
            in_flight: AtomicU64::new(0),
            queued: AtomicU64::new(0),
            admitted: AtomicU64::new(0),
            completed: AtomicU64::new(0),
            overloaded: AtomicU64::new(0),
            timeouts: AtomicU64::new(0),
            cancellations: AtomicU64::new(0),
            queue_wait_total_ms: AtomicU64::new(0),
            queue_wait_max_ms: AtomicU64::new(0),
            daemon_latency_total_ms: AtomicU64::new(0),
            daemon_latency_max_ms: AtomicU64::new(0),
            last_warning_ms: AtomicU64::new(u64::MAX),
        }
    }
}

fn saturating_add(counter: &AtomicU64, amount: u64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_add(amount))
    });
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Point-in-time counters for one class, used by throttled pressure logs and
/// tests. The worker wire protocol is unchanged; the server sees pressure only
/// through retryable command errors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DockerClassMetrics {
    pub concurrency_limit: usize,
    pub deadline_ms: u64,
    pub in_flight: u64,
    pub queued: u64,
    pub admitted: u64,
    pub completed: u64,
    pub overloaded: u64,
    pub timeouts: u64,
    pub cancellations: u64,
    pub queue_wait_total_ms: u64,
    pub queue_wait_max_ms: u64,
    pub daemon_latency_total_ms: u64,
    pub daemon_latency_max_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DockerAdmissionMetrics {
    pub read: DockerClassMetrics,
    pub lifecycle: DockerClassMetrics,
}

struct Lane {
    class: DockerOperationClass,
    slots: Arc<Semaphore>,
    concurrency: usize,
    deadline: Duration,
    queue_wait: Duration,
    counters: ClassCounters,
}

impl Lane {
    fn new(
        class: DockerOperationClass,
        concurrency: usize,
        deadline: Duration,
        queue_wait: Duration,
    ) -> Self {
        Self {
            class,
            slots: Arc::new(Semaphore::new(concurrency)),
            concurrency,
            deadline,
            queue_wait,
            counters: ClassCounters::new(),
        }
    }

    fn snapshot(&self) -> DockerClassMetrics {
        let c = &self.counters;
        DockerClassMetrics {
            concurrency_limit: self.concurrency,
            deadline_ms: millis(self.deadline),
            in_flight: c.in_flight.load(Ordering::Relaxed),
            queued: c.queued.load(Ordering::Relaxed),
            admitted: c.admitted.load(Ordering::Relaxed),
            completed: c.completed.load(Ordering::Relaxed),
            overloaded: c.overloaded.load(Ordering::Relaxed),
            timeouts: c.timeouts.load(Ordering::Relaxed),
            cancellations: c.cancellations.load(Ordering::Relaxed),
            queue_wait_total_ms: c.queue_wait_total_ms.load(Ordering::Relaxed),
            queue_wait_max_ms: c.queue_wait_max_ms.load(Ordering::Relaxed),
            daemon_latency_total_ms: c.daemon_latency_total_ms.load(Ordering::Relaxed),
            daemon_latency_max_ms: c.daemon_latency_max_ms.load(Ordering::Relaxed),
        }
    }

    fn warn_throttled(&self, since_start: Duration, error: &DockerAdmissionError) {
        let now = millis(since_start);
        let last = self.counters.last_warning_ms.load(Ordering::Relaxed);
        if last != u64::MAX && now.saturating_sub(last) < millis(PRESSURE_LOG_WINDOW) {
            return;
        }
        if self
            .counters
            .last_warning_ms
            .compare_exchange(last, now, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        let snapshot = self.snapshot();
        tracing::warn!(
            class = self.class.as_str(),
            %error,
            in_flight = snapshot.in_flight,
            queued = snapshot.queued,
            overloaded = snapshot.overloaded,
            timeouts = snapshot.timeouts,
            cancellations = snapshot.cancellations,
            queue_wait_max_ms = snapshot.queue_wait_max_ms,
            daemon_latency_max_ms = snapshot.daemon_latency_max_ms,
            "Docker daemon admission pressure"
        );
    }
}

/// Releases the slot and the in-flight gauge however the call ends. A call
/// that neither completed nor timed out was dropped by its caller.
struct InFlight<'a> {
    lane: &'a Lane,
    _permit: OwnedSemaphorePermit,
    settled: bool,
}

impl<'a> InFlight<'a> {
    fn new(lane: &'a Lane, permit: OwnedSemaphorePermit) -> Self {
        saturating_add(&lane.counters.admitted, 1);
        lane.counters.in_flight.fetch_add(1, Ordering::Relaxed);
        Self {
            lane,
            _permit: permit,
            settled: false,
        }
    }

    fn completed(&mut self, latency: Duration) {
        self.settled = true;
        saturating_add(&self.lane.counters.completed, 1);
        saturating_add(&self.lane.counters.daemon_latency_total_ms, millis(latency));
        self.lane
            .counters
            .daemon_latency_max_ms
            .fetch_max(millis(latency), Ordering::Relaxed);
    }

    fn timed_out(&mut self) {
        self.settled = true;
        saturating_add(&self.lane.counters.timeouts, 1);
    }
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.lane.counters.in_flight.fetch_sub(1, Ordering::Relaxed);
        if !self.settled {
            saturating_add(&self.lane.counters.cancellations, 1);
        }
    }
}

pub struct DockerAdmission {
    started: Instant,
    pull_deadline: Duration,
    read: Lane,
    lifecycle: Lane,
}

impl DockerAdmission {
    pub fn new(limits: DockerAdmissionLimits) -> Self {
        Self {
            started: Instant::now(),
            pull_deadline: limits.pull_deadline,
            read: Lane::new(
                DockerOperationClass::Read,
                limits.read_concurrency,
                limits.read_deadline,
                limits.read_queue_wait,
            ),
            lifecycle: Lane::new(
                DockerOperationClass::Lifecycle,
                limits.lifecycle_concurrency,
                limits.lifecycle_deadline,
                limits.lifecycle_queue_wait,
            ),
        }
    }

    pub fn metrics(&self) -> DockerAdmissionMetrics {
        DockerAdmissionMetrics {
            read: self.read.snapshot(),
            lifecycle: self.lifecycle.snapshot(),
        }
    }

    fn lane(&self, class: DockerOperationClass) -> &Lane {
        match class {
            DockerOperationClass::Read => &self.read,
            DockerOperationClass::Lifecycle => &self.lifecycle,
        }
    }

    /// Admit one short-lived call and run it under `deadline`. The outer
    /// error is the admission verdict; the inner value is the call's output.
    pub async fn run<F: Future>(
        &self,
        class: DockerOperationClass,
        operation: &'static str,
        deadline: Duration,
        future: F,
    ) -> Result<F::Output, DockerAdmissionError> {
        let lane = self.lane(class);
        lane.counters.queued.fetch_add(1, Ordering::Relaxed);
        let queued_at = Instant::now();
        let acquired =
            tokio::time::timeout(lane.queue_wait, Arc::clone(&lane.slots).acquire_owned()).await;
        lane.counters.queued.fetch_sub(1, Ordering::Relaxed);
        let permit = match acquired {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) => return Err(DockerAdmissionError::Closed),
            Err(_) => {
                saturating_add(&lane.counters.overloaded, 1);
                let error = DockerAdmissionError::Overloaded {
                    class,
                    operation,
                    waited: queued_at.elapsed(),
                };
                lane.warn_throttled(self.started.elapsed(), &error);
                return Err(error);
            }
        };
        let waited = millis(queued_at.elapsed());
        saturating_add(&lane.counters.queue_wait_total_ms, waited);
        lane.counters
            .queue_wait_max_ms
            .fetch_max(waited, Ordering::Relaxed);

        let mut in_flight = InFlight::new(lane, permit);
        let started = Instant::now();
        match tokio::time::timeout(deadline, future).await {
            Ok(output) => {
                in_flight.completed(started.elapsed());
                Ok(output)
            }
            Err(_) => {
                // `timeout` dropped the future and its daemon response stream.
                in_flight.timed_out();
                let error = DockerAdmissionError::Timeout {
                    class,
                    operation,
                    deadline,
                };
                lane.warn_throttled(self.started.elapsed(), &error);
                Err(error)
            }
        }
    }

    pub async fn read<F: Future>(
        &self,
        operation: &'static str,
        future: F,
    ) -> Result<F::Output, DockerAdmissionError> {
        self.run(
            DockerOperationClass::Read,
            operation,
            self.read.deadline,
            future,
        )
        .await
    }

    pub async fn lifecycle<F: Future>(
        &self,
        operation: &'static str,
        future: F,
    ) -> Result<F::Output, DockerAdmissionError> {
        self.run(
            DockerOperationClass::Lifecycle,
            operation,
            self.lifecycle.deadline,
            future,
        )
        .await
    }

    /// Pulls hold a lifecycle slot under the longer pull deadline.
    pub async fn pull<F: Future>(
        &self,
        operation: &'static str,
        future: F,
    ) -> Result<F::Output, DockerAdmissionError> {
        self.run(
            DockerOperationClass::Lifecycle,
            operation,
            self.pull_deadline,
            future,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::*;

    fn limits(concurrency: usize, deadline_ms: u64, queue_wait_ms: u64) -> DockerAdmissionLimits {
        DockerAdmissionLimits {
            read_concurrency: concurrency,
            read_deadline: Duration::from_millis(deadline_ms),
            read_queue_wait: Duration::from_millis(queue_wait_ms),
            lifecycle_concurrency: concurrency,
            lifecycle_deadline: Duration::from_millis(deadline_ms),
            lifecycle_queue_wait: Duration::from_millis(queue_wait_ms),
            pull_deadline: Duration::from_millis(deadline_ms * 4),
        }
    }

    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn concurrency_cap_rejects_the_extra_caller_as_runtime_unavailable() {
        let admission = Arc::new(DockerAdmission::new(limits(1, 5_000, 50)));
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let holder = {
            let admission = Arc::clone(&admission);
            tokio::spawn(async move {
                admission
                    .lifecycle("create_container", async move {
                        let _ = release_rx.await;
                        1_u8
                    })
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(admission.metrics().lifecycle.in_flight, 1);

        let rejected = admission.lifecycle("start_container", async { 2_u8 }).await;
        assert!(matches!(
            rejected,
            Err(DockerAdmissionError::Overloaded {
                class: DockerOperationClass::Lifecycle,
                operation: "start_container",
                ..
            })
        ));
        assert_eq!(admission.metrics().lifecycle.overloaded, 1);
        let runtime_error: RuntimeError = rejected.unwrap_err().into();
        assert_eq!(runtime_error.code, CommandErrorCode::RuntimeUnavailable);

        release_tx.send(()).unwrap();
        assert_eq!(holder.await.unwrap(), Ok(1));
        assert_eq!(
            admission.lifecycle("stop_container", async { 3_u8 }).await,
            Ok(3)
        );
        let metrics = admission.metrics().lifecycle;
        assert_eq!(metrics.in_flight, 0);
        assert_eq!(metrics.completed, 2);
        assert_eq!(metrics.admitted, 2);
    }

    #[tokio::test]
    async fn queued_caller_is_admitted_when_a_slot_frees_and_records_its_wait() {
        let admission = Arc::new(DockerAdmission::new(limits(1, 5_000, 2_000)));
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let holder = {
            let admission = Arc::clone(&admission);
            tokio::spawn(async move {
                admission
                    .read("inspect_container", async move {
                        let _ = release_rx.await;
                    })
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(60)).await;
            release_tx.send(()).unwrap();
        });
        let waited_at = Instant::now();
        admission.read("list_containers", async {}).await.unwrap();
        assert!(waited_at.elapsed() >= Duration::from_millis(50));
        holder.await.unwrap().unwrap();

        let metrics = admission.metrics().read;
        assert_eq!(metrics.overloaded, 0);
        assert_eq!(metrics.completed, 2);
        assert!(metrics.queue_wait_max_ms >= 50);
    }

    #[tokio::test]
    async fn deadline_drops_the_wrapped_future_and_maps_to_timeout() {
        let admission = DockerAdmission::new(limits(4, 40, 100));
        let dropped = Arc::new(AtomicBool::new(false));
        let flag = DropFlag(Arc::clone(&dropped));
        let result = admission
            .read("download_from_container", async move {
                let _flag = flag;
                std::future::pending::<()>().await;
            })
            .await;
        assert!(matches!(
            result,
            Err(DockerAdmissionError::Timeout {
                operation: "download_from_container",
                ..
            })
        ));
        assert!(dropped.load(Ordering::SeqCst));
        let runtime_error: RuntimeError = result.unwrap_err().into();
        assert_eq!(runtime_error.code, CommandErrorCode::Timeout);

        let metrics = admission.metrics().read;
        assert_eq!(metrics.timeouts, 1);
        assert_eq!(metrics.cancellations, 0);
        assert_eq!(metrics.in_flight, 0);
    }

    #[tokio::test]
    async fn caller_cancellation_releases_the_slot_and_counts_a_cancellation() {
        let admission = Arc::new(DockerAdmission::new(limits(1, 5_000, 50)));
        let task = {
            let admission = Arc::clone(&admission);
            tokio::spawn(async move {
                admission
                    .read("inspect_container", std::future::pending::<()>())
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        task.abort();
        let _ = task.await;

        let metrics = admission.metrics().read;
        assert_eq!(metrics.in_flight, 0);
        assert_eq!(metrics.cancellations, 1);
        assert_eq!(admission.read("ping", async { 1 }).await, Ok(1));
    }

    #[tokio::test]
    async fn pull_gets_the_longer_deadline_on_the_lifecycle_lane() {
        let admission = DockerAdmission::new(limits(1, 30, 50));
        let result = admission
            .pull("create_image", async {
                tokio::time::sleep(Duration::from_millis(60)).await;
                "pulled"
            })
            .await;
        assert_eq!(result, Ok("pulled"));
        assert_eq!(admission.metrics().lifecycle.completed, 1);
    }
}
