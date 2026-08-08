use std::time::Duration;

use chrono::{DateTime, Utc};

// One marker read follows the functional probe and the verdict still needs a
// durable transaction. The pre-probe marker is guarded separately after it
// completes, so it cannot consume this reserved tail.
pub(super) const KOTH_COMPLETION_MARGIN: Duration = Duration::from_secs(4);

// A referee learns a new round by polling its signed context. Give the bundled
// five-second poll cadence one bounded arrival window before sampling. This
// never carries evidence across rounds: the database read still requires the
// exact round, cycle, reset attempt, and container identity.
pub(super) const API_SNAPSHOT_ARRIVAL_GRACE: Duration = Duration::from_secs(6);
pub(super) const API_SNAPSHOT_POLL_INTERVAL: Duration = Duration::from_millis(250);
pub(super) const API_MAX_PROBE_BUDGET: Duration = Duration::from_secs(10);

pub(super) fn api_settlement_start_instant(
    round_end: DateTime<Utc>,
    wall_now: DateTime<Utc>,
    monotonic_now: tokio::time::Instant,
) -> tokio::time::Instant {
    let cutoff = round_end
        - chrono::Duration::seconds(
            crate::services::ad::engine::koth_api::API_WAVE_SETTLEMENT_LAG_SECONDS,
        );
    let remaining = cutoff
        .signed_duration_since(wall_now)
        .to_std()
        .unwrap_or_default();
    monotonic_now + remaining
}

pub(super) fn api_snapshot_arrival_deadline(
    effective_deadline: tokio::time::Instant,
    planned_timeout: Duration,
    now: tokio::time::Instant,
) -> tokio::time::Instant {
    let reserved = planned_timeout
        .checked_add(KOTH_COMPLETION_MARGIN)
        .unwrap_or(Duration::MAX);
    let latest_safe_probe_start = effective_deadline.checked_sub(reserved).unwrap_or(now);
    std::cmp::min(
        now.checked_add(API_SNAPSHOT_ARRIVAL_GRACE)
            .unwrap_or(latest_safe_probe_start),
        latest_safe_probe_start,
    )
}
