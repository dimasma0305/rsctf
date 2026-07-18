use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rsctf_worker_protocol::{
    ControlEnvelope, ControlMessage, InventoryItem, InventoryPage, PROTOCOL_REVISION,
};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::{InventoryProgress, SessionContext, WorkerRegistry};
use crate::services::worker::{WorkerError, WorkerResult};

pub(super) struct InventoryReservation {
    total: Arc<AtomicUsize>,
    limit: usize,
    used: usize,
}

impl InventoryReservation {
    fn new(total: Arc<AtomicUsize>, limit: usize) -> Self {
        Self {
            total,
            limit,
            used: 0,
        }
    }

    fn try_grow(&mut self, additional: usize) -> bool {
        loop {
            let current = self.total.load(Ordering::Acquire);
            let Some(next) = current.checked_add(additional) else {
                return false;
            };
            if next > self.limit {
                return false;
            }
            if self
                .total
                .compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                self.used += additional;
                return true;
            }
        }
    }
}

impl Drop for InventoryReservation {
    fn drop(&mut self) {
        self.total.fetch_sub(self.used, Ordering::AcqRel);
    }
}

impl WorkerRegistry {
    pub(crate) async fn collect_inventory(
        &self,
        context: &SessionContext,
        page: InventoryPage,
    ) -> WorkerResult<Option<(Uuid, Vec<InventoryItem>)>> {
        let session = self.current(context.worker_id, &context.fence).await?;
        let mut progress = session.inventory.lock().await;
        let current = progress.as_mut().ok_or(WorkerError::Protocol(
            "inventory snapshot was not requested",
        ))?;
        if current.applying
            || current.snapshot_id != page.snapshot_id
            || current.next_page != page.page
        {
            *progress = None;
            return Err(WorkerError::Protocol("inventory pages are out of order"));
        }
        if page.items.is_empty() && !page.final_page {
            *progress = None;
            return Err(WorkerError::Protocol(
                "non-final inventory pages cannot be empty",
            ));
        }

        let page_number = usize::try_from(page.page)
            .map_err(|_| WorkerError::Protocol("inventory page counter exceeds limits"))?;
        let encoded_bytes = serde_json::to_vec(&page.items)
            .map_err(|error| WorkerError::ProtocolOwned(error.to_string()))?
            .len();
        let replicas = page
            .items
            .iter()
            .map(|item| item.replicas.len())
            .try_fold(0_usize, usize::checked_add)
            .ok_or(WorkerError::Protocol("inventory replica counter overflow"))?;
        if page_number >= self.config.max_inventory_pages
            || current.items.len().saturating_add(page.items.len())
                > self.config.max_inventory_items
            || current.reservation.used.saturating_add(encoded_bytes)
                > self.config.max_inventory_bytes
            || current.replicas.saturating_add(replicas) > self.config.max_inventory_replicas
        {
            *progress = None;
            return Err(WorkerError::Protocol("inventory snapshot exceeds limits"));
        }

        if !current.reservation.try_grow(encoded_bytes) {
            *progress = None;
            return Err(WorkerError::Busy);
        }
        current.replicas += replicas;
        current.items.extend(page.items);
        current.next_page = current
            .next_page
            .checked_add(1)
            .ok_or(WorkerError::Protocol("inventory page counter overflow"))?;
        if page.final_page {
            current.applying = true;
            Ok(Some((
                current.snapshot_id,
                std::mem::take(&mut current.items),
            )))
        } else {
            Ok(None)
        }
    }

    /// Queue one inventory request only when no previous snapshot is being
    /// collected or applied. This prevents slow Docker enumeration or database
    /// adoption from interleaving two page streams on the control connection.
    pub(crate) async fn request_inventory(
        &self,
        worker_id: Uuid,
        timeout: Duration,
    ) -> WorkerResult<bool> {
        let session = self
            .sessions
            .read()
            .await
            .get(&worker_id)
            .cloned()
            .ok_or(WorkerError::Offline)?;
        if !session.lease_is_current(self.config.heartbeat_lease) {
            return Err(WorkerError::Offline);
        }
        let mut progress = session.inventory.lock().await;
        if progress
            .as_ref()
            .is_some_and(|current| current.started_at.elapsed() < timeout)
        {
            return Ok(false);
        }

        let snapshot_id = Uuid::new_v4();
        *progress = Some(InventoryProgress {
            snapshot_id,
            next_page: 0,
            items: Vec::new(),
            reservation: InventoryReservation::new(
                self.inventory_bytes.clone(),
                self.config.max_total_inventory_bytes,
            ),
            replicas: 0,
            started_at: Instant::now(),
            applying: false,
        });
        let envelope = ControlEnvelope {
            protocol_revision: PROTOCOL_REVISION,
            message_id: Uuid::new_v4(),
            reply_to: None,
            session_epoch: session.context.fence.session_epoch,
            body: ControlMessage::InventoryRequest(rsctf_worker_protocol::InventoryRequest {
                command_id: Uuid::new_v4(),
                snapshot_id,
            }),
        };
        if let Err(error) = session.control.try_send(envelope) {
            *progress = None;
            return Err(match error {
                mpsc::error::TrySendError::Full(_) => WorkerError::Busy,
                mpsc::error::TrySendError::Closed(_) => WorkerError::Offline,
            });
        }
        Ok(true)
    }

    pub(crate) async fn complete_inventory(
        &self,
        context: &SessionContext,
        snapshot_id: Uuid,
    ) -> WorkerResult<()> {
        let session = self.current(context.worker_id, &context.fence).await?;
        let mut progress = session.inventory.lock().await;
        if progress
            .as_ref()
            .is_some_and(|current| current.snapshot_id == snapshot_id && current.applying)
        {
            *progress = None;
            Ok(())
        } else {
            Err(WorkerError::Protocol("inventory snapshot state changed"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reservations_bound_all_concurrent_inventory_snapshots() {
        let total = Arc::new(AtomicUsize::new(0));
        let mut first = InventoryReservation::new(total.clone(), 10);
        let mut second = InventoryReservation::new(total.clone(), 10);
        assert!(first.try_grow(6));
        assert!(!second.try_grow(5));
        assert_eq!(total.load(Ordering::Acquire), 6);

        drop(first);
        assert_eq!(total.load(Ordering::Acquire), 0);
        assert!(second.try_grow(5));
        assert_eq!(total.load(Ordering::Acquire), 5);
        drop(second);
        assert_eq!(total.load(Ordering::Acquire), 0);
    }
}
