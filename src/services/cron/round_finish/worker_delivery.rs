use std::time::Duration;

use crate::app_state::SharedState;
use crate::services::ad_engine::{worker_flag, AdvancedRoundFlag};
use crate::services::worker::WorkerHandle;

#[cfg(test)]
mod tests;

pub(super) async fn attempt(
    state: &SharedState,
    round_id: i32,
    planted: &AdvancedRoundFlag,
    handle: WorkerHandle,
    timeout: Duration,
    tracker: &super::DeliveryAttemptTracker,
) -> super::DeliveryAttempt {
    if deliver(state, round_id, planted, handle, timeout).await {
        tracker.participant_completed(planted.team_service_id);
        super::DeliveryAttempt::Delivered
    } else {
        // A missing worker receipt alone is insufficient evidence for a team penalty.
        super::DeliveryAttempt::PlatformFailure
    }
}

async fn deliver(
    state: &SharedState,
    round_id: i32,
    planted: &AdvancedRoundFlag,
    handle: WorkerHandle,
    timeout: Duration,
) -> bool {
    worker_flag::deliver(
        state,
        worker_flag::TeamFlag {
            round_id,
            participation_id: planted.participation_id,
            challenge_id: planted.challenge_id,
            value: &planted.flag,
        },
        worker_flag::DeliveryTarget::Published {
            service_id: planted.team_service_id,
        },
        handle,
        timeout,
    )
    .await
}
