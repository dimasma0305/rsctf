use std::collections::HashMap;
use std::time::Duration;

use rsctf_worker_protocol::{ValidatedWorkloadSpec, WorkloadSpec};
use serde::Serialize;
use serde_json::Value as JsonValue;
use sqlx::FromRow;
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::models::data::game_challenge;
use crate::services::challenge_mutations::load_challenge;
use crate::services::worker::{parse_worker_handle, WorkerContainerManager, WorkerHandle};
use crate::services::worker_store::DefinitionUpdateOutcome;
use crate::utils::error::{AppError, AppResult};

const ROLLOUT_TIMEOUT: Duration = Duration::from_secs(90);
const ROLLOUT_POLL_INITIAL: Duration = Duration::from_millis(200);
const ROLLOUT_POLL_MAX: Duration = Duration::from_secs(1);

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkloadRolloutModel {
    pub matched: usize,
    pub updated: usize,
    pub already_current: usize,
    pub stale: usize,
    pub incompatible: usize,
    pub insufficient_capacity: usize,
    pub failed: usize,
}

#[derive(Debug)]
struct PendingRollout {
    handle: WorkerHandle,
}

#[derive(Debug, FromRow)]
struct RolloutStateRow {
    id: Uuid,
    assignment_id: Uuid,
    generation: i64,
    desired_state: String,
    observed_state: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RolloutConvergence {
    Ready,
    Waiting,
    Stale,
    Failed,
}

pub(crate) async fn execute_workload_rollout_job(
    st: &SharedState,
    claimed: &crate::services::control_jobs::ClaimedControlJob,
) -> AppResult<WorkloadRolloutModel> {
    let job = &claimed.model;
    let challenge_id = job
        .challenge_id
        .ok_or_else(|| AppError::internal("workload rollout job has no challenge id"))?;
    crate::services::challenge_mutations::reject_pending_mutation(
        st.pg(),
        job.game_id,
        challenge_id,
    )
    .await?;
    let challenge = load_challenge(st, job.game_id, challenge_id).await?;
    let workload = crate::services::challenge_workloads::from_challenge(&challenge)?
        .ok_or_else(|| AppError::bad_request("no workloadSpec is saved"))?;
    crate::services::challenge_workloads::ensure_live_rollout_is_stateless(&workload)?;
    let runtime_identity = crate::services::challenge_workloads::runtime_identity(st, &challenge)?;
    let fingerprint = crate::services::control_jobs::fingerprint(&runtime_identity)?;
    if fingerprint != job.fingerprint {
        return Err(AppError::conflict(
            "the saved workload changed after this rollout was queued",
        ));
    }
    let endpoint_port = primary_endpoint_port(&workload);
    let rows = sqlx::query_as::<_, (Uuid, String)>(
        r#"SELECT container.id, container.container_id
             FROM "Containers" container
            WHERE container.id IN (
                  SELECT instance.container_id
                    FROM "GameInstances" instance
                   WHERE instance.challenge_id = $1
                     AND instance.container_id IS NOT NULL
                  UNION
                  SELECT challenge.shared_container_id
                    FROM "GameChallenges" challenge
                   WHERE challenge.id = $1
                     AND challenge.shared_container_id IS NOT NULL
                  UNION
                  SELECT challenge.test_container_id
                    FROM "GameChallenges" challenge
                   WHERE challenge.id = $1
                     AND challenge.test_container_id IS NOT NULL
            )
              AND container.container_id LIKE 'rsctf-worker:%'
         ORDER BY container.id"#,
    )
    .bind(challenge_id)
    .fetch_all(st.pg())
    .await
    .map_err(database_error)?;
    let manager = WorkerContainerManager::new(st.worker_store.clone());
    // Preflight every current definition before advancing the first one. The
    // manager repeats this check per update, closing a TOCTOU without allowing
    // one old stateful instance to turn the batch into a partial rollout.
    for (_, backend_id) in &rows {
        if let Some(handle) = parse_worker_handle(backend_id) {
            manager.preflight_rollout(handle, &workload).await?;
        }
    }
    let mut result = WorkloadRolloutModel {
        matched: rows.len(),
        updated: 0,
        already_current: 0,
        stale: 0,
        incompatible: 0,
        insufficient_capacity: 0,
        failed: 0,
    };
    let progress_total = i32::try_from(rows.len().max(1)).unwrap_or(i32::MAX);
    crate::services::control_jobs::set_progress(
        st.pg(),
        job.id,
        claimed.lease_token,
        0,
        progress_total,
    )
    .await?;
    let mut pending = Vec::new();
    let mut examined = 0i32;
    for (container_id, backend_id) in rows {
        if crate::services::control_jobs::cancellation_requested(
            st.pg(),
            job.id,
            claimed.lease_token,
        )
        .await?
        {
            return Ok(result);
        }
        let Some(handle) = parse_worker_handle(&backend_id) else {
            result.failed += 1;
            continue;
        };
        match manager.rollout(handle, workload.clone()).await {
            Ok(DefinitionUpdateOutcome::Updated { generation }) => {
                let next_handle = WorkerHandle {
                    generation,
                    ..handle
                };
                // Publish the fenced generation before waiting for runtime
                // convergence. The proxy rejects this handle until the exact
                // generation is Ready, but a slow image pull that outlives the
                // request timeout can still become reachable without another
                // edit request having to repair stale container metadata.
                if publish_container_metadata(
                    st,
                    container_id,
                    &backend_id,
                    &next_handle,
                    &runtime_identity,
                    endpoint_port,
                )
                .await?
                {
                    pending.push(PendingRollout {
                        handle: next_handle,
                    });
                } else {
                    result.stale += 1;
                }
            }
            Ok(DefinitionUpdateOutcome::AlreadyCurrent { .. }) => result.already_current += 1,
            Ok(DefinitionUpdateOutcome::Stale) => result.stale += 1,
            Ok(DefinitionUpdateOutcome::WorkerNoLongerCompatible) => result.incompatible += 1,
            Ok(DefinitionUpdateOutcome::InsufficientCapacity) => result.insufficient_capacity += 1,
            Err(error) => {
                result.failed += 1;
                tracing::warn!(%container_id, %error, "worker workload rollout failed");
            }
        }
        examined = examined.saturating_add(1).min(progress_total);
        crate::services::control_jobs::set_progress(
            st.pg(),
            job.id,
            claimed.lease_token,
            examined,
            progress_total,
        )
        .await?;
    }
    if crate::services::control_jobs::cancellation_requested(st.pg(), job.id, claimed.lease_token)
        .await?
    {
        return Ok(result);
    }
    await_rollout_convergence(st, &mut result, pending).await?;
    Ok(result)
}

async fn await_rollout_convergence(
    st: &SharedState,
    result: &mut WorkloadRolloutModel,
    mut pending: Vec<PendingRollout>,
) -> AppResult<()> {
    let deadline = tokio::time::Instant::now() + ROLLOUT_TIMEOUT;
    let mut poll_delay = ROLLOUT_POLL_INITIAL;
    while !pending.is_empty() {
        let ids = pending
            .iter()
            .map(|rollout| rollout.handle.workload_id)
            .collect::<Vec<_>>();
        let rows = sqlx::query_as::<_, RolloutStateRow>(
            r#"SELECT id, assignment_id, generation, desired_state, observed_state
                 FROM "WorkerWorkloads"
                WHERE id = ANY($1)"#,
        )
        .bind(&ids)
        .fetch_all(st.pg())
        .await
        .map_err(database_error)?
        .into_iter()
        .map(|row| (row.id, row))
        .collect::<HashMap<_, _>>();
        let mut waiting = Vec::new();
        for rollout in pending {
            match classify_convergence(&rollout, rows.get(&rollout.handle.workload_id)) {
                RolloutConvergence::Ready => result.updated += 1,
                RolloutConvergence::Stale => result.stale += 1,
                RolloutConvergence::Failed => result.failed += 1,
                RolloutConvergence::Waiting => waiting.push(rollout),
            }
        }
        pending = waiting;
        if pending.is_empty() {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            result.failed += pending.len();
            tracing::warn!(
                workloads = pending.len(),
                "worker workload rollout did not converge before its timeout"
            );
            break;
        }
        tokio::time::sleep(poll_delay).await;
        poll_delay = poll_delay
            .saturating_mul(3)
            .checked_div(2)
            .unwrap_or(ROLLOUT_POLL_MAX)
            .min(ROLLOUT_POLL_MAX);
    }
    Ok(())
}

fn classify_convergence(
    rollout: &PendingRollout,
    row: Option<&RolloutStateRow>,
) -> RolloutConvergence {
    let Some(row) = row else {
        return RolloutConvergence::Stale;
    };
    if row.assignment_id != rollout.handle.assignment_id
        || row.generation != rollout.handle.generation
        || row.desired_state != "Present"
    {
        return RolloutConvergence::Stale;
    }
    match row.observed_state.as_str() {
        "Ready" => RolloutConvergence::Ready,
        "Failed" => RolloutConvergence::Failed,
        _ => RolloutConvergence::Waiting,
    }
}

async fn publish_container_metadata(
    st: &SharedState,
    container_id: Uuid,
    previous_backend_id: &str,
    handle: &WorkerHandle,
    runtime_identity: &str,
    endpoint_port: i32,
) -> AppResult<bool> {
    let updated = sqlx::query(
        r#"UPDATE "Containers"
              SET container_id = $3, image = $4, port = $5
            WHERE id = $1 AND container_id = $2"#,
    )
    .bind(container_id)
    .bind(previous_backend_id)
    .bind(handle.encode())
    .bind(runtime_identity)
    .bind(endpoint_port)
    .execute(st.pg())
    .await
    .map_err(database_error)?;
    Ok(updated.rows_affected() == 1)
}

fn primary_endpoint_port(workload: &ValidatedWorkloadSpec) -> i32 {
    workload
        .services
        .iter()
        .find(|service| service.name == workload.primary_endpoint.service)
        .and_then(|service| {
            service
                .ports
                .iter()
                .find(|port| port.name == workload.primary_endpoint.port)
        })
        .map(|port| i32::from(port.container_port))
        .expect("validated workload primary endpoint exists")
}

#[cfg(test)]
fn rollout_lock_key(game_id: i32, challenge_id: i32) -> String {
    crate::services::challenge_workloads::definition_lock_key(game_id, challenge_id)
}

pub(crate) fn ensure_expected_identity(actual: &str, expected: Option<&str>) -> AppResult<()> {
    if expected.is_some_and(|expected| expected != actual) {
        return Err(AppError::conflict(
            "the saved workload changed; save again before rolling it out",
        ));
    }
    Ok(())
}

fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

/// Convert the three-state edit field (missing/set/clear) to canonical JSON.
/// Validation lives outside the controller so runtime and import paths reuse the
/// same game-mode invariants.
pub(crate) fn validate_update(
    challenge: &game_challenge::Model,
    update: &Option<Option<WorkloadSpec>>,
) -> AppResult<Option<Option<JsonValue>>> {
    match update {
        None => Ok(None),
        Some(None) => Ok(Some(None)),
        Some(Some(input)) => {
            let validated = crate::services::challenge_workloads::validate_for_challenge(
                challenge.challenge_type,
                input.clone(),
            )?;
            Ok(Some(Some(crate::services::challenge_workloads::to_json(
                validated,
            )?)))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use rsctf_worker_protocol::{
        EndpointRef, GameKind, ImageIdentity, OperatingSystem, Platform, PortProtocol,
        ResourceLimits, ServicePort, ServiceSpec,
    };

    use super::*;

    fn rollout_target(stateless: bool) -> ValidatedWorkloadSpec {
        ValidatedWorkloadSpec::try_from(WorkloadSpec {
            game_kind: GameKind::Jeopardy,
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
                    cpu_millis: 100,
                    memory_bytes: 64 * 1024 * 1024,
                    storage_bytes: rsctf_worker_protocol::DEFAULT_STORAGE_BYTES,
                },
                replicas: 1,
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
            flag_target: None,
        })
        .unwrap()
    }

    #[test]
    fn missing_and_null_remain_distinct() {
        let missing: Option<Option<WorkloadSpec>> = None;
        let cleared: Option<Option<WorkloadSpec>> = Some(None);
        assert!(missing.is_none());
        assert!(matches!(cleared, Some(None)));
    }

    #[test]
    fn convergence_requires_the_exact_ready_generation() {
        let rollout = PendingRollout {
            handle: WorkerHandle {
                workload_id: Uuid::new_v4(),
                assignment_id: Uuid::new_v4(),
                generation: 3,
            },
        };
        let mut row = RolloutStateRow {
            id: rollout.handle.workload_id,
            assignment_id: rollout.handle.assignment_id,
            generation: 3,
            desired_state: "Present".into(),
            observed_state: "Ready".into(),
        };
        assert_eq!(
            classify_convergence(&rollout, Some(&row)),
            RolloutConvergence::Ready
        );
        row.generation = 4;
        assert_eq!(
            classify_convergence(&rollout, Some(&row)),
            RolloutConvergence::Stale
        );
        row.generation = 3;
        row.observed_state = "Starting".into();
        assert_eq!(
            classify_convergence(&rollout, Some(&row)),
            RolloutConvergence::Waiting
        );
        row.observed_state = "Failed".into();
        assert_eq!(
            classify_convergence(&rollout, Some(&row)),
            RolloutConvergence::Failed
        );
    }

    #[test]
    fn lock_key_is_scoped_to_game_and_challenge() {
        assert_ne!(rollout_lock_key(1, 2), rollout_lock_key(1, 3));
        assert_ne!(rollout_lock_key(1, 2), rollout_lock_key(2, 2));
    }

    #[test]
    fn live_rollout_rejects_stateful_target_before_dispatch() {
        assert!(
            crate::services::challenge_workloads::ensure_live_rollout_is_stateless(
                &rollout_target(true)
            )
            .is_ok()
        );
        assert!(
            crate::services::challenge_workloads::ensure_live_rollout_is_stateless(
                &rollout_target(false)
            )
            .is_err()
        );
    }

    #[test]
    fn expected_identity_fences_the_save_to_rollout_gap() {
        assert!(ensure_expected_identity("workload:sha256:new", None).is_ok());
        assert!(
            ensure_expected_identity("workload:sha256:same", Some("workload:sha256:same")).is_ok()
        );
        assert!(matches!(
            ensure_expected_identity("workload:sha256:new", Some("workload:sha256:old")),
            Err(AppError::Conflict(_))
        ));
    }
}
