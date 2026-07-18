use std::collections::HashSet;

use rsctf_worker_protocol::{
    is_valid_architecture, DataStreamRequest, InventoryItem, ObservedWorkloadState,
    ValidatedWorkloadSpec, WorkerCapabilities, WorkerHello, MAX_AGENT_VERSION_BYTES,
    MAX_RUNTIME_VERSION_BYTES, MAX_WINDOWS_BUILD_BYTES, MAX_WORKLOAD_REPLICAS,
};

use crate::services::worker::{WorkerError, WorkerResult};
use crate::services::worker_store::WorkloadObservedState;

const MAX_LABELS: usize = 64;
const MAX_LABEL_KEY_BYTES: usize = 63;
const MAX_LABEL_VALUE_BYTES: usize = 256;

pub(super) fn validate_hello_metadata(hello: &WorkerHello) -> WorkerResult<()> {
    validate_text(
        &hello.agent_version,
        MAX_AGENT_VERSION_BYTES,
        "invalid worker agent version",
    )?;
    if !is_valid_architecture(&hello.platform.architecture) {
        return Err(WorkerError::Protocol("invalid worker architecture"));
    }
    if let Some(build) = hello.platform.windows_build.as_deref() {
        validate_text(build, MAX_WINDOWS_BUILD_BYTES, "invalid Windows build")?;
    }
    if let Some(version) = hello.runtime.version.as_deref() {
        validate_text(
            version,
            MAX_RUNTIME_VERSION_BYTES,
            "invalid worker runtime version",
        )?;
    }
    Ok(())
}

fn validate_text(value: &str, maximum: usize, error: &'static str) -> WorkerResult<()> {
    if value.trim().is_empty()
        || value.trim() != value
        || value.len() > maximum
        || value.chars().any(char::is_control)
    {
        return Err(WorkerError::Protocol(error));
    }
    Ok(())
}

pub(super) fn validate_v1_capabilities(capabilities: &WorkerCapabilities) -> WorkerResult<()> {
    if !capabilities.ensure_workload
        || !capabilities.write_flag
        || !capabilities.tcp_proxy
        || !capabilities.inventory
        || capabilities.max_data_lanes == 0
        || capabilities.max_workload_replicas == 0
        || usize::from(capabilities.max_workload_replicas) > MAX_WORKLOAD_REPLICAS
    {
        return Err(WorkerError::Protocol(
            "worker is missing required revision 1 capabilities",
        ));
    }
    Ok(())
}

pub(super) fn validate_labels(hello: &WorkerHello) -> WorkerResult<()> {
    if hello.labels.len() > MAX_LABELS {
        return Err(WorkerError::Protocol("worker has too many labels"));
    }
    if hello.labels.iter().any(|(key, value)| {
        key.is_empty()
            || key.len() > MAX_LABEL_KEY_BYTES
            || value.len() > MAX_LABEL_VALUE_BYTES
            || !key.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/')
            })
    }) {
        return Err(WorkerError::Protocol("worker has an invalid label"));
    }
    Ok(())
}

pub(super) fn decode_hash(value: &str) -> WorkerResult<[u8; 32]> {
    let mut output = [0_u8; 32];
    hex::decode_to_slice(value, &mut output)
        .map_err(|_| WorkerError::Protocol("invalid workload specification hash"))?;
    Ok(output)
}

pub(super) fn observed_state(state: ObservedWorkloadState) -> WorkloadObservedState {
    match state {
        ObservedWorkloadState::Unknown => WorkloadObservedState::Unknown,
        ObservedWorkloadState::Reconciling => WorkloadObservedState::Reconciling,
        ObservedWorkloadState::Ready => WorkloadObservedState::Ready,
        ObservedWorkloadState::Degraded => WorkloadObservedState::Degraded,
        ObservedWorkloadState::Failed => WorkloadObservedState::Failed,
        ObservedWorkloadState::Absent => WorkloadObservedState::Absent,
    }
}

pub(super) fn stream_exists(spec: &ValidatedWorkloadSpec, request: &DataStreamRequest) -> bool {
    match request {
        DataStreamRequest::TcpProxy(request) => spec.services.iter().any(|service| {
            service.name == request.service
                && request
                    .replica
                    .is_none_or(|replica| replica < service.replicas)
                && service.ports.iter().any(|port| port.name == request.port)
        }),
        DataStreamRequest::InteractiveExec(request) => spec
            .services
            .iter()
            .any(|service| service.name == request.service && request.replica < service.replicas),
    }
}

pub(super) fn validate_replica_observation(
    spec: &ValidatedWorkloadSpec,
    item: &InventoryItem,
) -> WorkerResult<()> {
    if item.state == ObservedWorkloadState::Absent {
        return if item.replicas.is_empty() {
            Ok(())
        } else {
            Err(WorkerError::Protocol(
                "absent workload status contains replicas",
            ))
        };
    }

    let expected_count = spec
        .services
        .iter()
        .map(|service| usize::from(service.replicas))
        .sum::<usize>();
    let mut seen = HashSet::with_capacity(item.replicas.len());
    for replica in &item.replicas {
        let Some(service) = spec
            .services
            .iter()
            .find(|service| service.name == replica.service)
        else {
            return Err(WorkerError::Protocol(
                "workload status contains an unknown service",
            ));
        };
        if replica.replica >= service.replicas
            || !seen.insert((replica.service.as_str(), replica.replica))
        {
            return Err(WorkerError::Protocol(
                "workload status contains an invalid or duplicate replica",
            ));
        }
    }
    if item.state == ObservedWorkloadState::Ready
        && (seen.len() != expected_count || item.replicas.iter().any(|replica| !replica.ready))
    {
        return Err(WorkerError::Protocol(
            "ready workload status does not contain every ready replica",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use rsctf_worker_protocol::{
        OperatingSystem, Platform, RuntimeDescriptor, RuntimeEndpointKind, RuntimeKind,
        WorkerCapacity, MAX_ARCHITECTURE_BYTES,
    };
    use uuid::Uuid;

    use super::*;

    fn hello() -> WorkerHello {
        WorkerHello {
            protocol_revision: 1,
            worker_id: Uuid::new_v4(),
            boot_id: Uuid::new_v4(),
            agent_version: "1.0.0".into(),
            platform: Platform {
                operating_system: OperatingSystem::Linux,
                architecture: "amd64".into(),
                windows_build: None,
            },
            runtime: RuntimeDescriptor {
                kind: RuntimeKind::Docker,
                version: Some("27.1.1".into()),
                endpoint_kind: Some(RuntimeEndpointKind::UnixSocket),
            },
            capabilities: WorkerCapabilities::default(),
            capacity: WorkerCapacity {
                cpu_millis: 1,
                memory_bytes: 1,
                slots: 1,
            },
            labels: BTreeMap::new(),
        }
    }

    #[test]
    fn hello_metadata_accepts_small_printable_runtime_identity() {
        assert!(validate_hello_metadata(&hello()).is_ok());
    }

    #[test]
    fn hello_metadata_rejects_oversized_or_control_text() {
        let mut value = hello();
        value.platform.architecture = "a".repeat(MAX_ARCHITECTURE_BYTES + 1);
        assert!(validate_hello_metadata(&value).is_err());

        let mut value = hello();
        value.platform.architecture = "amd64\0forged".into();
        assert!(validate_hello_metadata(&value).is_err());

        let mut value = hello();
        value.runtime.version = Some("v\nforged".into());
        assert!(validate_hello_metadata(&value).is_err());

        let mut value = hello();
        value.agent_version = "v".repeat(MAX_AGENT_VERSION_BYTES + 1);
        assert!(validate_hello_metadata(&value).is_err());
    }
}
