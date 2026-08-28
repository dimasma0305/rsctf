//! Bounded, rotating state for runtime orphan passes.

use std::collections::HashMap;
use std::time::{Duration, Instant};

const ORPHAN_GRACE: Duration = Duration::from_secs(60);
const ORPHAN_SCAN_BATCH: usize = 512;
const ORPHAN_DESTROY_BATCH: usize = 32;
pub(super) const ORPHAN_DESTROY_CONCURRENCY: usize = 4;
const ORPHAN_SWEEP_BUDGET: Duration = Duration::from_secs(18);

pub(super) static ORPHAN_FIRST_SEEN: std::sync::LazyLock<
    std::sync::Mutex<HashMap<String, Instant>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));
static ORPHAN_SCAN_CURSOR: std::sync::LazyLock<std::sync::Mutex<Option<String>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

pub(super) fn inventory_cursor() -> Option<String> {
    ORPHAN_SCAN_CURSOR
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

pub(super) fn advance_inventory_cursor(next: Option<String>) {
    *ORPHAN_SCAN_CURSOR
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = next;
}

#[derive(Clone, Copy)]
pub(super) struct OrphanSweepPolicy {
    pub scan_batch: usize,
    pub destroy_batch: usize,
    pub concurrency: usize,
    pub grace: Duration,
    pub budget: Duration,
}

impl Default for OrphanSweepPolicy {
    fn default() -> Self {
        Self {
            scan_batch: ORPHAN_SCAN_BATCH,
            destroy_batch: ORPHAN_DESTROY_BATCH,
            concurrency: ORPHAN_DESTROY_CONCURRENCY,
            grace: ORPHAN_GRACE,
            budget: ORPHAN_SWEEP_BUDGET,
        }
    }
}

#[cfg(test)]
pub(super) fn managed_scan_batch(mut managed: Vec<String>, limit: usize) -> (Vec<String>, usize) {
    managed.retain(|id| !id.trim().is_empty());
    for id in &mut managed {
        *id = id.trim().to_string();
    }
    managed.sort_unstable();
    managed.dedup();
    let total = managed.len();
    if total == 0 || limit == 0 {
        return (Vec::new(), total);
    }
    let take = limit.min(total);
    let mut cursor = ORPHAN_SCAN_CURSOR
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let start = cursor
        .as_deref()
        .and_then(|after| managed.iter().position(|id| id.as_str() > after))
        .unwrap_or(0);
    let batch = (0..take)
        .map(|offset| managed[(start + offset) % total].clone())
        .collect::<Vec<_>>();
    *cursor = batch.last().cloned();
    (batch, total)
}

#[cfg(test)]
pub(super) fn reset_scan_cursor() {
    advance_inventory_cursor(None);
}
