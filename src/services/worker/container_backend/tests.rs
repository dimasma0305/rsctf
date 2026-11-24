use super::*;

fn aggregate_workload(flag_target: bool) -> ValidatedWorkloadSpec {
    let services = vec![
        ServiceSpec {
            name: "app".into(),
            image: ImageIdentity::RegistryDigest {
                repository: "registry.example/ctf/app".into(),
                digest: format!("sha256:{}", "a".repeat(64)),
            },
            resources: ResourceLimits {
                cpu_millis: 500,
                memory_bytes: 128 * 1024 * 1024,
                storage_bytes: rsctf_worker_protocol::DEFAULT_STORAGE_BYTES,
            },
            replicas: 3,
            stateless: true,
            environment: BTreeMap::new(),
            ports: vec![ServicePort {
                name: "http".into(),
                container_port: 8080,
                protocol: PortProtocol::Tcp,
            }],
        },
        ServiceSpec {
            name: "cache".into(),
            image: ImageIdentity::RegistryDigest {
                repository: "registry.example/ctf/cache".into(),
                digest: format!("sha256:{}", "b".repeat(64)),
            },
            resources: ResourceLimits {
                cpu_millis: 250,
                memory_bytes: 64 * 1024 * 1024,
                storage_bytes: rsctf_worker_protocol::DEFAULT_STORAGE_BYTES,
            },
            replicas: 1,
            stateless: false,
            environment: BTreeMap::new(),
            ports: vec![ServicePort {
                name: "cache".into(),
                container_port: 6379,
                protocol: PortProtocol::Tcp,
            }],
        },
    ];
    ValidatedWorkloadSpec::try_from(WorkloadSpec {
        game_kind: GameKind::Jeopardy,
        platform: Platform {
            operating_system: OperatingSystem::Linux,
            architecture: "amd64".into(),
            windows_build: None,
        },
        services,
        primary_endpoint: EndpointRef {
            service: "app".into(),
            port: "http".into(),
        },
        team_data_mount: None,
        flag_target: flag_target.then(|| FlagTarget {
            service: "app".into(),
            path: "/flag".into(),
        }),
    })
    .unwrap()
}

fn all_stateless(workload: ValidatedWorkloadSpec) -> ValidatedWorkloadSpec {
    let mut workload = workload.into_inner();
    for service in &mut workload.services {
        service.stateless = true;
    }
    ValidatedWorkloadSpec::try_from(workload).unwrap()
}

#[test]
fn handles_are_strict_and_round_trip() {
    let expected = WorkerHandle {
        workload_id: Uuid::new_v4(),
        assignment_id: Uuid::new_v4(),
        generation: 7,
    };
    assert_eq!(parse_worker_handle(&expected.encode()), Some(expected));
    assert!(parse_worker_handle("rsctf-worker:not-a-uuid:x:1").is_none());
    assert!(parse_worker_handle(&format!("{}:extra", expected.encode())).is_none());
}

#[test]
fn pure_remote_backend_rejects_competitive_game_kinds() {
    assert!(require_jeopardy_game_kind(GameKind::AttackDefense).is_err());
    assert!(require_jeopardy_game_kind(GameKind::KingOfTheHill).is_err());
    assert!(require_jeopardy_game_kind(GameKind::Jeopardy).is_ok());
}

#[test]
fn worker_workloads_fail_closed_without_platform_proxy_admission() {
    assert!(require_proxy_only_workload(true).is_ok());
    assert!(require_proxy_only_workload(false).is_err());
}

#[test]
fn worker_local_images_pin_placement() {
    let worker = Uuid::new_v4();
    let reference = format!("worker://{worker}/sha256:{}", "a".repeat(64));
    let (image, exact) = image_identity(&reference).unwrap();
    assert!(matches!(image, ImageIdentity::WorkerLocal { .. }));
    assert_eq!(exact, Some(worker));
}

#[test]
fn aggregate_capacity_counts_replica_resources_and_one_workload_slot() {
    let reservation = aggregate_reservation(&aggregate_workload(true)).unwrap();
    assert_eq!(reservation.cpu_millis, 1_750);
    assert_eq!(reservation.memory_bytes, 448 * 1024 * 1024);
    assert_eq!(reservation.slots, 1);
}

#[test]
fn runtime_flags_require_and_use_one_declared_target() {
    let missing = crate::services::challenge_workloads::with_flag(
        aggregate_workload(false),
        Some("flag".into()),
    );
    assert!(missing.is_err());

    let injected = crate::services::challenge_workloads::with_flag(
        aggregate_workload(true),
        Some("flag".into()),
    )
    .unwrap();
    let app = injected
        .services
        .iter()
        .find(|service| service.name == "app")
        .unwrap();
    let cache = injected
        .services
        .iter()
        .find(|service| service.name == "cache")
        .unwrap();
    assert_eq!(
        app.environment.get("RSCTF_FLAG").map(String::as_str),
        Some("flag")
    );
    assert!(!cache.environment.contains_key("RSCTF_FLAG"));
}

#[test]
fn rollout_preserves_request_scoped_values_and_applies_replica_changes() {
    let current = crate::services::challenge_workloads::with_flag(
        crate::services::challenge_workloads::with_environment(
            aggregate_workload(true),
            "RSCTF_TEAM_ID",
            "42",
        )
        .unwrap(),
        Some("RSCTF{current}".into()),
    )
    .unwrap();
    let mut next = aggregate_workload(true).into_inner();
    next.services[0].replicas = 2;
    next.services[0]
        .environment
        .insert("CONFIGURED".into(), "new".into());
    let next = ValidatedWorkloadSpec::try_from(next).unwrap();

    let rolled = specialize_rollout(next, &current).unwrap();
    assert!(rolled.services.iter().all(|service| {
        service.environment.get("RSCTF_TEAM_ID").map(String::as_str) == Some("42")
    }));
    let app = rolled
        .services
        .iter()
        .find(|service| service.name == "app")
        .unwrap();
    assert_eq!(
        app.environment.get("RSCTF_FLAG").map(String::as_str),
        Some("RSCTF{current}")
    );
    assert_eq!(
        app.environment.get("CONFIGURED").map(String::as_str),
        Some("new")
    );
    assert_eq!(app.replicas, 2);
}

#[test]
fn rollout_transition_rejects_stateful_current_or_target() {
    let stateful = aggregate_workload(true);
    let stateless = all_stateless(aggregate_workload(true));
    assert!(ensure_stateless_rollout_transition(&stateless, &stateless).is_ok());
    assert!(ensure_stateless_rollout_transition(&stateful, &stateless).is_err());
    assert!(ensure_stateless_rollout_transition(&stateless, &stateful).is_err());
}

#[test]
fn readiness_polling_backs_off_to_one_second_with_stable_jitter() {
    let mut delay = READY_POLL_INITIAL;
    let mut sequence = Vec::new();
    for _ in 0..6 {
        sequence.push(delay);
        delay = next_ready_poll(delay);
    }
    assert_eq!(
        sequence,
        vec![
            Duration::from_millis(200),
            Duration::from_millis(300),
            Duration::from_millis(450),
            Duration::from_millis(675),
            Duration::from_secs(1),
            Duration::from_secs(1),
        ]
    );

    let workload = Uuid::new_v4();
    let first = jittered_ready_poll(Duration::from_secs(1), workload, 3);
    assert_eq!(
        first,
        jittered_ready_poll(Duration::from_secs(1), workload, 3)
    );
    assert!((Duration::from_millis(800)..=Duration::from_millis(1_200)).contains(&first));
}
