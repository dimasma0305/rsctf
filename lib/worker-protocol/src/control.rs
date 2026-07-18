use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{FlagTarget, Platform, ValidatedWorkloadSpec, PROTOCOL_REVISION};

/// Maximum isolated workload/network slots one worker may advertise in
/// protocol revision 1.
///
/// Inventory snapshots are independently bounded to this many workloads, and
/// every workload consumes at least one slot. Keeping the two limits aligned
/// prevents a valid advertised capacity from becoming impossible to inventory
/// after reconnect.
pub const MAX_WORKER_SLOTS: u32 = 4_096;
/// Bounded before an authenticated hello can trigger durable session writes.
pub const MAX_AGENT_VERSION_BYTES: usize = 128;
/// Docker/Kubernetes runtime version metadata is operational context, not a
/// free-form log field.
pub const MAX_RUNTIME_VERSION_BYTES: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeDescriptor {
    pub kind: RuntimeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_kind: Option<RuntimeEndpointKind>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeKind {
    Docker,
    Kubernetes,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeEndpointKind {
    UnixSocket,
    WindowsNamedPipe,
    KubernetesConfig,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerCapabilities {
    pub ensure_workload: bool,
    pub write_flag: bool,
    pub tcp_proxy: bool,
    pub interactive_exec: bool,
    pub inventory: bool,
    pub local_image_build: bool,
    pub max_data_lanes: u16,
    /// Maximum total replicas one isolated workload network can contain.
    /// Missing revision-1 advertisements deserialize to zero and are rejected.
    #[serde(default)]
    pub max_workload_replicas: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerCapacity {
    pub cpu_millis: u64,
    pub memory_bytes: u64,
    /// Number of isolated workload networks, not the number of containers.
    pub slots: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerHello {
    pub protocol_revision: u16,
    pub worker_id: Uuid,
    pub boot_id: Uuid,
    pub agent_version: String,
    pub platform: Platform,
    pub runtime: RuntimeDescriptor,
    pub capabilities: WorkerCapabilities,
    pub capacity: WorkerCapacity,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionFence {
    pub session_id: Uuid,
    pub session_epoch: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkloadFence {
    pub workload_id: Uuid,
    pub assignment_id: Uuid,
    pub generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLimits {
    pub max_control_frame_bytes: u32,
    pub max_in_flight_commands: u16,
    pub max_data_lanes: u16,
    pub max_streams_per_lane: u16,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerWelcome {
    pub protocol_revision: u16,
    pub session: SessionFence,
    pub heartbeat_interval_ms: u64,
    pub lease_timeout_ms: u64,
    pub limits: SessionLimits,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlEnvelope {
    pub protocol_revision: u16,
    pub message_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<Uuid>,
    pub session_epoch: u64,
    #[serde(flatten)]
    pub body: ControlMessage,
}

impl ControlEnvelope {
    pub fn new(session_epoch: u64, body: ControlMessage) -> Self {
        Self {
            protocol_revision: PROTOCOL_REVISION,
            message_id: Uuid::new_v4(),
            reply_to: None,
            session_epoch,
            body,
        }
    }

    pub fn reply(message: &Self, body: ControlMessage) -> Self {
        Self {
            protocol_revision: PROTOCOL_REVISION,
            message_id: Uuid::new_v4(),
            reply_to: Some(message.message_id),
            session_epoch: message.session_epoch,
            body,
        }
    }

    pub fn workload_fence(&self) -> Option<WorkloadFence> {
        self.body.workload_fence()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "body", rename_all = "camelCase")]
pub enum ControlMessage {
    Heartbeat(Heartbeat),
    InventoryRequest(InventoryRequest),
    EnsureWorkload(EnsureWorkload),
    EnsureAbsent(EnsureAbsent),
    WriteFlag(WriteFlag),
    CommandAck(CommandAck),
    CommandResult(CommandResult),
    InventoryPage(InventoryPage),
    WorkloadStatus(WorkloadStatus),
}

impl ControlMessage {
    pub fn workload_fence(&self) -> Option<WorkloadFence> {
        match self {
            Self::EnsureWorkload(message) => Some(message.fence),
            Self::EnsureAbsent(message) => Some(message.fence),
            Self::WriteFlag(message) => Some(message.fence),
            Self::WorkloadStatus(message) => Some(message.fence),
            _ => None,
        }
    }

    pub fn command_id(&self) -> Option<Uuid> {
        match self {
            Self::InventoryRequest(message) => Some(message.command_id),
            Self::EnsureWorkload(message) => Some(message.command_id),
            Self::EnsureAbsent(message) => Some(message.command_id),
            Self::WriteFlag(message) => Some(message.command_id),
            Self::CommandAck(message) => Some(message.command_id),
            Self::CommandResult(message) => Some(message.command_id),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceUsage {
    pub reserved_cpu_millis: u64,
    pub reserved_memory_bytes: u64,
    pub running_workloads: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Heartbeat {
    pub sent_at_unix_ms: i64,
    pub usage: ResourceUsage,
    pub runtime_healthy: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_error: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryRequest {
    pub command_id: Uuid,
    pub snapshot_id: Uuid,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnsureWorkload {
    pub command_id: Uuid,
    #[serde(flatten)]
    pub fence: WorkloadFence,
    pub spec_hash: String,
    pub timeout_ms: u64,
    pub spec: ValidatedWorkloadSpec,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnsureAbsent {
    pub command_id: Uuid,
    #[serde(flatten)]
    pub fence: WorkloadFence,
    pub spec_hash: String,
    pub timeout_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteFlag {
    pub command_id: Uuid,
    #[serde(flatten)]
    pub fence: WorkloadFence,
    pub flag_sequence: u64,
    pub target: FlagTarget,
    pub value: String,
    pub timeout_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AckDisposition {
    Accepted,
    Busy,
    Stale,
    Rejected,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandAck {
    pub command_id: Uuid,
    pub disposition: AckDisposition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CommandErrorCode {
    Unsupported,
    InvalidSpec,
    StaleSession,
    StaleAssignment,
    StaleGeneration,
    StaleFlagSequence,
    SpecConflict,
    RuntimeUnavailable,
    Timeout,
    NotFound,
    PartialFailure,
    Internal,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandError {
    pub code: CommandErrorCode,
    pub message: String,
    #[serde(default)]
    pub failed_replicas: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandResult {
    pub command_id: Uuid,
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<CommandError>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ObservedWorkloadState {
    Unknown,
    Reconciling,
    Ready,
    Degraded,
    Failed,
    Absent,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplicaStatus {
    pub service: String,
    pub replica: u16,
    pub ready: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkloadStatus {
    #[serde(flatten)]
    pub fence: WorkloadFence,
    pub spec_hash: String,
    pub state: ObservedWorkloadState,
    pub replicas: Vec<ReplicaStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryItem {
    #[serde(flatten)]
    pub fence: WorkloadFence,
    pub spec_hash: String,
    pub state: ObservedWorkloadState,
    pub replicas: Vec<ReplicaStatus>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryPage {
    pub snapshot_id: Uuid,
    pub page: u32,
    pub final_page: bool,
    pub items: Vec<InventoryItem>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_workload_fence() {
        let fence = WorkloadFence {
            workload_id: Uuid::new_v4(),
            assignment_id: Uuid::new_v4(),
            generation: 7,
        };
        let envelope = ControlEnvelope::new(
            2,
            ControlMessage::EnsureAbsent(EnsureAbsent {
                command_id: Uuid::new_v4(),
                fence,
                spec_hash: "0".repeat(64),
                timeout_ms: 10_000,
            }),
        );
        assert_eq!(envelope.workload_fence(), Some(fence));
    }

    #[test]
    fn envelope_uses_tagged_camel_case_message() {
        let envelope = ControlEnvelope::new(
            1,
            ControlMessage::InventoryRequest(InventoryRequest {
                command_id: Uuid::new_v4(),
                snapshot_id: Uuid::new_v4(),
            }),
        );
        let json = serde_json::to_value(envelope).unwrap();
        assert_eq!(json["type"], "inventoryRequest");
        assert!(json.get("body").is_some());
    }

    #[test]
    fn workload_replica_capability_is_camel_case_and_missing_is_fail_closed() {
        let capabilities = WorkerCapabilities {
            max_workload_replicas: 37,
            ..WorkerCapabilities::default()
        };
        let mut json = serde_json::to_value(&capabilities).unwrap();
        assert_eq!(json["maxWorkloadReplicas"], 37);

        json.as_object_mut().unwrap().remove("maxWorkloadReplicas");
        let decoded: WorkerCapabilities = serde_json::from_value(json).unwrap();
        assert_eq!(decoded.max_workload_replicas, 0);
    }
}
