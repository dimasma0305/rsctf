//! Small, short-lived cache and single-flight guard for expensive monitor reports.

use bytes::Bytes;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

pub(super) const MAX_CHEAT_REPORT_BYTES: usize = 8 * 1024 * 1024;
const CHEAT_REPORT_CACHE_TTL: Duration = Duration::from_secs(15);
const MAX_CACHED_CHEAT_REPORTS: usize = 64;

pub(super) static CHEAT_REPORT_BUILD_SLOTS: tokio::sync::Semaphore =
    tokio::sync::Semaphore::const_new(2);
pub(super) static CHEAT_REPORT_FLIGHTS: LazyLock<
    crate::utils::single_flight::SingleFlight<CheatReportFill>,
> = LazyLock::new(crate::utils::single_flight::SingleFlight::new);
static CHEAT_REPORT_CACHE: LazyLock<Mutex<HashMap<i32, (Instant, Bytes)>>> =
    LazyLock::new(Default::default);

#[derive(Clone, Default)]
pub(super) enum CheatReportFill {
    Ready(Bytes),
    Busy,
    Oversized,
    Failed(String),
    #[default]
    TimedOut,
}

pub(super) fn cached_cheat_report(game_id: i32) -> Option<Bytes> {
    let now = Instant::now();
    let mut cache = CHEAT_REPORT_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.retain(|_, (expires, _)| *expires > now);
    cache.get(&game_id).map(|(_, body)| body.clone())
}

pub(super) fn cache_cheat_report(game_id: i32, body: &Bytes) {
    let mut cache = CHEAT_REPORT_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if cache.len() >= MAX_CACHED_CHEAT_REPORTS && !cache.contains_key(&game_id) {
        if let Some(oldest) = cache
            .iter()
            .min_by_key(|(_, (expires, _))| *expires)
            .map(|(id, _)| *id)
        {
            cache.remove(&oldest);
        }
    }
    cache.insert(
        game_id,
        (Instant::now() + CHEAT_REPORT_CACHE_TTL, body.clone()),
    );
}
