//! Read-only planning: which challenges take part, which launch definition
//! each one would use, and what the event would request in aggregate.

use rsctf_worker_protocol::{GameKind, ImageIdentity, ValidatedWorkloadSpec};
use serde::Serialize;

use super::{bounded_error, CapacityReport, ResourceTotals};
use crate::app_state::SharedState;
use crate::models::data::game_challenge;
use crate::services::container::{
    storage_limit_or_default, ContainerBackendKind, ContainerResourceLimits, ContainerSpec,
};
use crate::utils::enums::{ChallengeReviewStatus, ChallengeType, NetworkMode};
use crate::utils::error::{AppError, AppResult};

/// Placeholder flag baked into a temporary instance. Some supervisors fail
/// closed without `RSCTF_FLAG`; the value never reaches a player.
pub(super) const PREFLIGHT_FLAG: &str = "rsctf{image-preflight}";
const MAX_IMAGE_CHARS: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Worker,
    Local(ContainerBackendKind),
}

impl Backend {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Worker => "Worker",
            Self::Local(ContainerBackendKind::Docker) => "Docker",
            Self::Local(ContainerBackendKind::Kubernetes) => "Kubernetes",
            Self::Local(ContainerBackendKind::Worker) => "Worker",
            Self::Local(ContainerBackendKind::None) => "None",
        }
    }

    pub const fn is_worker(self) -> bool {
        matches!(self, Self::Worker)
    }
}

#[derive(Clone, Debug)]
pub enum Launch {
    Legacy(ContainerSpec),
    Workload {
        spec: ValidatedWorkloadSpec,
        proxy_only: bool,
    },
}

#[derive(Clone, Debug)]
pub struct PreflightTarget {
    pub challenge_id: i32,
    /// Display reference: the immutable image, or every workload image.
    pub image: String,
    /// Grouping identity; one temporary instance is started per distinct key.
    pub group_key: String,
    pub backend: Backend,
    pub launch: Option<Launch>,
    /// Why no instance will be started for this challenge.
    pub skip_reason: Option<String>,
    pub per_instance: ResourceTotals,
    pub instances: i64,
}

#[derive(Clone, Debug)]
pub struct PreflightPlan {
    pub targets: Vec<PreflightTarget>,
    pub fingerprint: String,
    pub capacity: CapacityReport,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FingerprintEntry<'a> {
    challenge_id: i32,
    group_key: &'a str,
    skipped: bool,
}

/// SHA-256 over the ordered (challenge, launch identity, skipped) tuples. Any
/// change to the participating set makes a queued job stale.
pub(super) fn fingerprint(targets: &[PreflightTarget]) -> AppResult<String> {
    let entries = targets
        .iter()
        .map(|target| FingerprintEntry {
            challenge_id: target.challenge_id,
            group_key: &target.group_key,
            skipped: target.skip_reason.is_some(),
        })
        .collect::<Vec<_>>();
    let encoded = serde_json::to_string(&entries)
        .map_err(|error| AppError::internal(format!("preflight fingerprint failed: {error}")))?;
    Ok(crate::utils::codec::sha256_str(&encoded))
}

/// Expected simultaneous instances once every accepted team plays.
pub(super) fn instances_for(challenge: &game_challenge::Model, accepted_teams: i64) -> i64 {
    match challenge.challenge_type {
        ChallengeType::StaticContainer | ChallengeType::KingOfTheHill => 1,
        ChallengeType::DynamicContainer if challenge.enable_shared_container => 1,
        _ => accepted_teams.max(1),
    }
}

fn participates(challenge: &game_challenge::Model) -> bool {
    challenge.is_enabled
        && challenge.review_status == ChallengeReviewStatus::Active
        && matches!(
            challenge.challenge_type,
            ChallengeType::StaticContainer
                | ChallengeType::DynamicContainer
                | ChallengeType::AttackDefense
                | ChallengeType::KingOfTheHill
        )
        && !(challenge.challenge_type == ChallengeType::AttackDefense && challenge.ad_self_hosted)
        && (challenge.workload_spec.is_some()
            || challenge
                .container_image
                .as_deref()
                .is_some_and(|image| !image.trim().is_empty()))
}

fn mib(value: i32) -> i64 {
    i64::from(value.max(0)) << 20
}

fn legacy_limits(challenge: &game_challenge::Model) -> ContainerResourceLimits {
    let default_memory = if challenge.challenge_type == ChallengeType::AttackDefense {
        256
    } else {
        64
    };
    ContainerResourceLimits {
        memory_limit: challenge.memory_limit.unwrap_or(default_memory),
        cpu_count: challenge.cpu_count.unwrap_or(1),
        storage_limit: storage_limit_or_default(challenge.storage_limit),
    }
}

pub(super) fn legacy_totals(limits: ContainerResourceLimits) -> ResourceTotals {
    ResourceTotals {
        cpu_millis: i64::from(limits.cpu_count.max(0)).saturating_mul(1_000),
        memory_bytes: mib(limits.memory_limit),
        storage_bytes: mib(limits.storage_limit),
        replicas: 1,
        slots: 1,
    }
}

pub(super) fn workload_totals(spec: &ValidatedWorkloadSpec) -> ResourceTotals {
    let mut totals = ResourceTotals {
        slots: 1,
        ..ResourceTotals::default()
    };
    for service in &spec.services {
        let replicas = i64::from(service.replicas);
        totals.add(ResourceTotals {
            cpu_millis: i64::from(service.resources.cpu_millis).saturating_mul(replicas),
            memory_bytes: i64::try_from(service.resources.memory_bytes)
                .unwrap_or(i64::MAX)
                .saturating_mul(replicas),
            storage_bytes: i64::try_from(service.resources.storage_bytes)
                .unwrap_or(i64::MAX)
                .saturating_mul(replicas),
            replicas,
            slots: 0,
        });
    }
    totals
}

fn image_display(identity: &ImageIdentity) -> String {
    match identity {
        ImageIdentity::RegistryDigest { repository, digest } => format!("{repository}@{digest}"),
        ImageIdentity::WorkerLocal {
            worker_id,
            image_id,
        } => format!("worker:{worker_id}/{image_id}"),
    }
}

fn bounded_image(text: String) -> String {
    if text.chars().count() <= MAX_IMAGE_CHARS {
        return text;
    }
    text.chars()
        .take(MAX_IMAGE_CHARS - 1)
        .chain(['…'])
        .collect()
}

/// Build the exact launch definition real provisioning would use for one
/// temporary instance, with placeholder team/flag inputs.
fn legacy_spec(
    challenge: &game_challenge::Model,
    image: String,
    proxy_only: bool,
) -> ContainerSpec {
    let limits = legacy_limits(challenge);
    let expose_port = challenge.expose_port.unwrap_or(80);
    match challenge.challenge_type {
        ChallengeType::AttackDefense => ContainerSpec::ad_service(
            image,
            limits,
            expose_port,
            0,
            challenge.ad_allow_egress,
            PREFLIGHT_FLAG.to_string(),
        ),
        ChallengeType::KingOfTheHill => ContainerSpec {
            game_kind: GameKind::KingOfTheHill,
            image,
            memory_limit: limits.memory_limit,
            cpu_count: limits.cpu_count,
            storage_limit: limits.storage_limit,
            expose_port,
            publish_port: true,
            proxy_only: false,
            env: Vec::new(),
            flag: Some(PREFLIGHT_FLAG.to_string()),
            ad_network: Some(crate::services::ad_vpn::services_network()),
            allow_egress: challenge.ad_allow_egress,
            control_plane_callback_ports: Vec::new(),
            network_mode: NetworkMode::Open,
            operation_id: None,
        },
        _ => {
            let network_mode = challenge.network_mode.unwrap_or(NetworkMode::Open);
            ContainerSpec {
                game_kind: GameKind::Jeopardy,
                image,
                memory_limit: limits.memory_limit,
                cpu_count: limits.cpu_count,
                storage_limit: limits.storage_limit,
                expose_port,
                publish_port: true,
                proxy_only,
                env: Vec::new(),
                flag: Some(PREFLIGHT_FLAG.to_string()),
                ad_network: None,
                allow_egress: network_mode == NetworkMode::Open,
                control_plane_callback_ports: Vec::new(),
                network_mode,
                operation_id: None,
            }
        }
    }
}

fn backend_for(st: &SharedState, challenge: &game_challenge::Model) -> Backend {
    if crate::services::challenge_workloads::uses_worker_runtime(st, challenge) {
        Backend::Worker
    } else {
        Backend::Local(st.containers.backend_kind())
    }
}

fn target_for(
    st: &SharedState,
    challenge: &game_challenge::Model,
    accepted_teams: i64,
    proxy_only: bool,
) -> PreflightTarget {
    let instances = instances_for(challenge, accepted_teams);
    let backend = backend_for(st, challenge);
    let configured_image = challenge
        .container_image
        .clone()
        .unwrap_or_default()
        .trim()
        .to_string();
    match crate::services::challenge_workloads::resolve_runtime(st, challenge) {
        Ok(runtime) => match runtime.workload {
            Some(spec) => {
                let image = bounded_image(
                    spec.services
                        .iter()
                        .map(|service| image_display(&service.image))
                        .collect::<Vec<_>>()
                        .join(", "),
                );
                PreflightTarget {
                    challenge_id: challenge.id,
                    image,
                    group_key: runtime.identity,
                    backend,
                    per_instance: workload_totals(&spec),
                    launch: Some(Launch::Workload { spec, proxy_only }),
                    skip_reason: None,
                    instances,
                }
            }
            None => {
                let image = runtime
                    .legacy_image
                    .unwrap_or_else(|| configured_image.clone());
                let spec = legacy_spec(challenge, image.clone(), proxy_only);
                PreflightTarget {
                    challenge_id: challenge.id,
                    group_key: format!("image:{image}"),
                    image: bounded_image(image),
                    backend,
                    per_instance: legacy_totals(legacy_limits(challenge)),
                    launch: Some(Launch::Legacy(spec)),
                    skip_reason: None,
                    instances,
                }
            }
        },
        Err(error) => PreflightTarget {
            challenge_id: challenge.id,
            group_key: format!("unresolved:{}", challenge.id),
            image: bounded_image(configured_image),
            backend,
            launch: None,
            skip_reason: Some(bounded_error(error.to_string())),
            per_instance: legacy_totals(legacy_limits(challenge)),
            instances,
        },
    }
}

async fn accepted_teams(pool: &sqlx::PgPool, game_id: i32) -> AppResult<i64> {
    sqlx::query_scalar::<_, i64>(
        r#"SELECT COUNT(*)::BIGINT FROM "Participations"
            WHERE game_id = $1 AND status = $2"#,
    )
    .bind(game_id)
    .bind(crate::utils::enums::ParticipationStatus::Accepted as i16)
    .fetch_one(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

pub(super) fn capacity_report(
    targets: &[PreflightTarget],
    accepted_teams: i64,
    available: Option<crate::services::worker_store::WorkerCapacitySummary>,
) -> CapacityReport {
    let mut report = CapacityReport {
        accepted_teams,
        available,
        ..CapacityReport::default()
    };
    for target in targets {
        let scaled = target.per_instance.scaled(target.instances);
        report.requested.add(scaled);
        report.instances = report.instances.saturating_add(target.instances);
        if target.backend.is_worker() {
            report.worker_requested.add(scaled);
        }
    }
    report
}

/// Plan the preflight for one game. This performs no runtime work.
pub async fn plan(st: &SharedState, game_id: i32) -> AppResult<PreflightPlan> {
    let challenges = crate::controllers::edit::load_game_challenges(st.pg(), game_id).await?;
    let accepted = accepted_teams(st.pg(), game_id).await?;
    let platform_proxy =
        crate::controllers::admin::container_port_mapping(st).await == "PlatformProxy";
    let proxy_only = crate::services::container::should_use_platform_proxy(
        GameKind::Jeopardy,
        st.containers.requires_proxy(),
        platform_proxy,
        false,
    );
    let mut targets = challenges
        .iter()
        .filter(|challenge| participates(challenge))
        .map(|challenge| target_for(st, challenge, accepted, proxy_only))
        .collect::<Vec<_>>();
    targets.sort_by_key(|target| target.challenge_id);
    let available = if st.containers.supports_worker_workloads() {
        Some(
            st.worker_store
                .capacity_summary()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?,
        )
    } else {
        None
    };
    let capacity = capacity_report(&targets, accepted, available);
    let fingerprint = fingerprint(&targets)?;
    Ok(PreflightPlan {
        targets,
        fingerprint,
        capacity,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(id: i32, key: &str, skipped: bool, worker: bool) -> PreflightTarget {
        PreflightTarget {
            challenge_id: id,
            image: key.to_string(),
            group_key: key.to_string(),
            backend: if worker {
                Backend::Worker
            } else {
                Backend::Local(ContainerBackendKind::Docker)
            },
            launch: None,
            skip_reason: skipped.then(|| "unresolved".to_string()),
            per_instance: ResourceTotals {
                cpu_millis: 1_000,
                memory_bytes: 64 << 20,
                storage_bytes: 512 << 20,
                replicas: 1,
                slots: 1,
            },
            instances: 2,
        }
    }

    #[test]
    fn fingerprint_covers_membership_identity_and_skips() {
        let base = vec![
            target(1, "image:a", false, true),
            target(2, "image:b", false, true),
        ];
        let same = fingerprint(&base).unwrap();
        assert_eq!(same, fingerprint(&base.clone()).unwrap());
        assert_eq!(same.len(), 64);
        let reimaged = vec![
            target(1, "image:a", false, true),
            target(2, "image:c", false, true),
        ];
        assert_ne!(same, fingerprint(&reimaged).unwrap());
        let skipped = vec![
            target(1, "image:a", true, true),
            target(2, "image:b", false, true),
        ];
        assert_ne!(same, fingerprint(&skipped).unwrap());
        let fewer = vec![target(1, "image:a", false, true)];
        assert_ne!(same, fingerprint(&fewer).unwrap());
    }

    #[test]
    fn capacity_scales_by_expected_instances_and_splits_worker_share() {
        let targets = vec![
            target(1, "image:a", false, true),
            target(2, "image:b", false, false),
        ];
        let report = capacity_report(&targets, 2, None);
        assert_eq!(report.instances, 4);
        assert_eq!(report.requested.cpu_millis, 4_000);
        assert_eq!(report.requested.slots, 4);
        assert_eq!(report.worker_requested.cpu_millis, 2_000);
        assert_eq!(report.accepted_teams, 2);
        assert!(report.available.is_none());
    }

    #[test]
    fn legacy_totals_convert_mib_and_whole_cpus() {
        let totals = legacy_totals(ContainerResourceLimits {
            memory_limit: 128,
            cpu_count: 2,
            storage_limit: 256,
        });
        assert_eq!(totals.cpu_millis, 2_000);
        assert_eq!(totals.memory_bytes, 128 << 20);
        assert_eq!(totals.storage_bytes, 256 << 20);
        assert_eq!((totals.replicas, totals.slots), (1, 1));
    }

    #[test]
    fn image_display_is_bounded() {
        let long = "r".repeat(MAX_IMAGE_CHARS * 2);
        assert_eq!(bounded_image(long).chars().count(), MAX_IMAGE_CHARS);
        assert_eq!(
            image_display(&ImageIdentity::RegistryDigest {
                repository: "registry.example/ctf/app".into(),
                digest: "sha256:abc".into(),
            }),
            "registry.example/ctf/app@sha256:abc"
        );
    }

    #[test]
    fn backend_names_are_stable_wire_values() {
        assert_eq!(Backend::Worker.as_str(), "Worker");
        assert_eq!(
            Backend::Local(ContainerBackendKind::Docker).as_str(),
            "Docker"
        );
        assert_eq!(
            Backend::Local(ContainerBackendKind::Kubernetes).as_str(),
            "Kubernetes"
        );
        assert!(!Backend::Local(ContainerBackendKind::None).is_worker());
    }
}
