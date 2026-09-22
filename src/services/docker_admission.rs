//! Bounded admission and deadlines for short-lived Docker Engine API work.
//!
//! One Docker daemon serves every replica-local container backend, image
//! sweep, and variant generator in this process. Without a bound, a burst of
//! polled `inspect`/`stats` reads or a wave of `create`/`remove` calls can
//! saturate the daemon and stall gameplay before any operator notices. Two
//! admission classes keep cheap reads and expensive lifecycle calls from
//! starving each other:
//!
//! - **Read**: `inspect`, one-shot `stats`, `list`, and bounded `logs`/file
//!   downloads. Many may run at once, each under a short deadline.
//! - **Lifecycle**: `create`, `start`, `stop`, `remove`, network creation,
//!   uploads, and image pulls. Few may run at once; pulls have a longer
//!   deadline than the other lifecycle calls.
//!
//! Interactive exec sessions, container exports, and other long-lived streams
//! keep their own admission and cancellation owners and never pass through
//! this wrapper. Every wrapped future owns its response stream, so a timeout
//! drops the stream and cancels the daemon transfer instead of letting the
//! request linger.

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::utils::error::AppError;

const DEFAULT_READ_CONCURRENCY: usize = 16;
const DEFAULT_READ_DEADLINE: Duration = Duration::from_secs(5);
const DEFAULT_READ_QUEUE_WAIT: Duration = Duration::from_secs(2);
const DEFAULT_LIFECYCLE_CONCURRENCY: usize = 4;
const DEFAULT_LIFECYCLE_DEADLINE: Duration = Duration::from_secs(30);
const DEFAULT_LIFECYCLE_QUEUE_WAIT: Duration = Duration::from_secs(15);
const DEFAULT_PULL_DEADLINE: Duration = Duration::from_secs(120);
const MAX_CONCURRENCY: usize = 256;
const MAX_SECONDS: u64 = 3_600;
/// Background sweeps and pollers share one warning per class per window so a
/// saturated daemon is visible in logs without a log storm.
const PRESSURE_LOG_WINDOW: Duration = Duration::from_secs(30);

pub const READ_CONCURRENCY_ENV: &str = "RSCTF_DOCKER_READ_CONCURRENCY";
pub const READ_DEADLINE_ENV: &str = "RSCTF_DOCKER_READ_DEADLINE_SECS";
pub const READ_QUEUE_WAIT_ENV: &str = "RSCTF_DOCKER_READ_QUEUE_WAIT_SECS";
pub const LIFECYCLE_CONCURRENCY_ENV: &str = "RSCTF_DOCKER_LIFECYCLE_CONCURRENCY";
pub const LIFECYCLE_DEADLINE_ENV: &str = "RSCTF_DOCKER_LIFECYCLE_DEADLINE_SECS";
pub const LIFECYCLE_QUEUE_WAIT_ENV: &str = "RSCTF_DOCKER_LIFECYCLE_QUEUE_WAIT_SECS";
pub const PULL_DEADLINE_ENV: &str = "RSCTF_DOCKER_PULL_DEADLINE_SECS";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
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
            read_concurrency: DEFAULT_READ_CONCURRENCY,
            read_deadline: DEFAULT_READ_DEADLINE,
            read_queue_wait: DEFAULT_READ_QUEUE_WAIT,
            lifecycle_concurrency: DEFAULT_LIFECYCLE_CONCURRENCY,
            lifecycle_deadline: DEFAULT_LIFECYCLE_DEADLINE,
            lifecycle_queue_wait: DEFAULT_LIFECYCLE_QUEUE_WAIT,
            pull_deadline: DEFAULT_PULL_DEADLINE,
        }
    }
}

impl DockerAdmissionLimits {
    /// Read the documented environment overrides. An absent, empty, or
    /// out-of-range value keeps the validated default and logs once.
    pub fn from_env() -> Self {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let defaults = Self::default();
        Self {
            read_concurrency: bounded_usize(
                &lookup,
                READ_CONCURRENCY_ENV,
                defaults.read_concurrency,
            ),
            read_deadline: bounded_secs(&lookup, READ_DEADLINE_ENV, defaults.read_deadline),
            read_queue_wait: bounded_secs(&lookup, READ_QUEUE_WAIT_ENV, defaults.read_queue_wait),
            lifecycle_concurrency: bounded_usize(
                &lookup,
                LIFECYCLE_CONCURRENCY_ENV,
                defaults.lifecycle_concurrency,
            ),
            lifecycle_deadline: bounded_secs(
                &lookup,
                LIFECYCLE_DEADLINE_ENV,
                defaults.lifecycle_deadline,
            ),
            lifecycle_queue_wait: bounded_secs(
                &lookup,
                LIFECYCLE_QUEUE_WAIT_ENV,
                defaults.lifecycle_queue_wait,
            ),
            pull_deadline: bounded_secs(&lookup, PULL_DEADLINE_ENV, defaults.pull_deadline),
        }
    }
}

fn bounded_value<T: Copy + PartialOrd + std::str::FromStr + std::fmt::Display>(
    lookup: &impl Fn(&str) -> Option<String>,
    name: &str,
    default: T,
    min: T,
    max: T,
) -> T {
    let Some(raw) = lookup(name).map(|value| value.trim().to_string()) else {
        return default;
    };
    if raw.is_empty() {
        return default;
    }
    match raw.parse::<T>() {
        Ok(value) if value >= min && value <= max => value,
        _ => {
            tracing::warn!(
                variable = name,
                value = %raw,
                %default,
                %min,
                %max,
                "ignoring invalid Docker admission override; using the default"
            );
            default
        }
    }
}

fn bounded_usize(lookup: &impl Fn(&str) -> Option<String>, name: &str, default: usize) -> usize {
    bounded_value(lookup, name, default, 1, MAX_CONCURRENCY)
}

fn bounded_secs(
    lookup: &impl Fn(&str) -> Option<String>,
    name: &str,
    default: Duration,
) -> Duration {
    Duration::from_secs(bounded_value(
        lookup,
        name,
        default.as_secs(),
        1,
        MAX_SECONDS,
    ))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DockerAdmissionError {
    /// No slot became free before the class queue-wait bound expired.
    Overloaded {
        class: DockerOperationClass,
        operation: &'static str,
        waited: Duration,
    },
    /// The daemon did not finish the call before its deadline; the response
    /// stream has already been dropped.
    Timeout {
        class: DockerOperationClass,
        operation: &'static str,
        deadline: Duration,
    },
    /// The process is shutting down and the semaphore was closed.
    Closed,
}

impl DockerAdmissionError {
    /// Seconds a client should wait before retrying the same call.
    pub fn retry_after_seconds(&self) -> u64 {
        match self {
            Self::Overloaded { waited, .. } => waited.as_secs().clamp(1, 30),
            Self::Timeout { .. } => 5,
            Self::Closed => 5,
        }
    }
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

impl From<DockerAdmissionError> for AppError {
    fn from(error: DockerAdmissionError) -> Self {
        let retry_after = error.retry_after_seconds();
        match error {
            DockerAdmissionError::Overloaded { .. } => {
                AppError::overloaded("The container host is busy; retry shortly", retry_after)
            }
            DockerAdmissionError::Timeout { .. } => AppError::retryable_unavailable(
                "The container host did not respond in time; retry shortly",
                retry_after,
            ),
            DockerAdmissionError::Closed => {
                AppError::unavailable("The container host is shutting down")
            }
        }
    }
}

/// Fixed-cardinality counters for one admission class. Sums and maxima let an
/// operator derive averages without per-call storage.
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

fn record_max(counter: &AtomicU64, value: u64) {
    counter.fetch_max(value, Ordering::Relaxed);
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

struct Lane {
    class: DockerOperationClass,
    slots: Arc<Semaphore>,
    concurrency: usize,
    deadline: Duration,
    queue_wait: Duration,
    counters: ClassCounters,
}

/// Snapshot of one admission class for the operator metrics surface.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DockerClassMetrics {
    pub concurrency_limit: usize,
    pub deadline_ms: u64,
    pub queue_wait_limit_ms: u64,
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

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DockerAdmissionMetrics {
    pub pull_deadline_ms: u64,
    pub read: DockerClassMetrics,
    pub lifecycle: DockerClassMetrics,
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
            queue_wait_limit_ms: millis(self.queue_wait),
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

    /// Log at most once per window per class. Request paths surface the
    /// retryable error to the caller; background sweeps only see this line.
    fn warn_throttled(&self, since_start: Duration, error: &DockerAdmissionError) {
        let now = millis(since_start);
        let window = millis(PRESSURE_LOG_WINDOW);
        let last = self.counters.last_warning_ms.load(Ordering::Relaxed);
        if last != u64::MAX && now.saturating_sub(last) < window {
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
            queue_wait_max_ms = snapshot.queue_wait_max_ms,
            daemon_latency_max_ms = snapshot.daemon_latency_max_ms,
            "Docker daemon admission pressure"
        );
    }
}

/// Decrements the in-flight gauge when the admitted call ends for any reason.
/// A call that neither completed nor timed out was dropped by its caller and
/// counts as a cancellation.
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
        record_max(&self.lane.counters.daemon_latency_max_ms, millis(latency));
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

    fn lane(&self, class: DockerOperationClass) -> &Lane {
        match class {
            DockerOperationClass::Read => &self.read,
            DockerOperationClass::Lifecycle => &self.lifecycle,
        }
    }

    pub fn metrics(&self) -> DockerAdmissionMetrics {
        DockerAdmissionMetrics {
            pull_deadline_ms: millis(self.pull_deadline),
            read: self.read.snapshot(),
            lifecycle: self.lifecycle.snapshot(),
        }
    }

    /// Admit one short-lived call in `class` and run it under `deadline`.
    /// The returned outer error is the admission verdict; the inner value is
    /// whatever the wrapped future produced.
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
        record_max(&lane.counters.queue_wait_max_ms, waited);

        let mut in_flight = InFlight::new(lane, permit);
        let started = Instant::now();
        match tokio::time::timeout(deadline, future).await {
            Ok(output) => {
                in_flight.completed(started.elapsed());
                Ok(output)
            }
            Err(_) => {
                // `timeout` already dropped the future and with it the daemon
                // response stream; only the accounting remains.
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

    /// A read whose caller carries its own tighter budget, such as a sweep
    /// with a pass-level deadline. The class deadline still applies.
    pub async fn read_within<F: Future>(
        &self,
        operation: &'static str,
        budget: Duration,
        future: F,
    ) -> Result<F::Output, DockerAdmissionError> {
        self.run(
            DockerOperationClass::Read,
            operation,
            budget.min(self.read.deadline),
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

    pub async fn lifecycle_within<F: Future>(
        &self,
        operation: &'static str,
        budget: Duration,
        future: F,
    ) -> Result<F::Output, DockerAdmissionError> {
        self.run(
            DockerOperationClass::Lifecycle,
            operation,
            budget.min(self.lifecycle.deadline),
            future,
        )
        .await
    }

    /// Image pulls hold a lifecycle slot but get the longer pull deadline.
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

static GLOBAL: LazyLock<DockerAdmission> =
    LazyLock::new(|| DockerAdmission::new(DockerAdmissionLimits::from_env()));

/// Process-wide admission shared by every local Docker client. The daemon is
/// one resource, so one budget covers the container backend, image sweeps,
/// and variant generators alike.
pub fn docker_admission() -> &'static DockerAdmission {
    &GLOBAL
}

#[cfg(test)]
#[path = "docker_admission/tests.rs"]
mod tests;
