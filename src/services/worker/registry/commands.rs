use std::collections::HashMap;
use std::time::{Duration, Instant};

use rsctf_worker_protocol::{ControlMessage, WorkloadFence};
use uuid::Uuid;

#[derive(Default)]
pub(super) struct CommandTracker {
    pub(super) by_id: HashMap<Uuid, InFlightCommand>,
    pub(super) by_workload: HashMap<Uuid, Uuid>,
}

#[derive(Clone)]
pub(super) struct InFlightCommand {
    pub(super) command_id: Uuid,
    pub(super) message_id: Uuid,
    pub(super) fence: WorkloadFence,
    pub(super) spec_hash: String,
    pub(super) deadline: Instant,
}

pub(super) fn lifecycle_command(
    body: &ControlMessage,
    message_id: Uuid,
) -> Option<InFlightCommand> {
    let (command_id, fence, spec_hash, timeout_ms) = match body {
        ControlMessage::EnsureWorkload(command) => (
            command.command_id,
            command.fence,
            command.spec_hash.clone(),
            command.timeout_ms,
        ),
        ControlMessage::EnsureAbsent(command) => (
            command.command_id,
            command.fence,
            command.spec_hash.clone(),
            command.timeout_ms,
        ),
        _ => return None,
    };
    let operation_timeout = Duration::from_millis(timeout_ms.clamp(1, 300_000));
    Some(InFlightCommand {
        command_id,
        message_id,
        fence,
        spec_hash,
        deadline: Instant::now() + operation_timeout + Duration::from_secs(10),
    })
}

pub(super) fn remove_tracked_command(tracker: &mut CommandTracker, command: &InFlightCommand) {
    if tracker.by_id.remove(&command.command_id).is_some()
        && tracker
            .by_workload
            .get(&command.fence.workload_id)
            .is_some_and(|id| *id == command.command_id)
    {
        tracker.by_workload.remove(&command.fence.workload_id);
    }
}

pub(super) fn prune_expired_commands(tracker: &mut CommandTracker, now: Instant) {
    tracker.by_id.retain(|_, command| command.deadline > now);
    let active = &tracker.by_id;
    tracker
        .by_workload
        .retain(|_, command_id| active.contains_key(command_id));
}
