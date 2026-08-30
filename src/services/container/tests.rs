use std::collections::HashMap;

use bollard::models::{
    ContainerConfig, ContainerInspectResponse, ContainerState, ContainerStateStatusEnum,
    HostConfig, ImageInspect, Ipam, IpamConfig, Network, SystemInfo,
};

use super::docker::{
    advertised_endpoint_ip, docker_liveness, docker_network_mode, failed_start_action,
    image_requests_restricted_profile, launch_spec_fingerprint, launch_spec_matches,
    parse_proxy_bind, published_bind_ip, restricted_profile_matches, restricted_tmpfs_mounts,
    stamp_restricted_profile, stamp_storage_quota_policy, storage_quota_policy_matches,
    validate_docker_container_spec, verify_container_scope, writable_layer_quota_supported,
    writable_layer_storage_opt, writable_layer_storage_option, FailedStartAction,
    LAUNCH_SPEC_LABEL, RESTRICTED_IMAGE_PROFILE, RESTRICTED_IMAGE_PROFILE_LABEL,
    RESTRICTED_TMPFS_OPTIONS, RESTRICTED_TMPFS_PATH,
};
use super::{
    append_snapshot_chunk, bounded_log_config, bridge_network_matches, container_name,
    docker_workload_scope, game_kind_for_challenge, labels_match_scope, managed_container_filters,
    network_scope_matches, scoped_managed_labels, scoped_operation_id, should_use_platform_proxy,
    validate_container_spec, ContainerLiveness, ContainerManager, ContainerResourceLimits,
    ContainerSpec, DockerContainerManager, DEFAULT_CONTAINER_STORAGE_MB,
};

#[test]
fn restricted_runtime_profile_requires_the_exact_organizer_image_label() {
    let image = |value: Option<&str>| ImageInspect {
        config: Some(ContainerConfig {
            labels: value.map(|value| {
                HashMap::from([(
                    RESTRICTED_IMAGE_PROFILE_LABEL.to_string(),
                    value.to_string(),
                )])
            }),
            ..Default::default()
        }),
        ..Default::default()
    };

    assert!(image_requests_restricted_profile(&image(Some(
        RESTRICTED_IMAGE_PROFILE
    ))));
    assert!(!image_requests_restricted_profile(&image(Some(
        "restricted"
    ))));
    assert!(!image_requests_restricted_profile(&image(None)));
    assert!(!image_requests_restricted_profile(&ImageInspect::default()));
}

#[test]
fn restricted_runtime_label_stamping_is_reversible() {
    let mut labels = HashMap::from([("existing".to_string(), "kept".to_string())]);
    stamp_restricted_profile(&mut labels, true);
    assert_eq!(
        labels
            .get(RESTRICTED_IMAGE_PROFILE_LABEL)
            .map(String::as_str),
        Some(RESTRICTED_IMAGE_PROFILE)
    );
    stamp_restricted_profile(&mut labels, false);
    assert!(!labels.contains_key(RESTRICTED_IMAGE_PROFILE_LABEL));
    assert_eq!(labels.get("existing").map(String::as_str), Some("kept"));
}

#[test]
fn restricted_retry_refuses_to_adopt_a_container_without_the_runtime_guards() {
    let guarded = ContainerInspectResponse {
        host_config: Some(HostConfig {
            cap_drop: Some(vec!["ALL".to_string()]),
            readonly_rootfs: Some(true),
            security_opt: Some(vec!["no-new-privileges:true".to_string()]),
            tmpfs: Some(restricted_tmpfs_mounts()),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(restricted_profile_matches(&guarded, true));
    assert!(restricted_profile_matches(&guarded, false));
    assert!(!restricted_profile_matches(
        &ContainerInspectResponse::default(),
        true
    ));

    let mut weakened = guarded.clone();
    weakened.host_config.as_mut().unwrap().readonly_rootfs = Some(false);
    assert!(!restricted_profile_matches(&weakened, true));

    let mut weakened = guarded.clone();
    weakened.host_config.as_mut().unwrap().cap_drop = Some(vec!["NET_RAW".to_string()]);
    assert!(!restricted_profile_matches(&weakened, true));

    let mut weakened = guarded.clone();
    weakened.host_config.as_mut().unwrap().security_opt = Some(vec!["seccomp=default".to_string()]);
    assert!(!restricted_profile_matches(&weakened, true));

    for options in [
        "rw",
        "rw,nosuid,nodev,noexec,size=268435456",
        "rw,nosuid,nodev,noexec,size=536870912,mode=1777",
    ] {
        let mut weakened = guarded.clone();
        weakened.host_config.as_mut().unwrap().tmpfs = Some(HashMap::from([(
            RESTRICTED_TMPFS_PATH.to_string(),
            options.to_string(),
        )]));
        assert!(!restricted_profile_matches(&weakened, true));
    }

    let mut weakened = guarded.clone();
    weakened.host_config.as_mut().unwrap().tmpfs = Some(HashMap::from([
        (
            RESTRICTED_TMPFS_PATH.to_string(),
            RESTRICTED_TMPFS_OPTIONS.to_string(),
        ),
        ("/run".to_string(), "rw".to_string()),
    ]));
    assert!(!restricted_profile_matches(&weakened, true));
}

fn inspected_network(subnets: &[&str], internal: bool) -> Network {
    Network {
        driver: Some("bridge".to_string()),
        internal: Some(internal),
        ipam: Some(Ipam {
            config: Some(
                subnets
                    .iter()
                    .map(|subnet| IpamConfig {
                        subnet: Some((*subnet).to_string()),
                        ..Default::default()
                    })
                    .collect(),
            ),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn inspected_container_state(status: ContainerStateStatusEnum) -> ContainerInspectResponse {
    ContainerInspectResponse {
        id: Some("canonical-container-id".to_string()),
        state: Some(ContainerState {
            status: Some(status),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn fingerprint_spec() -> ContainerSpec {
    ContainerSpec {
        game_kind: rsctf_worker_protocol::GameKind::KingOfTheHill,
        image: format!("registry.example/hill@sha256:{}", "a".repeat(64)),
        memory_limit: 256,
        cpu_count: 1,
        storage_limit: DEFAULT_CONTAINER_STORAGE_MB,
        expose_port: 8080,
        publish_port: true,
        proxy_only: false,
        env: vec![("TEAM".to_string(), "7".to_string())],
        flag: Some("flag-secret".to_string()),
        ad_network: Some("rsctf-ad".to_string()),
        allow_egress: false,
        control_plane_callback_ports: Vec::new(),
        network_mode: crate::utils::enums::NetworkMode::Open,
        operation_id: Some("cycle:9".to_string()),
    }
}

#[test]
fn existing_ad_network_must_match_exact_ipv4_subnet() {
    let expected = inspected_network(&["10.13.40.0/24"], true);
    assert!(bridge_network_matches(
        &expected,
        Some("10.13.40.0/24"),
        true,
        false,
    ));

    let stale = inspected_network(&["10.13.41.0/24"], true);
    assert!(!bridge_network_matches(
        &stale,
        Some("10.13.40.0/24"),
        true,
        false,
    ));

    let ambiguous = inspected_network(&["10.13.40.0/24", "10.13.41.0/24"], true);
    assert!(!bridge_network_matches(
        &ambiguous,
        Some("10.13.40.0/24"),
        true,
        false,
    ));
}

#[test]
fn isolated_bridge_requires_inter_container_communication_to_be_disabled() {
    let mut isolated = inspected_network(&[], true);
    assert!(!bridge_network_matches(&isolated, None, true, true));
    isolated.options = Some(HashMap::from([(
        "com.docker.network.bridge.enable_icc".to_string(),
        "false".to_string(),
    )]));
    assert!(bridge_network_matches(&isolated, None, true, true));
}

#[test]
fn docker_storage_quota_support_uses_an_unbounded_fallback() {
    let xfs = SystemInfo {
        driver: Some("overlay2".to_string()),
        driver_status: Some(vec![vec![
            "Backing Filesystem".to_string(),
            "xfs".to_string(),
        ]]),
        ..Default::default()
    };
    assert!(writable_layer_quota_supported(&xfs));

    let mut ext4 = xfs;
    ext4.driver_status = Some(vec![vec![
        "Backing Filesystem".to_string(),
        "extfs".to_string(),
    ]]);
    assert!(!writable_layer_quota_supported(&ext4));
    assert_eq!(
        writable_layer_storage_opt(768)
            .get("size")
            .map(String::as_str),
        Some("768M")
    );

    assert_eq!(writable_layer_storage_option(false, 768), None);
    assert_eq!(
        writable_layer_storage_option(
            writable_layer_quota_supported(&SystemInfo {
                driver: Some("btrfs".to_string()),
                ..Default::default()
            }),
            768,
        )
        .and_then(|options| options.get("size").cloned()),
        Some("768M".to_string())
    );
}

#[test]
fn adopted_container_must_match_the_storage_quota_policy() {
    let mut labels = HashMap::new();
    stamp_storage_quota_policy(&mut labels, false);
    let mut inspected = ContainerInspectResponse {
        config: Some(ContainerConfig {
            labels: Some(labels),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(storage_quota_policy_matches(&inspected, false));
    assert!(!storage_quota_policy_matches(&inspected, true));

    let labels = inspected
        .config
        .as_mut()
        .and_then(|config| config.labels.as_mut())
        .expect("test labels");
    stamp_storage_quota_policy(labels, true);
    assert!(storage_quota_policy_matches(&inspected, true));
    assert!(!storage_quota_policy_matches(&inspected, false));

    assert!(storage_quota_policy_matches(
        &ContainerInspectResponse::default(),
        true
    ));
    assert!(!storage_quota_policy_matches(
        &ContainerInspectResponse::default(),
        false
    ));
}

#[test]
fn snapshot_buffer_rejects_the_chunk_that_crosses_its_limit() {
    let mut out = Vec::new();
    append_snapshot_chunk(&mut out, b"1234", 6).unwrap();
    let error = append_snapshot_chunk(&mut out, b"567", 6).unwrap_err();
    assert!(matches!(
        error,
        crate::utils::error::AppError::PayloadTooLarge(_)
    ));
    assert_eq!(out, b"1234", "the rejected chunk was partially appended");
}

#[test]
fn docker_workloads_are_scoped_without_exposing_the_identity() {
    let first = docker_workload_scope(Some("event-a"), Some("ignored-secret"));
    let replica = docker_workload_scope(Some("event-a"), Some("rotated-secret"));
    let second = docker_workload_scope(Some("event-b"), Some("ignored-secret"));
    assert_eq!(first, replica);
    assert_ne!(first, second);
    assert_eq!(first.len(), 32);
    assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));

    let labels = scoped_managed_labels(&first);
    assert!(labels_match_scope(Some(&labels), &first));
    assert!(!labels_match_scope(Some(&labels), &second));
    assert_ne!(
        labels.get("rsctf.managed").map(String::as_str),
        Some("true")
    );
    assert_eq!(
        managed_container_filters(&first).get("label"),
        Some(&vec![
            format!("rsctf.managed={first}"),
            format!("rsctf.scope={first}"),
        ])
    );
}

#[test]
fn inspected_container_must_belong_to_the_current_installation() {
    let owned = ContainerInspectResponse {
        config: Some(ContainerConfig {
            labels: Some(scoped_managed_labels("installation-a")),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(verify_container_scope(&owned, "installation-a").is_ok());
    assert!(matches!(
        verify_container_scope(&owned, "installation-b"),
        Err(crate::utils::error::AppError::Conflict(_))
    ));

    let unlabeled = ContainerInspectResponse::default();
    assert!(matches!(
        verify_container_scope(&unlabeled, "installation-a"),
        Err(crate::utils::error::AppError::Conflict(_))
    ));
}

#[test]
fn launch_fingerprint_rejects_stale_runtime_configuration() {
    let spec = fingerprint_spec();
    let expected = launch_spec_fingerprint(&spec);
    assert_eq!(expected.len(), 64);
    assert!(expected.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert!(!expected.contains("flag-secret"));

    let mut retry = spec.clone();
    retry.operation_id = Some("a different lifecycle identity".to_string());
    assert_eq!(launch_spec_fingerprint(&retry), expected);

    let mut changed = spec.clone();
    changed.expose_port += 1;
    assert_ne!(launch_spec_fingerprint(&changed), expected);
    changed = spec.clone();
    changed.memory_limit += 1;
    assert_ne!(launch_spec_fingerprint(&changed), expected);
    changed = spec.clone();
    changed.cpu_count += 1;
    assert_ne!(launch_spec_fingerprint(&changed), expected);
    changed = spec.clone();
    changed.storage_limit += 1;
    assert_ne!(launch_spec_fingerprint(&changed), expected);
    changed = spec.clone();
    changed.image = format!("registry.example/hill@sha256:{}", "b".repeat(64));
    assert_ne!(launch_spec_fingerprint(&changed), expected);
    changed = spec.clone();
    changed.game_kind = rsctf_worker_protocol::GameKind::AttackDefense;
    assert_ne!(launch_spec_fingerprint(&changed), expected);
    changed = spec.clone();
    changed.flag = Some("different-flag".to_string());
    assert_ne!(launch_spec_fingerprint(&changed), expected);
    changed = spec.clone();
    changed.env.push(("EXTRA".to_string(), "1".to_string()));
    assert_ne!(launch_spec_fingerprint(&changed), expected);
    changed = spec.clone();
    changed.ad_network = Some("different-network".to_string());
    assert_ne!(launch_spec_fingerprint(&changed), expected);
    changed = spec.clone();
    changed.allow_egress = true;
    assert_ne!(launch_spec_fingerprint(&changed), expected);
    changed = spec.clone();
    changed.control_plane_callback_ports.push(8080);
    assert_ne!(launch_spec_fingerprint(&changed), expected);
    changed = spec.clone();
    changed.network_mode = crate::utils::enums::NetworkMode::Isolated;
    assert_ne!(launch_spec_fingerprint(&changed), expected);
    changed = spec.clone();
    changed.publish_port = false;
    assert_ne!(launch_spec_fingerprint(&changed), expected);
    changed = spec.clone();
    changed.proxy_only = true;
    assert_ne!(launch_spec_fingerprint(&changed), expected);

    let mut labels = scoped_managed_labels("installation-a");
    labels.insert(LAUNCH_SPEC_LABEL.to_string(), expected.clone());
    let mut inspected = ContainerInspectResponse {
        config: Some(ContainerConfig {
            labels: Some(labels),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(launch_spec_matches(&inspected, &expected));
    inspected
        .config
        .as_mut()
        .and_then(|config| config.labels.as_mut())
        .expect("test labels")
        .remove(LAUNCH_SPEC_LABEL);
    assert!(!launch_spec_matches(&inspected, &expected));
}

#[test]
fn storage_and_network_policy_fence_legacy_launch_identities() {
    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct LegacyDockerLaunchSpec<'a> {
        revision: u8,
        game_kind: rsctf_worker_protocol::GameKind,
        image: &'a str,
        memory_limit: i32,
        cpu_count: i32,
        expose_port: i32,
        env: &'a [(String, String)],
        flag: Option<&'a str>,
        ad_network: Option<&'a str>,
        allow_egress: bool,
    }

    let published = fingerprint_spec();
    let legacy = LegacyDockerLaunchSpec {
        revision: 1,
        game_kind: published.game_kind,
        image: &published.image,
        memory_limit: published.memory_limit,
        cpu_count: published.cpu_count,
        expose_port: published.expose_port,
        env: &published.env,
        flag: published.flag.as_deref(),
        ad_network: published.ad_network.as_deref(),
        allow_egress: published.allow_egress,
    };
    let legacy_fingerprint = crate::utils::codec::sha256_hex(
        &serde_json::to_vec(&legacy).expect("legacy launch spec serializes"),
    );
    assert_ne!(
        launch_spec_fingerprint(&published),
        legacy_fingerprint,
        "legacy workloads do not prove storage or network enforcement"
    );
    assert_eq!(docker_network_mode(&published), None);

    let mut inspector = published.clone();
    inspector.publish_port = false;
    inspector.ad_network = None;
    inspector.network_mode = crate::utils::enums::NetworkMode::Isolated;
    assert_eq!(docker_network_mode(&inspector).as_deref(), Some("none"));
    assert_ne!(
        launch_spec_fingerprint(&inspector),
        legacy_fingerprint,
        "the isolated inspector shape needs its own adoption identity"
    );

    inspector.ad_network = Some("rsctf-internal".to_string());
    assert_eq!(
        docker_network_mode(&inspector),
        None,
        "an explicit internal network remains the caller's isolation boundary"
    );
}

#[test]
fn docker_publication_keeps_proxy_private_and_direct_mode_public() {
    let proxy_ip = parse_proxy_bind("172.31.253.1").unwrap();
    assert!(parse_proxy_bind("127.0.0.1").is_err());
    assert!(parse_proxy_bind("0.0.0.0").is_err());
    assert!(parse_proxy_bind("198.51.100.10").is_err());

    let mut spec = fingerprint_spec();
    spec.game_kind = rsctf_worker_protocol::GameKind::Jeopardy;
    spec.ad_network = None;
    assert_eq!(
        published_bind_ip(&spec, Some(proxy_ip)).unwrap().as_deref(),
        Some("0.0.0.0"),
        "direct Jeopardy remains externally published"
    );

    spec.proxy_only = true;
    assert!(published_bind_ip(&spec, None).is_err());
    assert_eq!(
        published_bind_ip(&spec, Some(proxy_ip)).unwrap().as_deref(),
        Some("172.31.253.1")
    );
    assert_eq!(
        advertised_endpoint_ip(
            &spec,
            Some("public.example"),
            Some("172.31.253.1"),
            Some(proxy_ip),
        )
        .unwrap(),
        "172.31.253.1",
        "the proxy target must never reuse the public entry"
    );
    assert!(advertised_endpoint_ip(
        &spec,
        Some("public.example"),
        Some("0.0.0.0"),
        Some(proxy_ip),
    )
    .is_err());

    let ad = ContainerSpec::ad_service(
        "registry.example/service@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .into(),
        ContainerResourceLimits {
            memory_limit: 256,
            cpu_count: 1,
            storage_limit: DEFAULT_CONTAINER_STORAGE_MB,
        },
        8080,
        7,
        false,
        "flag".into(),
    );
    assert_eq!(published_bind_ip(&ad, Some(proxy_ip)).unwrap(), None);
}

#[test]
fn proxy_only_spec_is_restricted_to_published_jeopardy() {
    let mut spec = fingerprint_spec();
    spec.proxy_only = true;
    assert!(validate_container_spec(&spec).is_err());
    spec.game_kind = rsctf_worker_protocol::GameKind::Jeopardy;
    spec.ad_network = None;
    assert!(validate_container_spec(&spec).is_ok());
    spec.publish_port = false;
    assert!(validate_container_spec(&spec).is_err());
}

#[test]
fn callback_ports_are_bounded_and_require_an_ad_network() {
    let mut spec = fingerprint_spec();
    spec.control_plane_callback_ports = vec![80, 8080];
    assert!(validate_container_spec(&spec).is_ok());

    spec.ad_network = None;
    assert!(validate_container_spec(&spec).is_err());

    spec.ad_network = Some("rsctf-ad".to_string());
    spec.control_plane_callback_ports.push(8443);
    assert!(validate_container_spec(&spec).is_err());

    spec.control_plane_callback_ports = vec![0];
    assert!(validate_container_spec(&spec).is_err());
    spec.control_plane_callback_ports = vec![65_536];
    assert!(validate_container_spec(&spec).is_err());
}

#[test]
fn platform_proxy_selection_never_changes_competitive_direct_modes() {
    for game_kind in [
        rsctf_worker_protocol::GameKind::AttackDefense,
        rsctf_worker_protocol::GameKind::KingOfTheHill,
    ] {
        assert!(!should_use_platform_proxy(game_kind, true, true, false));
    }
    assert!(should_use_platform_proxy(
        rsctf_worker_protocol::GameKind::Jeopardy,
        false,
        true,
        false,
    ));
    assert!(should_use_platform_proxy(
        rsctf_worker_protocol::GameKind::Jeopardy,
        true,
        false,
        false,
    ));
    assert!(!should_use_platform_proxy(
        rsctf_worker_protocol::GameKind::Jeopardy,
        false,
        false,
        false,
    ));
    assert!(!should_use_platform_proxy(
        rsctf_worker_protocol::GameKind::Jeopardy,
        false,
        true,
        true,
    ));
}

#[test]
fn concurrent_adopter_start_never_authorizes_creator_cleanup() {
    let running = inspected_container_state(ContainerStateStatusEnum::RUNNING);
    assert_eq!(
        failed_start_action(true, Some(&running)),
        FailedStartAction::TreatAsStarted
    );

    let created = inspected_container_state(ContainerStateStatusEnum::CREATED);
    assert_eq!(
        failed_start_action(true, Some(&created)),
        FailedStartAction::RetainForRetry,
        "a stable-operation container may be starting on another replica"
    );
    assert_eq!(
        failed_start_action(false, Some(&created)),
        FailedStartAction::RemoveOwned,
        "a unique non-adoptable failed create remains safe to clean up"
    );

    for terminal in [
        ContainerStateStatusEnum::EXITED,
        ContainerStateStatusEnum::DEAD,
    ] {
        let inspected = inspected_container_state(terminal);
        assert_eq!(
            failed_start_action(true, Some(&inspected)),
            FailedStartAction::RemoveOwned,
            "a terminal stable-operation holder must be removed before rotating the key"
        );
    }

    let paused = inspected_container_state(ContainerStateStatusEnum::PAUSED);
    assert_eq!(
        failed_start_action(false, Some(&paused)),
        FailedStartAction::RetainForRetry,
        "ambiguous live states must never be removed after a start error"
    );
    assert_eq!(
        failed_start_action(false, None),
        FailedStartAction::RetainForRetry,
        "failed ownership reinspection must fail closed"
    );
}

#[test]
fn a_declared_network_scope_cannot_be_adopted_by_another_installation() {
    let mut network = inspected_network(&["10.13.40.0/24"], true);
    network.labels = Some(scoped_managed_labels("installation-a"));
    assert!(network_scope_matches(&network, "installation-a"));
    assert!(!network_scope_matches(&network, "installation-b"));

    network.labels = None;
    assert!(
        network_scope_matches(&network, "installation-b"),
        "legacy Compose bridges remain migratable after exact shape validation"
    );
}

#[test]
fn jwt_secret_is_the_replica_safe_scope_fallback() {
    let first = docker_workload_scope(None, Some("deployment-secret-a"));
    assert_eq!(
        first,
        docker_workload_scope(None, Some("deployment-secret-a"))
    );
    assert_ne!(
        first,
        docker_workload_scope(None, Some("deployment-secret-b"))
    );
}

#[test]
fn ad_service_specs_are_internal_only() {
    let spec = ContainerSpec::ad_service(
        "image".into(),
        ContainerResourceLimits {
            memory_limit: 256,
            cpu_count: 1,
            storage_limit: DEFAULT_CONTAINER_STORAGE_MB,
        },
        8080,
        7,
        false,
        "flag".into(),
    );
    assert_eq!(
        spec.game_kind,
        rsctf_worker_protocol::GameKind::AttackDefense
    );
    assert_eq!(
        spec.ad_network,
        Some(crate::services::ad_vpn::services_network())
    );
    assert_eq!(spec.env, vec![("RSCTF_TEAM_ID".into(), "7".into())]);
    assert!(!spec.allow_egress);
}

#[test]
fn challenge_game_kind_preserves_competitive_modes() {
    use crate::utils::enums::ChallengeType;
    use rsctf_worker_protocol::GameKind;

    assert_eq!(
        game_kind_for_challenge(ChallengeType::AttackDefense),
        GameKind::AttackDefense
    );
    assert_eq!(
        game_kind_for_challenge(ChallengeType::KingOfTheHill),
        GameKind::KingOfTheHill
    );
    assert_eq!(
        game_kind_for_challenge(ChallengeType::DynamicContainer),
        GameKind::Jeopardy
    );
}

#[test]
fn docker_competitive_egress_fails_closed_for_both_game_modes() {
    let image = "registry.example/service@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let mut spec = ContainerSpec::ad_service(
        image.into(),
        ContainerResourceLimits {
            memory_limit: 256,
            cpu_count: 1,
            storage_limit: DEFAULT_CONTAINER_STORAGE_MB,
        },
        8080,
        7,
        true,
        "flag".into(),
    );

    // Generic validation remains backend-neutral so Kubernetes can enforce
    // allowed egress with its per-workload NetworkPolicy.
    assert!(validate_container_spec(&spec).is_ok());
    for game_kind in [
        rsctf_worker_protocol::GameKind::AttackDefense,
        rsctf_worker_protocol::GameKind::KingOfTheHill,
    ] {
        spec.game_kind = game_kind;
        let error = validate_docker_container_spec(&spec).unwrap_err();
        assert!(matches!(
            error,
            crate::utils::error::AppError::BadRequest(_)
        ));
        assert_eq!(
            error.to_string(),
            "Docker does not safely support allowEgress=true for A&D or KotH workloads; set allowEgress=false or use the Kubernetes backend with per-workload NetworkPolicy isolation"
        );
    }
}

#[test]
fn docker_competitive_default_deny_egress_remains_supported() {
    let image = "registry.example/service@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let mut spec = ContainerSpec::ad_service(
        image.into(),
        ContainerResourceLimits {
            memory_limit: 256,
            cpu_count: 1,
            storage_limit: DEFAULT_CONTAINER_STORAGE_MB,
        },
        8080,
        7,
        false,
        "flag".into(),
    );
    assert!(validate_docker_container_spec(&spec).is_ok());

    spec.game_kind = rsctf_worker_protocol::GameKind::KingOfTheHill;
    assert!(validate_docker_container_spec(&spec).is_ok());
}

#[tokio::test]
async fn docker_create_rejects_competitive_egress_before_daemon_access() {
    let image = "registry.example/service@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let spec = ContainerSpec::ad_service(
        image.into(),
        ContainerResourceLimits {
            memory_limit: 256,
            cpu_count: 1,
            storage_limit: DEFAULT_CONTAINER_STORAGE_MB,
        },
        8080,
        7,
        true,
        "flag".into(),
    );
    let error = DockerContainerManager::default()
        .create(spec)
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        crate::utils::error::AppError::BadRequest(_)
    ));
    assert!(error.to_string().contains("use the Kubernetes backend"));
}

#[test]
fn container_resource_limits_reject_invalid_values() {
    let image = "registry.example/service@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let mut spec = ContainerSpec::ad_service(
        image.into(),
        ContainerResourceLimits {
            memory_limit: 256,
            cpu_count: 1,
            storage_limit: DEFAULT_CONTAINER_STORAGE_MB,
        },
        8080,
        7,
        false,
        "flag".into(),
    );
    assert!(validate_container_spec(&spec).is_ok());
    spec.memory_limit = -1;
    assert!(validate_container_spec(&spec).is_err());
    spec.memory_limit = 256;
    spec.cpu_count = 0;
    assert!(validate_container_spec(&spec).is_err());
    spec.cpu_count = 1;
    spec.storage_limit = 0;
    assert!(validate_container_spec(&spec).is_err());
    spec.storage_limit = DEFAULT_CONTAINER_STORAGE_MB;
    spec.expose_port = 65_536;
    assert!(validate_container_spec(&spec).is_err());
    spec.expose_port = 8080;
    spec.image = "registry.example/service:latest".to_string();
    assert!(validate_container_spec(&spec).is_err());

    spec.image = image.to_string();
    spec.network_mode = crate::utils::enums::NetworkMode::Custom;
    assert!(validate_container_spec(&spec).is_err());
    spec.network_mode = crate::utils::enums::NetworkMode::Isolated;
    assert!(validate_container_spec(&spec).is_err());
}

#[test]
fn challenge_container_logs_are_bounded() {
    let log_config = bounded_log_config();
    let options = log_config.config.expect("json-file options");

    assert_eq!(log_config.typ.as_deref(), Some("json-file"));
    assert_eq!(options.get("max-size").map(String::as_str), Some("5m"));
    assert_eq!(options.get("max-file").map(String::as_str), Some("3"));
}

#[test]
fn container_names_are_unique_for_identical_specs() {
    let env = vec![("RSCTF_TEAM_ID".to_string(), "7".to_string())];
    let first = container_name("registry.example/ctf/web:latest", &env, None);
    let second = container_name("registry.example/ctf/web:latest", &env, None);

    assert_ne!(first, second);
    assert!(first.starts_with("registry.example-ctf-web-t7-"));
    assert_eq!(first.rsplit('-').next().map(str::len), Some(12));
}

#[test]
fn recovery_operation_names_are_stable_and_scoped() {
    let env = vec![("RSCTF_TEAM_ID".to_string(), "7".to_string())];
    let first_operation = scoped_operation_id("scope-a", Some("koth-cycle:41"));
    let retry_operation = scoped_operation_id("scope-a", Some("koth-cycle:41"));
    let foreign_operation = scoped_operation_id("scope-b", Some("koth-cycle:41"));
    let next_operation = scoped_operation_id("scope-a", Some("koth-cycle:42"));
    let first = container_name(
        "registry.example/ctf/web:latest",
        &env,
        first_operation.as_deref(),
    );
    let retry = container_name(
        "registry.example/ctf/web:latest",
        &env,
        retry_operation.as_deref(),
    );
    let foreign = container_name(
        "registry.example/ctf/web:latest",
        &env,
        foreign_operation.as_deref(),
    );
    let next = container_name(
        "registry.example/ctf/web:latest",
        &env,
        next_operation.as_deref(),
    );
    let changed_image = container_name(
        "another.registry/renamed/image:latest",
        &[("RSCTF_TEAM_ID".to_string(), "99".to_string())],
        first_operation.as_deref(),
    );
    assert_eq!(first, retry);
    assert_eq!(first, changed_image);
    assert!(first.starts_with("rsctf-operation-"));
    assert_ne!(first, foreign);
    assert_ne!(first, next);
}

#[test]
fn only_terminal_docker_states_authorize_repair() {
    use bollard::models::ContainerStateStatusEnum as Status;

    assert_eq!(
        docker_liveness(Some(Status::RUNNING)),
        ContainerLiveness::Running
    );
    assert_eq!(
        docker_liveness(Some(Status::EXITED)),
        ContainerLiveness::Stopped
    );
    assert_eq!(
        docker_liveness(Some(Status::DEAD)),
        ContainerLiveness::Stopped
    );
    for state in [
        Status::CREATED,
        Status::PAUSED,
        Status::RESTARTING,
        Status::REMOVING,
    ] {
        assert_eq!(docker_liveness(Some(state)), ContainerLiveness::Unknown);
    }
    assert_eq!(docker_liveness(None), ContainerLiveness::Unknown);
}
