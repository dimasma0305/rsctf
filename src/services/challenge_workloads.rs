//! Validation and stable identities for aggregate worker workloads attached to
//! Jeopardy container challenges.

use rsctf_worker_protocol::{GameKind, ValidatedWorkloadSpec, WorkloadSpec};
use serde::Serialize;
use serde_json::Value as JsonValue;

use crate::app_state::SharedState;
use crate::models::data::game_challenge;
use crate::utils::enums::ChallengeType;
use crate::utils::error::{AppError, AppResult};

/// Validate an API value before it crosses the database boundary. Aggregate
/// workloads intentionally remain a Jeopardy-only feature: A&D and KotH retain
/// their existing one-service lifecycle and constant scoring engine.
pub fn validate_for_challenge(
    challenge_type: ChallengeType,
    input: WorkloadSpec,
) -> AppResult<ValidatedWorkloadSpec> {
    if !matches!(
        challenge_type,
        ChallengeType::StaticContainer | ChallengeType::DynamicContainer
    ) {
        return Err(AppError::bad_request(
            "workloadSpec is supported only by Jeopardy container challenges",
        ));
    }
    if input.game_kind != GameKind::Jeopardy {
        return Err(AppError::bad_request(
            "a Jeopardy container challenge requires gameKind=jeopardy",
        ));
    }
    let validated = ValidatedWorkloadSpec::try_from(input)
        .map_err(|error| AppError::bad_request(format!("invalid workloadSpec: {error}")))?;
    if validated.flag_target.is_none() {
        return Err(AppError::bad_request(
            "workloadSpec.flagTarget is required for container challenges",
        ));
    }
    Ok(validated)
}

/// Revalidate stored JSON before runtime use. This protects the worker boundary
/// from manually edited or legacy database values as well as API input.
pub fn from_challenge(
    challenge: &game_challenge::Model,
) -> AppResult<Option<ValidatedWorkloadSpec>> {
    let Some(value) = challenge.workload_spec.clone() else {
        return Ok(None);
    };
    validate_json_for_challenge(challenge.challenge_type, value).map(Some)
}

pub fn validate_json_for_challenge(
    challenge_type: ChallengeType,
    value: JsonValue,
) -> AppResult<ValidatedWorkloadSpec> {
    let input = serde_json::from_value::<WorkloadSpec>(value)
        .map_err(|error| AppError::bad_request(format!("invalid stored workloadSpec: {error}")))?;
    validate_for_challenge(challenge_type, input)
}

/// Whether this challenge's workload definition is handled by the trusted
/// worker plane. Hybrid deployments deliberately keep A&D and KotH on
/// their local backend, so topology-wide worker support is not sufficient to
/// decide image portability for those challenge kinds.
pub(crate) fn uses_worker_runtime(st: &SharedState, challenge: &game_challenge::Model) -> bool {
    uses_worker_runtime_for_type(st, challenge.challenge_type)
}

pub(crate) fn uses_worker_runtime_for_type(
    st: &SharedState,
    challenge_type: ChallengeType,
) -> bool {
    worker_runtime_for(st.containers.supports_worker_workloads(), challenge_type)
}

fn worker_runtime_for(supports_worker_workloads: bool, challenge_type: ChallengeType) -> bool {
    supports_worker_workloads
        && matches!(
            challenge_type,
            ChallengeType::StaticContainer | ChallengeType::DynamicContainer
        )
}

#[derive(Clone, Debug)]
pub struct ResolvedChallengeRuntime {
    pub workload: Option<ValidatedWorkloadSpec>,
    pub identity: String,
    /// Internal Save -> launch -> publication fence. This can cover
    /// challenge-level launch inputs while the aggregate protocol identity
    /// remains byte-for-byte unchanged for API and persisted metadata.
    pub publication_fence: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimePublicationFence<'a> {
    schema: u8,
    runtime_identity: &'a str,
    challenge_type: ChallengeType,
    topology: &'static str,
    flag_template: Option<&'a str>,
}

fn legacy_topology(
    challenge_type: ChallengeType,
    enable_shared_container: bool,
    ad_self_hosted: bool,
) -> &'static str {
    match challenge_type {
        ChallengeType::KingOfTheHill => "koth-shared-ad-network",
        ChallengeType::AttackDefense if ad_self_hosted => "ad-self-hosted",
        ChallengeType::AttackDefense => "ad-managed-per-team",
        ChallengeType::StaticContainer if enable_shared_container => "shared",
        _ => "per-team",
    }
}

fn runtime_publication_fence(
    challenge: &game_challenge::Model,
    runtime_identity: &str,
) -> AppResult<String> {
    let value = RuntimePublicationFence {
        schema: 1,
        runtime_identity,
        challenge_type: challenge.challenge_type,
        topology: legacy_topology(
            challenge.challenge_type,
            challenge.enable_shared_container,
            challenge.ad_self_hosted,
        ),
        flag_template: (challenge.challenge_type == ChallengeType::DynamicContainer)
            .then_some(challenge.flag_template.as_deref())
            .flatten(),
    };
    let canonical =
        serde_json::to_vec(&value).map_err(|error| AppError::internal(error.to_string()))?;
    Ok(format!(
        "runtime-definition:sha256:{}",
        crate::utils::codec::sha256_hex(&canonical)
    ))
}

/// Resolve the exact runtime definition once while the caller holds the
/// challenge-definition fence. Aggregate identity remains the protocol hash
/// encoded from a persisted workloadSpec.
pub fn resolve_runtime(
    _st: &SharedState,
    challenge: &game_challenge::Model,
) -> AppResult<ResolvedChallengeRuntime> {
    let workload = from_challenge(challenge)?.ok_or_else(|| {
        AppError::bad_request(
            "Container definition missing workloadSpec; legacy single-container runtime is no longer supported",
        )
    })?;
    let identity = workload_identity(&workload)?;
    Ok(ResolvedChallengeRuntime {
        publication_fence: runtime_publication_fence(challenge, &identity)?,
        identity,
        workload: Some(workload),
    })
}

/// Stable bookkeeping identity for the persisted aggregate workload definition.
pub fn runtime_identity(st: &SharedState, challenge: &game_challenge::Model) -> AppResult<String> {
    resolve_runtime(st, challenge).map(|runtime| runtime.identity)
}

/// Stable identity exposed to editors and used as the Save -> Roll out fence.
pub fn workload_identity(spec: &ValidatedWorkloadSpec) -> AppResult<String> {
    let hash = spec
        .spec_hash()
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(format!("workload:sha256:{hash}"))
}

/// Worker handles name durable desired state, not an individual container
/// process. A transient Reconciling/Degraded observation during explicit
/// rollout must therefore never make a create/access path delete the workload.
pub fn is_stable_worker_runtime(backend_id: &str) -> bool {
    crate::services::worker::parse_worker_handle(backend_id).is_some()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RuntimeProbe {
    Running,
    Unknown,
    Stopped,
    NotFound,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExistingRuntimeAction {
    Reuse,
    Replace,
    FailClosed,
}

fn classify_existing_runtime(
    worker_handle: bool,
    identity_matches: bool,
    probe: RuntimeProbe,
) -> ExistingRuntimeAction {
    if !worker_handle {
        return if identity_matches && probe == RuntimeProbe::Running {
            ExistingRuntimeAction::Reuse
        } else {
            ExistingRuntimeAction::Replace
        };
    }
    match probe {
        RuntimeProbe::Running | RuntimeProbe::Unknown => ExistingRuntimeAction::Reuse,
        RuntimeProbe::Stopped | RuntimeProbe::NotFound => ExistingRuntimeAction::Replace,
        RuntimeProbe::Error => ExistingRuntimeAction::FailClosed,
    }
}

/// Probe a persisted runtime without turning a worker reconnect or rollout into
/// an implicit destroy. Worker NotFound/terminal states are replaceable;
/// unexpected worker-store errors propagate so callers retain the handle.
pub async fn existing_runtime_is_reusable(
    containers: &dyn crate::services::container::ContainerManager,
    backend_id: &str,
    recorded_identity: &str,
    saved_identity: &str,
) -> AppResult<bool> {
    let worker_handle = is_stable_worker_runtime(backend_id);
    let identity_matches = recorded_identity == saved_identity;
    if !worker_handle && !identity_matches {
        return Ok(false);
    }
    let probe = match containers.inspect_liveness(backend_id).await {
        Ok(crate::services::container::ContainerLiveness::Running) => RuntimeProbe::Running,
        Ok(crate::services::container::ContainerLiveness::Unknown) => RuntimeProbe::Unknown,
        Ok(crate::services::container::ContainerLiveness::Stopped) => RuntimeProbe::Stopped,
        Err(AppError::NotFound(_)) => RuntimeProbe::NotFound,
        Err(error) => {
            return match classify_existing_runtime(
                worker_handle,
                identity_matches,
                RuntimeProbe::Error,
            ) {
                ExistingRuntimeAction::FailClosed => Err(error),
                ExistingRuntimeAction::Reuse => Ok(true),
                ExistingRuntimeAction::Replace => Ok(false),
            };
        }
    };
    Ok(matches!(
        classify_existing_runtime(worker_handle, identity_matches, probe),
        ExistingRuntimeAction::Reuse
    ))
}

/// Live generation replacement removes every old service before recreating the
/// target. Only explicitly stateless definitions are safe for that operation.
pub fn ensure_live_rollout_is_stateless(workload: &ValidatedWorkloadSpec) -> AppResult<()> {
    if workload.services.iter().any(|service| !service.stateless) {
        return Err(AppError::bad_request(
            "live rollout requires every current and target service to set stateless=true; destroy and recreate stateful workloads",
        ));
    }
    Ok(())
}

/// Cross-replica fence shared by workload saves, explicit rollouts, and
/// provisioning publication. Provisioning holds it only while taking a fresh
/// definition snapshot and while publishing the resulting runtime, never while
/// waiting for a worker to launch containers.
pub fn definition_lock_key(game_id: i32, challenge_id: i32) -> String {
    format!("challenge-workload-rollout:{game_id}:{challenge_id}")
}

/// Serialize a tombstone teardown with the only state transition that can make
/// its stale cleanup destructive (false -> true). Its independent admission
/// gate is deliberately outside the bounded provisioning semaphore: cleanup
/// may take per-runtime provisioning locks while it owns this outer fence.
pub fn runtime_transition_lock_key(challenge_id: i32) -> String {
    format!("challenge-runtime-transition:{challenge_id}")
}

pub async fn acquire_runtime_transition_lock(
    pool: &sqlx::PgPool,
    challenge_id: i32,
) -> AppResult<crate::utils::single_flight::PgAdvisoryLock> {
    crate::utils::single_flight::PgAdvisoryLock::acquire_transition(
        pool,
        &runtime_transition_lock_key(challenge_id),
    )
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

pub async fn acquire_definition_lock(
    pool: &sqlx::PgPool,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<crate::utils::single_flight::PgAdvisoryLock> {
    crate::utils::single_flight::PgAdvisoryLock::acquire_definition(
        pool,
        &definition_lock_key(game_id, challenge_id),
    )
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

/// Nonblocking definition fence for callers that already hold the per-game
/// configuration lock. Returning `None` keeps that broad game fence short when
/// definition-only build, attachment, or publication work is in flight.
pub async fn try_acquire_definition_lock(
    pool: &sqlx::PgPool,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<Option<crate::utils::single_flight::PgAdvisoryLock>> {
    crate::utils::single_flight::PgAdvisoryLock::try_acquire(
        pool,
        &definition_lock_key(game_id, challenge_id),
    )
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

pub fn ensure_definition_unchanged(snapshot: &str, current: &str) -> AppResult<()> {
    if snapshot != current {
        return Err(AppError::conflict(
            "challenge workload changed while the container was starting; retry",
        ));
    }
    Ok(())
}

/// Dynamic Jeopardy containers generate their per-team flag. Every other
/// container mode selects an existing static flag row, if one exists.
pub async fn load_selected_static_flag(
    pool: &sqlx::PgPool,
    challenge_id: i32,
    challenge_type: ChallengeType,
) -> AppResult<Option<String>> {
    if challenge_type == ChallengeType::DynamicContainer {
        return Ok(None);
    }
    sqlx::query_scalar::<_, String>(
        r#"SELECT flag FROM "FlagContexts"
            WHERE challenge_id = $1
         ORDER BY id
           LIMIT 1"#,
    )
    .bind(challenge_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

fn ensure_selected_static_flag_unchanged(
    selected_flag: Option<&str>,
    still_exists: bool,
) -> AppResult<()> {
    if selected_flag.is_some() && !still_exists {
        return Err(AppError::conflict(
            "the selected static flag changed while the container was starting; retry",
        ));
    }
    Ok(())
}

/// Recheck a selected static flag in the same definition-lock transaction used
/// to publish the runtime. Additions do not invalidate the selection; removal
/// of the injected value does.
pub async fn ensure_selected_static_flag_current(
    lock: &mut crate::utils::single_flight::PgAdvisoryLock,
    challenge_id: i32,
    selected_flag: Option<&str>,
) -> AppResult<()> {
    let still_exists = match selected_flag {
        Some(flag) => sqlx::query_scalar::<_, bool>(
            r#"SELECT EXISTS (
                   SELECT 1 FROM "FlagContexts"
                    WHERE challenge_id = $1 AND flag = $2
               )"#,
        )
        .bind(challenge_id)
        .bind(flag)
        .fetch_one(&mut **lock.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?,
        None => true,
    };
    ensure_selected_static_flag_unchanged(selected_flag, still_exists)
}

pub fn has_runtime(challenge: &game_challenge::Model) -> bool {
    challenge.workload_spec.is_some()
}

/// Add one request-scoped value to every service in an aggregate workload.
/// The flag remains limited to `flagTarget`.
pub fn with_environment(
    workload: ValidatedWorkloadSpec,
    key: impl Into<String>,
    value: impl Into<String>,
) -> AppResult<ValidatedWorkloadSpec> {
    let key = key.into();
    let value = value.into();
    let mut spec = workload.into_inner();
    for service in &mut spec.services {
        service.environment.insert(key.clone(), value.clone());
    }
    ValidatedWorkloadSpec::try_from(spec)
        .map_err(|error| AppError::bad_request(format!("invalid workloadSpec: {error}")))
}

/// Inject the request-scoped flag only into the declared target service.
/// Keeping this alongside the other workload specializers lets creation and
/// in-place replica rollouts share exactly the same rule.
pub fn with_flag(
    workload: ValidatedWorkloadSpec,
    flag: Option<String>,
) -> AppResult<ValidatedWorkloadSpec> {
    let Some(flag) = flag else {
        return Ok(workload);
    };
    let mut spec = workload.into_inner();
    let target = spec.flag_target.as_ref().ok_or_else(|| {
        AppError::bad_request("workloadSpec.flagTarget is required when injecting a flag")
    })?;
    let service = spec
        .services
        .iter_mut()
        .find(|service| service.name == target.service)
        .expect("validated workload flag target service exists");
    service.environment.insert("RSCTF_FLAG".into(), flag);
    ValidatedWorkloadSpec::try_from(spec)
        .map_err(|error| AppError::bad_request(format!("invalid workloadSpec: {error}")))
}

/// Persist the protocol's canonical representation, not the potentially
/// reordered input JSON.
pub fn to_json(spec: ValidatedWorkloadSpec) -> AppResult<JsonValue> {
    serde_json::to_value(spec).map_err(|error| AppError::internal(error.to_string()))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use rsctf_worker_protocol::{
        EndpointRef, FlagTarget, ImageIdentity, OperatingSystem, Platform, PortProtocol,
        ResourceLimits, ServicePort, ServiceSpec,
    };

    use super::*;

    fn workload(kind: GameKind, replicas: u16, stateless: bool) -> WorkloadSpec {
        WorkloadSpec {
            game_kind: kind,
            platform: Platform {
                operating_system: OperatingSystem::Linux,
                architecture: "amd64".into(),
                windows_build: None,
            },
            services: vec![ServiceSpec {
                name: "app".into(),
                image: ImageIdentity::RegistryDigest {
                    repository: "registry.example/ctf/app".into(),
                    digest: format!("sha256:{}", "a".repeat(64)),
                },
                resources: ResourceLimits {
                    cpu_millis: 500,
                    memory_bytes: 128 * 1024 * 1024,
                },
                replicas,
                stateless,
                environment: BTreeMap::new(),
                ports: vec![ServicePort {
                    name: "http".into(),
                    container_port: 8080,
                    protocol: PortProtocol::Tcp,
                }],
            }],
            primary_endpoint: EndpointRef {
                service: "app".into(),
                port: "http".into(),
            },
            flag_target: Some(FlagTarget {
                service: "app".into(),
                path: "/flag".into(),
            }),
        }
    }

    #[test]
    fn accepts_stateless_jeopardy_replicas() {
        assert!(validate_for_challenge(
            ChallengeType::DynamicContainer,
            workload(GameKind::Jeopardy, 3, true),
        )
        .is_ok());
    }

    #[test]
    fn rejects_container_workload_without_flag_target() {
        let mut input = workload(GameKind::Jeopardy, 1, false);
        input.flag_target = None;
        assert!(validate_for_challenge(ChallengeType::StaticContainer, input).is_err());
    }

    #[test]
    fn rejects_worker_specs_for_engine_challenges() {
        assert!(validate_for_challenge(
            ChallengeType::AttackDefense,
            workload(GameKind::AttackDefense, 1, false),
        )
        .is_err());
        assert!(validate_for_challenge(
            ChallengeType::KingOfTheHill,
            workload(GameKind::KingOfTheHill, 1, false),
        )
        .is_err());
    }

    #[test]
    fn hybrid_routes_only_jeopardy_containers_to_worker_images() {
        assert!(worker_runtime_for(true, ChallengeType::StaticContainer));
        assert!(worker_runtime_for(true, ChallengeType::DynamicContainer));
        assert!(!worker_runtime_for(true, ChallengeType::AttackDefense));
        assert!(!worker_runtime_for(true, ChallengeType::KingOfTheHill));
        assert!(!worker_runtime_for(false, ChallengeType::DynamicContainer));
    }

    #[test]
    fn aggregate_identity_remains_the_protocol_hash() {
        let validated = validate_for_challenge(
            ChallengeType::DynamicContainer,
            workload(GameKind::Jeopardy, 2, true),
        )
        .unwrap();
        let expected = format!("workload:sha256:{}", validated.spec_hash().unwrap());
        assert_eq!(workload_identity(&validated).unwrap(), expected);
    }

    #[test]
    fn template_and_topology_match_actual_modes() {
        assert_eq!(
            legacy_topology(ChallengeType::StaticContainer, true, false),
            "shared"
        );
        assert_eq!(
            legacy_topology(ChallengeType::AttackDefense, false, true),
            "ad-self-hosted"
        );
        assert_eq!(
            legacy_topology(ChallengeType::AttackDefense, false, false),
            "ad-managed-per-team"
        );
        assert_eq!(
            legacy_topology(ChallengeType::KingOfTheHill, false, false),
            "koth-shared-ad-network"
        );
    }

    #[test]
    fn removed_selected_static_flag_rejects_publication() {
        assert!(ensure_selected_static_flag_unchanged(Some("flag"), true).is_ok());
        assert!(matches!(
            ensure_selected_static_flag_unchanged(Some("flag"), false),
            Err(AppError::Conflict(_))
        ));
        assert!(ensure_selected_static_flag_unchanged(None, false).is_ok());
    }

    #[test]
    fn request_environment_reaches_every_service() {
        let mut input = workload(GameKind::Jeopardy, 1, false);
        let mut second = input.services[0].clone();
        second.name = "sidecar".into();
        second.ports[0].name = "metrics".into();
        second.ports[0].container_port = 9090;
        input.services.push(second);
        let workload = validate_for_challenge(ChallengeType::DynamicContainer, input).unwrap();
        let workload = with_environment(workload, "RSCTF_TEAM_ID", "42").unwrap();
        assert!(workload.services.iter().all(|service| {
            service.environment.get("RSCTF_TEAM_ID").map(String::as_str) == Some("42")
        }));
    }

    #[test]
    fn valid_worker_identity_is_durable() {
        let handle = format!(
            "rsctf-worker:{}:{}:1",
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4()
        );
        assert!(is_stable_worker_runtime(&handle));
    }

    #[test]
    fn malformed_worker_identity_is_not_treated_as_durable() {
        assert!(!is_stable_worker_runtime("rsctf-worker:malformed"));
    }

    #[test]
    fn runtime_probe_retains_only_live_or_indeterminate_worker_handles() {
        assert_eq!(
            classify_existing_runtime(true, false, RuntimeProbe::Running),
            ExistingRuntimeAction::Reuse
        );
        assert_eq!(
            classify_existing_runtime(true, false, RuntimeProbe::Unknown),
            ExistingRuntimeAction::Reuse
        );
        assert_eq!(
            classify_existing_runtime(true, false, RuntimeProbe::Stopped),
            ExistingRuntimeAction::Replace
        );
        assert_eq!(
            classify_existing_runtime(true, false, RuntimeProbe::NotFound),
            ExistingRuntimeAction::Replace
        );
        assert_eq!(
            classify_existing_runtime(true, false, RuntimeProbe::Error),
            ExistingRuntimeAction::FailClosed
        );
        assert_eq!(
            classify_existing_runtime(false, true, RuntimeProbe::Running),
            ExistingRuntimeAction::Reuse
        );
        assert_eq!(
            classify_existing_runtime(false, true, RuntimeProbe::Unknown),
            ExistingRuntimeAction::Replace
        );
        assert_eq!(
            classify_existing_runtime(false, false, RuntimeProbe::Running),
            ExistingRuntimeAction::Replace
        );
    }

    #[test]
    fn definition_publication_rejects_a_late_old_runtime() {
        assert!(ensure_definition_unchanged("workload:sha256:a", "workload:sha256:a",).is_ok());
        assert!(ensure_definition_unchanged("workload:sha256:a", "workload:sha256:b",).is_err());
        assert_ne!(definition_lock_key(1, 2), definition_lock_key(1, 3));
        assert_ne!(
            runtime_transition_lock_key(2),
            runtime_transition_lock_key(3)
        );
        assert_ne!(runtime_transition_lock_key(2), definition_lock_key(1, 2));
    }
}
