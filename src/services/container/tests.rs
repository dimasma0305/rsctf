use bollard::models::{Ipam, IpamConfig, Network};

use super::docker::docker_liveness;
use super::{
    ad_network_plan, bounded_log_config, bridge_network_matches, container_name,
    game_kind_for_challenge, validate_container_spec, ContainerLiveness, ContainerSpec,
};

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

#[test]
fn existing_ad_network_must_match_exact_ipv4_subnet() {
    let expected = inspected_network(&["10.13.40.0/24"], true);
    assert!(bridge_network_matches(
        &expected,
        Some("10.13.40.0/24"),
        true
    ));

    let stale = inspected_network(&["10.13.41.0/24"], true);
    assert!(!bridge_network_matches(&stale, Some("10.13.40.0/24"), true));

    let ambiguous = inspected_network(&["10.13.40.0/24", "10.13.41.0/24"], true);
    assert!(!bridge_network_matches(
        &ambiguous,
        Some("10.13.40.0/24"),
        true
    ));
}

#[test]
fn ad_service_specs_are_internal_only() {
    let spec = ContainerSpec::ad_service("image".into(), 256, 1, 8080, 7, false, "flag".into());
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
fn ad_egress_uses_a_separate_external_bridge() {
    let primary = crate::services::ad_vpn::services_network();
    assert_eq!(
        ad_network_plan(&primary, false),
        vec![(primary.clone(), true)]
    );
    let plan = ad_network_plan(&primary, true);
    assert_eq!(plan[0], (primary.clone(), true));
    assert_eq!(plan[1], (crate::services::ad_vpn::egress_network(), false));
    assert_ne!(plan[0].0, plan[1].0);
}

#[test]
fn container_resource_limits_reject_invalid_values() {
    let image = "registry.example/service@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let mut spec = ContainerSpec::ad_service(image.into(), 256, 1, 8080, 7, false, "flag".into());
    assert!(validate_container_spec(&spec).is_ok());
    spec.memory_limit = -1;
    assert!(validate_container_spec(&spec).is_err());
    spec.memory_limit = 256;
    spec.cpu_count = 0;
    assert!(validate_container_spec(&spec).is_err());
    spec.cpu_count = 1;
    spec.expose_port = 65_536;
    assert!(validate_container_spec(&spec).is_err());
    spec.expose_port = 8080;
    spec.image = "registry.example/service:latest".to_string();
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
    let first = container_name(
        "registry.example/ctf/web:latest",
        &env,
        Some("koth-cycle:41"),
    );
    let retry = container_name(
        "registry.example/ctf/web:latest",
        &env,
        Some("koth-cycle:41"),
    );
    let next = container_name(
        "registry.example/ctf/web:latest",
        &env,
        Some("koth-cycle:42"),
    );
    assert_eq!(first, retry);
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
