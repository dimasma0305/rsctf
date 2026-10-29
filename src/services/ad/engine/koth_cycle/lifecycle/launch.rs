//! Exact launch contract for a replacement hill.
use super::{data, CycleRow};
use crate::services::container::ContainerSpec;

pub(super) fn replacement_container_spec(
    image: String,
    cycle: &CycleRow,
    spec: &data::HillSpec,
    reporter: Option<&crate::services::ad::koth_reporter::TargetReporterRuntime>,
) -> ContainerSpec {
    let operation_id = replacement_operation_id(cycle, reporter);
    ContainerSpec {
        runtime_profile: spec.runtime_profile.clone(),
        game_kind: rsctf_worker_protocol::GameKind::KingOfTheHill,
        image,
        memory_limit: spec.memory_limit,
        cpu_count: spec.cpu_count,
        storage_limit: spec.storage_limit,
        expose_port: spec.expose_port,
        publish_port: true,
        proxy_only: false,
        env: reporter
            .map(|runtime| runtime.env.clone())
            .unwrap_or_default(),
        // Initial shared-hill provisioning injects the selected static flag.
        // Persistent arena replacements must preserve that exact runtime
        // contract as well; some challenge supervisors derive their internal
        // control credential from RSCTF_FLAG and fail closed when it is absent.
        flag: Some(spec.runtime_flag.clone().unwrap_or_default()),
        ad_network: Some(crate::services::ad_vpn::services_network()),
        allow_egress: spec.allow_egress,
        control_plane_callback_ports: reporter
            .map(|runtime| runtime.callback_ports.clone())
            .unwrap_or_default(),
        network_mode: crate::utils::enums::NetworkMode::Open,
        operation_id: Some(operation_id),
    }
}

pub(super) fn replacement_operation_id(
    cycle: &CycleRow,
    reporter: Option<&crate::services::ad::koth_reporter::TargetReporterRuntime>,
) -> String {
    let reporter_identity = reporter.map_or_else(String::new, |runtime| {
        // v0.1.92 can leave a crash-orphan under the unsuffixed identity. The
        // contract version fences that workload. The non-secret routing and
        // credential revisions prevent adoption when callback policy changes
        // or a route is revisited after its prior credential was revoked.
        format!(
            ":managed-reporter-v2:{}:{}",
            runtime.routing_revision, runtime.credential_revision
        )
    });
    format!(
        "koth-cycle:{}:attempt:{}{}",
        cycle.id, cycle.reset_attempt, reporter_identity
    )
}
