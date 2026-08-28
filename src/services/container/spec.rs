//! Backend-neutral container request and response models.

use rsctf_worker_protocol::GameKind;

use crate::utils::enums::{ChallengeType, NetworkMode};

pub(super) const TEAM_ENV: &str = "RSCTF_TEAM_ID";

/// Requested container configuration.
///
/// Mirrors RSCTF `Models.Internal.ContainerConfig`: the challenge image, its
/// resource limits, the port the challenge exposes inside the container, any
/// injected environment variables, and the flag to bake into the environment
/// for dynamic-flag challenges.
#[derive(Debug, Clone)]
pub struct ContainerSpec {
    /// Competition semantics for routing. Network shape is not a safe proxy:
    /// admin tests and ended-game practice can omit the A&D services network.
    pub game_kind: GameKind,
    /// Immutable image reference: a repository digest or, for one Docker
    /// daemon, a content-addressed local image id.
    pub image: String,
    /// Hard memory limit in megabytes.
    pub memory_limit: i32,
    /// CPU quota expressed as a whole CPU count (0.1 CPU units in RSCTF).
    pub cpu_count: i32,
    /// Hard writable-layer limit in mebibytes.
    pub storage_limit: i32,
    /// Port the challenge process listens on inside the container.
    pub expose_port: i32,
    /// Whether the backend may publish the exposed port outside the workload.
    /// Interactive inspectors disable this because their sole entry point is
    /// the authenticated exec hub. Docker also gives a no-publish workload no
    /// network when `ad_network` is absent, preventing default-bridge egress.
    pub publish_port: bool,
    /// Bind a published Jeopardy port only to the configured private proxy entry.
    pub proxy_only: bool,
    /// Additional environment variables injected at creation time.
    pub env: Vec<(String, String)>,
    /// Optional dynamic flag baked into the container environment.
    pub flag: Option<String>,
    /// A&D-over-VPN Docker network. It publishes no host ports and exposes the
    /// assigned VPN address and internal port through `ContainerInfo`.
    pub ad_network: Option<String>,
    /// Whether an A&D/KotH container may use backend-isolated outbound access.
    /// Kubernetes enforces this with a per-workload NetworkPolicy. Docker
    /// rejects it because a shared external bridge cannot prevent east-west,
    /// private-network, or metadata access.
    pub allow_egress: bool,
    /// Service and target-pod ports for narrow Kubernetes callback egress.
    pub control_plane_callback_ports: Vec<i32>,
    /// Author-selected network isolation for legacy container definitions.
    pub network_mode: NetworkMode,
    /// Stable lifecycle identity for crash-recoverable create operations. When
    /// present, a backend must adopt the matching existing workload instead of
    /// launching a second one after a retry.
    pub operation_id: Option<String>,
}

/// Resource ceilings shared by every container backend.
#[derive(Debug, Clone, Copy)]
pub struct ContainerResourceLimits {
    pub memory_limit: i32,
    pub cpu_count: i32,
    pub storage_limit: i32,
}

pub fn game_kind_for_challenge(challenge_type: ChallengeType) -> GameKind {
    match challenge_type {
        ChallengeType::AttackDefense => GameKind::AttackDefense,
        ChallengeType::KingOfTheHill => GameKind::KingOfTheHill,
        _ => GameKind::Jeopardy,
    }
}

impl ContainerSpec {
    /// Build the invariant placement for a platform-hosted A&D service: it joins
    /// the internal services network and is never published on a host port.
    /// Docker accepts only `allow_egress=false`; Kubernetes can enforce an
    /// allowed-egress policy per workload. Both the initial provision and every
    /// restart/reset must use this constructor.
    pub fn ad_service(
        image: String,
        resources: ContainerResourceLimits,
        expose_port: i32,
        team_id: i32,
        allow_egress: bool,
        flag: String,
    ) -> Self {
        Self {
            game_kind: GameKind::AttackDefense,
            image,
            memory_limit: resources.memory_limit,
            cpu_count: resources.cpu_count,
            storage_limit: resources.storage_limit,
            expose_port,
            publish_port: true,
            proxy_only: false,
            env: vec![(TEAM_ENV.into(), team_id.to_string())],
            flag: Some(flag),
            ad_network: Some(crate::services::ad_vpn::services_network()),
            allow_egress,
            control_plane_callback_ports: Vec::new(),
            network_mode: NetworkMode::Open,
            operation_id: None,
        }
    }
}

/// Runtime information about a created / running container.
///
/// Mirrors the parts of RSCTF `Models.Data.Container` that callers need to
/// reach the running instance: its backend id, the routable IP, the mapped
/// public port, and a coarse status string.
#[derive(Debug, Clone)]
pub struct ContainerInfo {
    /// Backend-assigned container id (Docker id or K8s pod name).
    pub id: String,
    /// Routable IP address the proxy/user connects to.
    pub ip: String,
    /// Publicly mapped port.
    pub port: i32,
    /// Coarse lifecycle status, e.g. `pending` / `running` / `destroyed`.
    pub status: String,
}
