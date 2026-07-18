use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use rsctf_worker_protocol::{ControlEnvelope, InventoryItem, SessionFence};
use tokio::sync::{mpsc, watch, Mutex, RwLock};
use uuid::Uuid;

use super::commands::CommandTracker;
use super::inventory::InventoryReservation;
use super::{now_millis, DataLane};

#[derive(Clone, Debug)]
pub struct SessionContext {
    pub worker_id: Uuid,
    pub boot_id: Uuid,
    pub certificate_fingerprint_sha256: [u8; 32],
    pub fence: SessionFence,
}

#[derive(Clone, Debug)]
pub struct RegistryConfig {
    pub max_workers: usize,
    pub max_data_lanes_per_worker: usize,
    pub control_queue_capacity: usize,
    pub max_in_flight_commands_per_worker: usize,
    pub heartbeat_lease: Duration,
    pub max_inventory_items: usize,
    pub max_inventory_pages: usize,
    pub max_inventory_bytes: usize,
    pub max_total_inventory_bytes: usize,
    pub max_inventory_replicas: usize,
}

impl Default for RegistryConfig {
    fn default() -> Self {
        Self {
            max_workers: 4_096,
            max_data_lanes_per_worker: 4,
            control_queue_capacity: 256,
            max_in_flight_commands_per_worker: 64,
            heartbeat_lease: Duration::from_secs(30),
            max_inventory_items: 4_096,
            max_inventory_pages: 64,
            max_inventory_bytes: 16 * 1024 * 1024,
            max_total_inventory_bytes: 64 * 1024 * 1024,
            max_inventory_replicas: 65_536,
        }
    }
}

pub(crate) struct SessionRegistration {
    pub context: SessionContext,
    pub outbound: mpsc::Receiver<ControlEnvelope>,
    pub shutdown: watch::Receiver<bool>,
}

pub(super) struct SessionEntry {
    pub(super) context: SessionContext,
    pub(super) max_data_lanes: usize,
    pub(super) control: mpsc::Sender<ControlEnvelope>,
    pub(super) shutdown: watch::Sender<bool>,
    pub(super) lanes: RwLock<HashMap<u16, DataLane>>,
    pub(super) next_lane: AtomicUsize,
    pub(super) last_heartbeat_ms: AtomicU64,
    pub(super) inventory: Mutex<Option<InventoryProgress>>,
    pub(super) commands: Mutex<CommandTracker>,
}

pub(super) struct InventoryProgress {
    pub(super) snapshot_id: Uuid,
    pub(super) next_page: u32,
    pub(super) items: Vec<InventoryItem>,
    pub(super) reservation: InventoryReservation,
    pub(super) replicas: usize,
    pub(super) started_at: Instant,
    pub(super) applying: bool,
}

impl SessionEntry {
    pub(super) fn new(
        context: SessionContext,
        max_data_lanes: usize,
        control: mpsc::Sender<ControlEnvelope>,
        shutdown: watch::Sender<bool>,
    ) -> Self {
        Self {
            context,
            max_data_lanes,
            control,
            shutdown,
            lanes: RwLock::new(HashMap::new()),
            next_lane: AtomicUsize::new(0),
            last_heartbeat_ms: AtomicU64::new(now_millis()),
            inventory: Mutex::new(None),
            commands: Mutex::new(CommandTracker::default()),
        }
    }

    pub(super) fn touch(&self) {
        self.last_heartbeat_ms
            .store(now_millis(), Ordering::Release);
    }

    pub(super) fn lease_is_current(&self, lease: Duration) -> bool {
        let age = now_millis().saturating_sub(self.last_heartbeat_ms.load(Ordering::Acquire));
        age <= lease.as_millis().min(u128::from(u64::MAX)) as u64
    }
}
