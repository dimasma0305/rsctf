use super::*;

fn input_bool(input: &Value, key: &str, default: bool) -> bool {
    input.get(key).and_then(Value::as_bool).unwrap_or(default)
}

pub(super) async fn execute_claimed(
    state: &StateHandle,
    job: &ClaimedControlJob,
) -> AppResult<Value> {
    match job.model.kind.as_str() {
        "VariantGeneration" => {
            let generated =
                crate::services::event_security::generate_event_variants_for_job(state, job)
                    .await?;
            Ok(serde_json::json!({ "generated": generated }))
        }
        "SecurityDerivation" => {
            let inserted =
                crate::services::event_security::derive_context_findings(state, job.model.game_id)
                    .await?;
            Ok(serde_json::json!({ "inserted": inserted }))
        }
        "AdReconcile" => {
            let (launched, failures) = crate::controllers::edit::run_ad_reconcile_job(
                state,
                job,
                input_bool(&job.input, "ensureVpn", false),
                input_bool(&job.input, "ensureKoth", false),
            )
            .await?;
            Ok(serde_json::json!({ "launched": launched, "failures": failures }))
        }
        "ChallengeBuild" => {
            crate::controllers::edit::execute_challenge_build_job(state, &job.model, &job.input)
                .await
        }
        "BuildBatch" => crate::controllers::edit::execute_build_batch_job(state, job).await,
        "WorkloadRollout" => {
            let result = crate::controllers::edit::execute_workload_rollout_job(state, job).await?;
            serde_json::to_value(result)
                .map_err(|error| AppError::internal(format!("rollout result failed: {error}")))
        }
        "AdReset" => crate::services::ad::reset::execute_job(state, job).await,
        _ => Err(AppError::internal("unsupported claimed control-job kind")),
    }
}
