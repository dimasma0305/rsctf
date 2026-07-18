use chrono::{DateTime, Utc};
use rsctf_worker_protocol::ValidatedWorkloadSpec;
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum WorkerStoreError {
    #[error("worker store database error: {0}")]
    Database(#[source] sqlx::Error),
    #[error("worker store contains invalid data: {0}")]
    InvalidStoredData(String),
    #[error("invalid worker request: {0}")]
    InvalidInput(String),
    #[error("worker conflict: {0}")]
    Conflict(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkerAdministrativeState {
    Enabled,
    Draining,
    Disabled,
}

impl WorkerAdministrativeState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Enabled => "Enabled",
            Self::Draining => "Draining",
            Self::Disabled => "Disabled",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, WorkerStoreError> {
        match value {
            "Enabled" => Ok(Self::Enabled),
            "Draining" => Ok(Self::Draining),
            "Disabled" => Ok(Self::Disabled),
            value => Err(WorkerStoreError::InvalidStoredData(format!(
                "unknown worker administrative state {value:?}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlatformOs {
    Linux,
    Windows,
}

impl PlatformOs {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::Windows => "windows",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, WorkerStoreError> {
        match value {
            "linux" => Ok(Self::Linux),
            "windows" => Ok(Self::Windows),
            value => Err(WorkerStoreError::InvalidStoredData(format!(
                "unknown worker platform {value:?}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkloadDesiredState {
    Present,
    Absent,
}

impl WorkloadDesiredState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Present => "Present",
            Self::Absent => "Absent",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, WorkerStoreError> {
        match value {
            "Present" => Ok(Self::Present),
            "Absent" => Ok(Self::Absent),
            value => Err(WorkerStoreError::InvalidStoredData(format!(
                "unknown workload desired state {value:?}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkloadObservedState {
    Unknown,
    Reconciling,
    Ready,
    Degraded,
    Failed,
    Absent,
    Lost,
}

impl WorkloadObservedState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::Reconciling => "Reconciling",
            Self::Ready => "Ready",
            Self::Degraded => "Degraded",
            Self::Failed => "Failed",
            Self::Absent => "Absent",
            Self::Lost => "Lost",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, WorkerStoreError> {
        match value {
            "Unknown" => Ok(Self::Unknown),
            "Reconciling" => Ok(Self::Reconciling),
            "Ready" => Ok(Self::Ready),
            "Degraded" => Ok(Self::Degraded),
            "Failed" => Ok(Self::Failed),
            "Absent" => Ok(Self::Absent),
            "Lost" => Ok(Self::Lost),
            value => Err(WorkerStoreError::InvalidStoredData(format!(
                "unknown workload observed state {value:?}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourceReservation {
    pub cpu_millis: i64,
    pub memory_bytes: i64,
    /// Number of isolated workload networks. A workload definition uses one;
    /// worker capacity may advertise many.
    pub slots: i32,
}

impl ResourceReservation {
    pub(crate) fn validate(self) -> Result<(), WorkerStoreError> {
        if self.cpu_millis < 0 || self.memory_bytes < 0 || self.slots < 0 {
            return Err(WorkerStoreError::InvalidInput(
                "resource reservations cannot be negative".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct CreateWorker {
    pub id: Uuid,
    pub name: String,
    pub enrollment_token_hash: [u8; 32],
    pub enrollment_token_expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct WorkerCertificate {
    pub fingerprint_sha256: [u8; 32],
    pub serial: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct WorkerInventory {
    pub platform_os: PlatformOs,
    pub architecture: String,
    pub runtime_kind: String,
    pub runtime_version: String,
    pub labels: Value,
    pub capabilities: Value,
    pub capacity: ResourceReservation,
}

impl WorkerInventory {
    pub(crate) fn validate(&self) -> Result<(), WorkerStoreError> {
        for (name, value) in [
            ("architecture", self.architecture.as_str()),
            ("runtime kind", self.runtime_kind.as_str()),
            ("runtime version", self.runtime_version.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(WorkerStoreError::InvalidInput(format!(
                    "{name} cannot be empty"
                )));
            }
        }
        if !self.labels.is_object() || !self.capabilities.is_object() {
            return Err(WorkerStoreError::InvalidInput(
                "worker labels and capabilities must be JSON objects".to_owned(),
            ));
        }
        self.capacity.validate()
    }
}

#[derive(Clone, Debug)]
pub struct WorkerNode {
    pub id: Uuid,
    pub name: String,
    pub administrative_state: WorkerAdministrativeState,
    pub platform_os: Option<PlatformOs>,
    pub architecture: Option<String>,
    pub runtime_kind: Option<String>,
    pub runtime_version: Option<String>,
    pub labels: Value,
    pub capabilities: Value,
    pub capacity: ResourceReservation,
    pub certificate_serial: Option<String>,
    pub certificate_expires_at: Option<DateTime<Utc>>,
    pub session_id: Option<Uuid>,
    pub session_epoch: i64,
    pub boot_id: Option<Uuid>,
    pub heartbeat_at: Option<DateTime<Utc>>,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthenticatedWorker {
    pub id: Uuid,
    pub administrative_state: WorkerAdministrativeState,
    pub certificate_expires_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionFence {
    pub worker_id: Uuid,
    pub session_id: Uuid,
    pub session_epoch: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkerSession {
    pub fence: SessionFence,
    pub lease_expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct WorkloadDefinition {
    pub spec: Value,
    pub spec_hash_sha256: [u8; 32],
    pub required_os: PlatformOs,
    pub required_architecture: String,
    pub required_runtime: String,
    pub reservation: ResourceReservation,
}

impl WorkloadDefinition {
    /// Validate the durable specification and return its derived replica count.
    pub(crate) fn validate(&self) -> Result<i32, WorkerStoreError> {
        if !self.spec.is_object() {
            return Err(WorkerStoreError::InvalidInput(
                "workload specification must be a JSON object".to_owned(),
            ));
        }
        if !rsctf_worker_protocol::is_valid_architecture(&self.required_architecture)
            || self.required_runtime.trim().is_empty()
        {
            return Err(WorkerStoreError::InvalidInput(
                "required architecture or runtime is invalid".to_owned(),
            ));
        }
        self.reservation.validate()?;
        if self.reservation.slots != 1 {
            return Err(WorkerStoreError::InvalidInput(
                "a workload must reserve exactly one isolated-network slot".to_owned(),
            ));
        }
        let spec: ValidatedWorkloadSpec =
            serde_json::from_value(self.spec.clone()).map_err(|error| {
                WorkerStoreError::InvalidInput(format!(
                    "workload specification is invalid: {error}"
                ))
            })?;
        if self.required_architecture != spec.platform.architecture {
            return Err(WorkerStoreError::InvalidInput(
                "required architecture does not match the workload specification".to_owned(),
            ));
        }
        let replicas = spec
            .services
            .iter()
            .map(|service| i32::from(service.replicas))
            .sum();
        Ok(replicas)
    }
}

#[derive(Clone, Debug)]
pub struct PlaceWorkload {
    pub id: Uuid,
    pub owner_kind: String,
    pub owner_key: String,
    pub assignment_id: Uuid,
    pub definition: WorkloadDefinition,
    /// Pin worker-local image identities to one exact host. `None` permits the
    /// best-fit scheduler to select any compatible live worker.
    pub exact_worker_id: Option<Uuid>,
    /// JSON object which must be contained in the worker's labels.
    pub required_labels: Value,
}

#[derive(Clone, Debug)]
pub struct UpdateWorkload {
    pub id: Uuid,
    pub assignment_id: Uuid,
    pub expected_generation: i64,
    pub definition: WorkloadDefinition,
}

#[derive(Clone, Debug)]
pub struct WorkerWorkload {
    pub id: Uuid,
    pub owner_kind: String,
    pub owner_key: String,
    pub worker_id: Uuid,
    pub assignment_id: Uuid,
    pub generation: i64,
    pub definition: WorkloadDefinition,
    pub required_labels: Value,
    pub desired_state: WorkloadDesiredState,
    pub observed_state: WorkloadObservedState,
    pub observed_session_epoch: Option<i64>,
    pub observed_message: Option<String>,
    pub observed_at: Option<DateTime<Utc>>,
    pub ready_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct WorkloadPlacement {
    pub workload: WorkerWorkload,
    pub session: WorkerSession,
}

#[derive(Clone, Debug)]
pub enum PlacementOutcome {
    Placed(WorkloadPlacement),
    NoCompatibleCapacity,
    AlreadyExists(WorkerWorkload),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DefinitionUpdateOutcome {
    Updated { generation: i64 },
    Stale,
    WorkerNoLongerCompatible,
    InsufficientCapacity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesiredUpdateOutcome {
    Updated { generation: i64 },
    Stale,
}

#[derive(Clone, Debug)]
pub struct WorkloadStatus {
    pub session: SessionFence,
    pub workload_id: Uuid,
    pub assignment_id: Uuid,
    pub generation: i64,
    pub spec_hash_sha256: [u8; 32],
    pub state: WorkloadObservedState,
    pub message: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusUpdateOutcome {
    Applied,
    Stale,
}

#[derive(Clone, Debug)]
pub struct DueWorkload {
    pub workload: WorkerWorkload,
    pub session: WorkerSession,
}
