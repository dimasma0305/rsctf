//! Fixed-cardinality process metrics. No keys, team identities, or flags are retained.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

use serde::Serialize;

mod prometheus;
pub(crate) use prometheus::render;

const UPPER_BOUNDS_MICROS: [u64; 14] = [
    10,
    100,
    1_000,
    5_000,
    10_000,
    25_000,
    50_000,
    100_000,
    250_000,
    500_000,
    1_000_000,
    5_000_000,
    30_000_000,
    u64::MAX,
];

pub(crate) struct Histogram {
    buckets: [AtomicU64; UPPER_BOUNDS_MICROS.len()],
    total_micros: AtomicU64,
}

impl Histogram {
    pub(crate) const fn new() -> Self {
        Self {
            buckets: [const { AtomicU64::new(0) }; UPPER_BOUNDS_MICROS.len()],
            total_micros: AtomicU64::new(0),
        }
    }

    pub(crate) fn observe(&self, duration: Duration) {
        let micros = u64::try_from(duration.as_micros()).unwrap_or(u64::MAX);
        let bucket = UPPER_BOUNDS_MICROS.partition_point(|bound| *bound < micros);
        self.buckets[bucket].fetch_add(1, Relaxed);
        self.total_micros.fetch_add(micros, Relaxed);
    }

    pub(crate) fn timer(&'static self) -> Timer {
        Timer {
            histogram: self,
            started: Instant::now(),
        }
    }

    fn snapshot(&self) -> HistogramSnapshot {
        let counts = self.buckets.each_ref().map(|bucket| bucket.load(Relaxed));
        HistogramSnapshot {
            count: counts.iter().sum(),
            total_micros: self.total_micros.load(Relaxed),
            // The final bucket includes every value above the preceding bound.
            upper_bounds_micros: UPPER_BOUNDS_MICROS,
            counts,
        }
    }
}

pub(crate) struct Timer {
    histogram: &'static Histogram,
    started: Instant,
}

impl Drop for Timer {
    fn drop(&mut self) {
        self.histogram.observe(self.started.elapsed());
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HistogramSnapshot {
    count: u64,
    total_micros: u64,
    upper_bounds_micros: [u64; UPPER_BOUNDS_MICROS.len()],
    counts: [u64; UPPER_BOUNDS_MICROS.len()],
}

pub(crate) static CACHE_L1_HITS: AtomicU64 = AtomicU64::new(0);
pub(crate) static CACHE_L1_MISSES: AtomicU64 = AtomicU64::new(0);
pub(crate) static CACHE_L2_HITS: AtomicU64 = AtomicU64::new(0);
pub(crate) static CACHE_L2_MISSES: AtomicU64 = AtomicU64::new(0);
pub(crate) static CACHE_READ: Histogram = Histogram::new();
pub(crate) static CACHE_L2_READ: Histogram = Histogram::new();
pub(crate) static CHECKER_SCHEDULING_DELAY: Histogram = Histogram::new();
pub(crate) static CHECKER_EXECUTION: Histogram = Histogram::new();
pub(crate) static CHECKER_STARTED: AtomicU64 = AtomicU64::new(0);
pub(crate) static CHECKER_COMPLETED: AtomicU64 = AtomicU64::new(0);
pub(crate) static CHECKER_TIMED_OUT: AtomicU64 = AtomicU64::new(0);
pub(crate) static CHECKER_NO_RUNWAY: AtomicU64 = AtomicU64::new(0);
pub(crate) static SCOREBOARD_REBUILD: Histogram = Histogram::new();
pub(crate) static TRANSACTION_ACQUIRE: Histogram = Histogram::new();
pub(crate) static CONTROL_JOB_QUEUE_DELAY: Histogram = Histogram::new();
pub(crate) static CONTROL_JOB_EXECUTION: Histogram = Histogram::new();
pub(crate) static CONTROL_JOB_STARTED: AtomicU64 = AtomicU64::new(0);
pub(crate) static CONTROL_JOB_FAILED: AtomicU64 = AtomicU64::new(0);
pub(crate) static CONTROL_JOB_SUCCEEDED: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PerformanceMetrics {
    cache_l1_hits: u64,
    cache_l1_misses: u64,
    cache_l2_hits: u64,
    cache_l2_misses: u64,
    cache_read: HistogramSnapshot,
    cache_l2_read: HistogramSnapshot,
    checker_scheduling_delay: HistogramSnapshot,
    checker_execution: HistogramSnapshot,
    checker_started: u64,
    checker_completed: u64,
    checker_timed_out: u64,
    checker_no_runway: u64,
    scoreboard_rebuild: HistogramSnapshot,
}

pub(crate) fn snapshot() -> PerformanceMetrics {
    PerformanceMetrics {
        cache_l1_hits: CACHE_L1_HITS.load(Relaxed),
        cache_l1_misses: CACHE_L1_MISSES.load(Relaxed),
        cache_l2_hits: CACHE_L2_HITS.load(Relaxed),
        cache_l2_misses: CACHE_L2_MISSES.load(Relaxed),
        cache_read: CACHE_READ.snapshot(),
        cache_l2_read: CACHE_L2_READ.snapshot(),
        checker_scheduling_delay: CHECKER_SCHEDULING_DELAY.snapshot(),
        checker_execution: CHECKER_EXECUTION.snapshot(),
        checker_started: CHECKER_STARTED.load(Relaxed),
        checker_completed: CHECKER_COMPLETED.load(Relaxed),
        checker_timed_out: CHECKER_TIMED_OUT.load(Relaxed),
        checker_no_runway: CHECKER_NO_RUNWAY.load(Relaxed),
        scoreboard_rebuild: SCOREBOARD_REBUILD.snapshot(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_records_boundaries_and_overflow_without_unbounded_labels() {
        let histogram = Histogram::new();
        for micros in [0, 10, 11, 100, 101, u64::MAX] {
            histogram.observe(Duration::from_micros(micros));
        }
        let snapshot = histogram.snapshot();
        assert_eq!(snapshot.count, 6);
        assert_eq!(&snapshot.counts[..3], &[2, 2, 1]);
        assert_eq!(snapshot.counts[13], 1);
    }
}
