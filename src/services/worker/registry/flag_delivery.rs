use std::time::{Duration, Instant};

use rsctf_worker_protocol::{ControlMessage, SessionFence, WriteFlag};
use tokio::sync::watch;
use uuid::Uuid;

use super::{Completion, InFlightCommand, WorkerError, WorkerRegistry, WorkerResult};

#[cfg(test)]
mod tests;

impl WorkerRegistry {
    /// Wait for the addressed worker session to confirm file readback on all replicas.
    pub async fn write_flag_confirmed(
        &self,
        worker_id: Uuid,
        fence: &SessionFence,
        command: WriteFlag,
    ) -> WorkerResult<Uuid> {
        let session = self.current(worker_id, fence).await?;
        if !session.verified_flag_delivery {
            return Err(WorkerError::Protocol("worker lacks verified flag delivery"));
        }
        if !(1..=300_000).contains(&command.timeout_ms) {
            return Err(WorkerError::Protocol("invalid flag delivery timeout"));
        }
        let deadline = Instant::now() + Duration::from_millis(command.timeout_ms);
        let message_id = Uuid::new_v4();
        let (sender, mut receipt) = watch::channel(false);
        let mut shutdown = session.shutdown.subscribe();
        let tracked = InFlightCommand {
            command_id: command.command_id,
            message_id,
            fence: command.fence,
            completion: Completion::FlagReadback(sender),
            deadline,
        };
        let enqueue = self.enqueue(
            &session,
            message_id,
            ControlMessage::WriteFlag(command),
            Some(tracked),
        );
        tokio::time::timeout_at(deadline.into(), enqueue)
            .await
            .map_err(|_| WorkerError::FlagUnconfirmed)??;
        tokio::select! {
            biased;
            _ = shutdown.changed() => return Err(WorkerError::StaleSession),
            _ = tokio::time::sleep_until(deadline.into()) => return Err(WorkerError::FlagUnconfirmed),
            changed = receipt.changed() => {
                if changed.is_err() || !*receipt.borrow() {
                    return Err(WorkerError::FlagUnconfirmed);
                }
            }
        }
        let current = self.current(worker_id, fence).await?;
        if !current.lease_is_current(self.config.heartbeat_lease) {
            return Err(WorkerError::Offline);
        }
        Ok(message_id)
    }
}
