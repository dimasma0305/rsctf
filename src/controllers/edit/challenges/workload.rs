//! HTTP authorization and edit fields for runtime workload operations.
use super::{load_challenge, manager_or_admin};
use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::CurrentUser;
use crate::services::workload_rollouts::ensure_expected_identity;
pub(super) use crate::services::workload_rollouts::validate_update;
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::RequestResponse;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
const EXPECTED_WORKLOAD_HEADER: &str = "x-rsctf-expected-workload";

/// Deliberately apply an all-stateless saved definition to running trusted-worker
/// instances. Generation replacement recreates every service, so stateful
/// targets must be destroyed and launched again instead of rolled in place.
pub async fn rollout_workloads(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game_id, challenge_id)): Path<(i32, i32)>,
    headers: HeaderMap,
) -> AppResult<(
    axum::http::StatusCode,
    RequestResponse<crate::services::control_jobs::ControlJobModel>,
)> {
    manager_or_admin(&st, &user, game_id).await?;
    super::reject_pending_mutation(st.pg(), game_id, challenge_id).await?;
    let challenge = load_challenge(&st, game_id, challenge_id).await?;
    let workload = crate::services::challenge_workloads::from_challenge(&challenge)?
        .ok_or_else(|| AppError::bad_request("no workloadSpec is saved"))?;
    crate::services::challenge_workloads::ensure_live_rollout_is_stateless(&workload)?;
    let runtime_identity = crate::services::challenge_workloads::runtime_identity(&st, &challenge)?;
    let expected_identity = headers
        .get(EXPECTED_WORKLOAD_HEADER)
        .map(|value| {
            value
                .to_str()
                .map_err(|_| AppError::bad_request("invalid expected workload identity header"))
        })
        .transpose()?;
    ensure_expected_identity(&runtime_identity, expected_identity)?;
    let operation_id = super::super::control_jobs::operation_id(&headers)?;
    let fingerprint = super::super::control_jobs::fingerprint(&runtime_identity)?;
    let job = crate::services::control_jobs::enqueue(
        st.pg(),
        crate::services::control_jobs::ControlJobKind::WorkloadRollout,
        &format!("challenge:{challenge_id}"),
        game_id,
        Some(challenge_id),
        operation_id,
        &fingerprint,
        serde_json::json!({ "challengeId": challenge_id }),
    )
    .await?;
    crate::services::control_jobs::kick(st);
    Ok((axum::http::StatusCode::ACCEPTED, RequestResponse::ok(job)))
}

/// Fields whose presence can change a legacy single-service launch or its
/// ownership topology. Aggregate workloads retain their protocol identity, but
/// use the same fence so switching between aggregate and legacy is ordered.
pub(super) fn update_changes_runtime_definition(model: &super::ChallengeUpdateModel) -> bool {
    model.workload_spec.is_some()
        || model.runtime_profile.is_some()
        || model.replace_runtime_instances
        || model.is_enabled.is_some()
        || model.container_image.is_some()
        || model.memory_limit.is_some()
        || model.cpu_count.is_some()
        || model.storage_limit.is_some()
        || model.network_mode.is_some()
        || model.expose_port.is_some()
        || model.flag_template.is_some()
        || model.enable_traffic_capture.is_some()
        || model.ad_checker_image.is_some()
        || model.ad_allow_egress.is_some()
        || model.ad_allow_self_reset.is_some()
        || model.ad_ssh_requires_flag.is_some()
        || model.enable_shared_container.is_some()
        || model.ad_self_hosted.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn runtime_update_predicate_covers_every_legacy_launch_field() {
        let fields = [
            "workloadSpec",
            "runtimeProfile",
            "replaceRuntimeInstances",
            "isEnabled",
            "containerImage",
            "memoryLimit",
            "cpuCount",
            "storageLimit",
            "networkMode",
            "exposePort",
            "flagTemplate",
            "enableTrafficCapture",
            "adCheckerImage",
            "adAllowEgress",
            "adAllowSelfReset",
            "adSshRequiresFlag",
            "enableSharedContainer",
            "adSelfHosted",
        ];
        for field in fields {
            let value = if field == "workloadSpec" {
                serde_json::Value::Null
            } else if field == "networkMode" {
                serde_json::json!("Isolated")
            } else if matches!(
                field,
                "isEnabled"
                    | "replaceRuntimeInstances"
                    | "enableTrafficCapture"
                    | "adAllowEgress"
                    | "adAllowSelfReset"
                    | "adSshRequiresFlag"
                    | "enableSharedContainer"
                    | "adSelfHosted"
            ) {
                serde_json::json!(true)
            } else if matches!(
                field,
                "memoryLimit" | "cpuCount" | "storageLimit" | "exposePort"
            ) {
                serde_json::json!(1)
            } else {
                serde_json::json!("value")
            };
            let mut object = serde_json::Map::new();
            object.insert(field.to_string(), value);
            let model: crate::controllers::edit::ChallengeUpdateModel =
                serde_json::from_value(serde_json::Value::Object(object)).unwrap();
            assert!(update_changes_runtime_definition(&model), "missed {field}");
        }
        let title_only: crate::controllers::edit::ChallengeUpdateModel =
            serde_json::from_value(serde_json::json!({ "title": "new" })).unwrap();
        assert!(!update_changes_runtime_definition(&title_only));
    }
}
