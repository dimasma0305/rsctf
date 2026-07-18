use super::*;

fn workload_spec(replicas: u16) -> serde_json::Value {
    use rsctf_worker_protocol::{
        EndpointRef, GameKind, ImageIdentity, OperatingSystem, Platform, PortProtocol,
        ResourceLimits, ServicePort, ServiceSpec, ValidatedWorkloadSpec, WorkloadSpec,
    };

    let spec = WorkloadSpec {
        game_kind: GameKind::Jeopardy,
        platform: Platform {
            operating_system: OperatingSystem::Linux,
            architecture: "amd64".to_owned(),
            windows_build: None,
        },
        services: vec![ServiceSpec {
            name: "challenge".to_owned(),
            image: ImageIdentity::RegistryDigest {
                repository: "registry.example/challenge".to_owned(),
                digest: format!("sha256:{}", "a".repeat(64)),
            },
            resources: ResourceLimits {
                cpu_millis: 100,
                memory_bytes: 1_048_576,
            },
            replicas,
            stateless: replicas > 1,
            environment: Default::default(),
            ports: vec![ServicePort {
                name: "service".to_owned(),
                container_port: 31337,
                protocol: PortProtocol::Tcp,
            }],
        }],
        primary_endpoint: EndpointRef {
            service: "challenge".to_owned(),
            port: "service".to_owned(),
        },
        flag_target: None,
    };
    serde_json::to_value(ValidatedWorkloadSpec::try_from(spec).unwrap()).unwrap()
}

#[test]
fn resource_reservations_reject_negative_values() {
    assert!(ResourceReservation {
        cpu_millis: -1,
        memory_bytes: 0,
        slots: 0,
    }
    .validate()
    .is_err());
    assert!(ResourceReservation {
        cpu_millis: 0,
        memory_bytes: -1,
        slots: 0,
    }
    .validate()
    .is_err());
    assert!(ResourceReservation {
        cpu_millis: 0,
        memory_bytes: 0,
        slots: -1,
    }
    .validate()
    .is_err());
}

#[test]
fn workload_definitions_require_one_slot_and_derive_replica_count() {
    let definition = WorkloadDefinition {
        spec: serde_json::json!([]),
        spec_hash_sha256: [0; 32],
        required_os: PlatformOs::Linux,
        required_architecture: "amd64".to_owned(),
        required_runtime: "docker".to_owned(),
        reservation: ResourceReservation {
            cpu_millis: 100,
            memory_bytes: 1024,
            slots: 0,
        },
    };
    assert!(definition.validate().is_err());

    let valid = WorkloadDefinition {
        spec: workload_spec(3),
        reservation: ResourceReservation {
            slots: 1,
            ..definition.reservation
        },
        ..definition
    };
    assert_eq!(valid.validate().unwrap(), 3);

    let replica_slots = WorkloadDefinition {
        reservation: ResourceReservation {
            slots: 3,
            ..valid.reservation
        },
        ..valid
    };
    assert!(replica_slots.validate().is_err());
}

#[test]
fn workload_definitions_reject_invalid_or_mismatched_architectures() {
    let definition = WorkloadDefinition {
        spec: workload_spec(1),
        spec_hash_sha256: [0; 32],
        required_os: PlatformOs::Linux,
        required_architecture: "amd64/variant".to_owned(),
        required_runtime: "docker".to_owned(),
        reservation: ResourceReservation {
            cpu_millis: 100,
            memory_bytes: 1024,
            slots: 1,
        },
    };
    assert!(definition.validate().is_err());

    let mismatch = WorkloadDefinition {
        required_architecture: "arm64".to_owned(),
        ..definition
    };
    assert!(mismatch.validate().is_err());
}

#[test]
fn stored_replica_count_rejects_corrupt_or_unbounded_dimensions() {
    assert_eq!(
        super::workloads::stored_replica_count(&workload_spec(3)).unwrap(),
        3
    );

    let mut invalid_type = workload_spec(1);
    invalid_type["services"][0]["replicas"] = serde_json::json!("1");
    assert!(super::workloads::stored_replica_count(&invalid_type).is_err());

    let mut zero = workload_spec(1);
    zero["services"][0]["replicas"] = serde_json::json!(0);
    assert!(super::workloads::stored_replica_count(&zero).is_err());

    let mut oversized = workload_spec(1);
    oversized["services"][0]["replicas"] = serde_json::json!(513);
    assert!(super::workloads::stored_replica_count(&oversized).is_err());
}

#[test]
fn worker_and_workload_states_have_stable_database_names() {
    assert_eq!(WorkerAdministrativeState::Draining.as_str(), "Draining");
    assert_eq!(PlatformOs::Windows.as_str(), "windows");
    assert_eq!(WorkloadDesiredState::Absent.as_str(), "Absent");
    assert_eq!(WorkloadObservedState::Reconciling.as_str(), "Reconciling");
    assert!(WorkerAdministrativeState::parse("retired").is_err());
    assert!(WorkloadObservedState::parse("ready").is_err());
}
