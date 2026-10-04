use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Duration;

pub(super) const ADMISSION_SHARDS: usize = 64;
pub(super) const MAX_KEYS_PER_SHARD: usize = 128;
pub(super) const GATE_IDLE_TTL: Duration = Duration::from_secs(10 * 60);
pub(super) const SOURCE_WINDOW_SECONDS: u64 = 10;
pub(super) const SOURCE_BURST: u32 = 4;
pub(super) const SAMPLE_EVERY: u32 = 32;
pub(super) const GLOBAL_EVENTS_PER_SECOND: u32 = 256;
pub(super) const QUEUE_CAPACITY: usize = 2_048;
pub(super) const MAX_AGGREGATES_PER_BATCH: usize = 256;
pub(super) const FLUSH_INTERVAL: Duration = Duration::from_millis(250);
pub(super) const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
pub(super) const RETENTION_INTERVAL: Duration = Duration::from_secs(60 * 60);
pub(super) const ROW_BUDGET_SWEEP_INTERVAL: Duration = Duration::from_secs(5);
pub(super) const RETENTION_AGE_DAYS: i64 = 30;
pub(super) const RETENTION_ROW_BUDGET: i64 = 100_000;
// Every aggregate write trims under the same distributed transaction lock.
// A trim batch larger than one admitted write keeps the global budget stable
// regardless of how many API/network replicas are running.
pub(super) const RETENTION_DELETE_BATCH: i64 = 4_096;
pub(super) const BUCKET_MILLIS: i64 = 60_000;
pub(super) const MAX_BAIT_BYTES: usize = 128;
pub(super) const MAX_USER_AGENT_BYTES: usize = 256;

pub(super) fn stable_hash(value: &(impl Hash + ?Sized)) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

pub(super) fn cap_text(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}
