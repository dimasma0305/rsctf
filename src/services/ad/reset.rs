//! Durable A&D service reset execution.
//!
//! HTTP handlers authorize and reserve the opaque operation before this code
//! enters provisioning admission. The expected backend fence prevents a late
//! retry from retiring a replacement created by an earlier operation.

use chrono::Utc;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::app_state::SharedState;
use crate::models::data::{ad_team_service, game, game_challenge, participation};
use crate::services::container::{
    storage_limit_or_default, ContainerResourceLimits, ContainerSpec,
};
use crate::utils::enums::{ChallengeReviewStatus, ChallengeType, ParticipationStatus};
use crate::utils::error::{AppError, AppResult};

const DEFAULT_PLAYER_COOLDOWN_SECONDS: i64 = 300;
const INFRASTRUCTURE_RESET_FLOOR_SECONDS: i64 = 30;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResetJobInput {
    service_id: i32,
    participation_id: i32,
    expected_backend_id: Option<String>,
    player_policy: bool,
    #[serde(default)]
    reset_prepared: bool,
    #[serde(default)]
    prepared_round_id: Option<i32>,
    #[serde(default)]
    retired_backend_id: Option<String>,
    #[serde(default)]
    replacement_backend_id: Option<String>,
}

fn backend_matches(actual: Option<&str>, expected: Option<&str>) -> bool {
    actual.filter(|value| !value.is_empty()) == expected.filter(|value| !value.is_empty())
}

pub async fn execute_job(
    st: &SharedState,
    claimed: &crate::services::control_jobs::ClaimedControlJob,
) -> AppResult<Value> {
    let job = &claimed.model;
    let input: ResetJobInput = serde_json::from_value(claimed.input.clone())
        .map_err(|error| AppError::internal(format!("invalid reset job input: {error}")))?;
    let initial = ad_team_service::Entity::find_by_id(input.service_id)
        .one(&st.db)
        .await?
        .filter(|service| {
            service.game_id == job.game_id && service.participation_id == input.participation_id
        })
        .ok_or_else(|| AppError::not_found("Service not found"))?;
    let replacement_checkpointed = input.replacement_backend_id.is_some();
    let replacement_published = input
        .replacement_backend_id
        .as_deref()
        .is_some_and(|replacement| initial.container_id.as_deref() == Some(replacement));
    let recovering_preparation = input.reset_prepared && input.replacement_backend_id.is_none();
    if replacement_published && !initial.host.trim().is_empty() && initial.port > 0 {
        return Ok(json!({ "reset": true, "alreadyReplaced": true }));
    }
    if !replacement_checkpointed
        && !recovering_preparation
        && !backend_matches(
            initial.container_id.as_deref(),
            input.expected_backend_id.as_deref(),
        )
    {
        return Ok(json!({ "reset": false, "alreadyReplaced": true }));
    }

    let lock_key = format!(
        "ad-service:{}:{}",
        initial.participation_id, initial.challenge_id
    );
    let _local = crate::utils::single_flight::coalesce(&lock_key).await;
    let distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(st.pg(), &lock_key)
            .await?;

    let service = ad_team_service::Entity::find_by_id(input.service_id)
        .one(&st.db)
        .await?
        .filter(|service| {
            service.game_id == job.game_id && service.participation_id == input.participation_id
        })
        .ok_or_else(|| AppError::not_found("Service not found"))?;
    let replacement_checkpointed = input.replacement_backend_id.is_some();
    let replacement_published = input
        .replacement_backend_id
        .as_deref()
        .is_some_and(|replacement| service.container_id.as_deref() == Some(replacement));
    let recovering_preparation = input.reset_prepared && input.replacement_backend_id.is_none();
    if replacement_published && !service.host.trim().is_empty() && service.port > 0 {
        distributed.release().await?;
        return Ok(json!({ "reset": true, "alreadyReplaced": true }));
    }
    if !replacement_checkpointed
        && !recovering_preparation
        && !backend_matches(
            service.container_id.as_deref(),
            input.expected_backend_id.as_deref(),
        )
    {
        distributed.release().await?;
        return Ok(json!({ "reset": false, "alreadyReplaced": true }));
    }
    let part = participation::Entity::find()
        .filter(participation::Column::Id.eq(service.participation_id))
        .filter(participation::Column::GameId.eq(job.game_id))
        .filter(participation::Column::Status.eq(ParticipationStatus::Accepted))
        .one(&st.db)
        .await?
        .ok_or(AppError::Forbidden)?;
    let challenge = game_challenge::Entity::find()
        .filter(game_challenge::Column::Id.eq(service.challenge_id))
        .filter(game_challenge::Column::GameId.eq(job.game_id))
        .filter(game_challenge::Column::IsEnabled.eq(true))
        .filter(game_challenge::Column::ReviewStatus.eq(ChallengeReviewStatus::Active))
        .filter(game_challenge::Column::ChallengeType.eq(ChallengeType::AttackDefense))
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Active A&D challenge not found"))?;
    if challenge.ad_self_hosted {
        return Err(AppError::bad_request(
            "Self-hosted services cannot be reset from the platform",
        ));
    }
    if input.player_policy && !challenge.ad_allow_self_reset {
        return Err(AppError::bad_request(
            "Self-reset is not allowed for this service",
        ));
    }
    let game = game::Entity::find_by_id(job.game_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Game not found"))?;
    if !game.is_active(Utc::now()) {
        return Err(AppError::bad_request(
            "Service reset is only available while the game is running",
        ));
    }
    if input.player_policy && !replacement_checkpointed && !recovering_preparation {
        let cooldown = game
            .ad_reset_cooldown_minutes
            .map(|minutes| i64::from(minutes) * 60)
            .unwrap_or(DEFAULT_PLAYER_COOLDOWN_SECONDS)
            .max(INFRASTRUCTURE_RESET_FLOOR_SECONDS);
        if service
            .last_reset_at
            .is_some_and(|last| (Utc::now() - last).num_seconds() < cooldown)
        {
            return Err(AppError::conflict(
                "Reset cooldown is still active; retry later",
            ));
        }
    }

    let image = crate::services::challenge_images::runtime_image(st, &challenge)?;
    let (prepared_round_id, current_flag) = if replacement_checkpointed || recovering_preparation {
        let flag = match input.prepared_round_id {
            Some(round_id) => sqlx::query_scalar::<_, String>(
                r#"SELECT flag FROM "AdFlags"
                    WHERE round_id = $1 AND team_service_id = $2"#,
            )
            .bind(round_id)
            .bind(service.id)
            .fetch_optional(st.pg())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?,
            None => None,
        };
        if recovering_preparation {
            crate::services::ad_vpn::deactivate_team_service(&st.db, service.id).await?;
            if let Some(retired_backend_id) = input
                .retired_backend_id
                .as_deref()
                .filter(|retired| service.container_id.as_deref() == Some(*retired))
            {
                crate::services::traffic::destroy_container_after_capture_fence(
                    st,
                    retired_backend_id,
                )
                .await?;
            }
        }
        (input.prepared_round_id, flag)
    } else {
        let reason = if input.player_policy {
            "service reset before checker completion"
        } else {
            "administrator restart before checker completion"
        };
        let replacement = crate::services::ad_engine::prepare_service_reset(
            &st.db,
            job.game_id,
            service.id,
            reason,
        )
        .await?;
        crate::services::control_jobs::checkpoint_input(
            st.pg(),
            job.id,
            claimed.lease_token,
            json!({
                "resetPrepared": true,
                "preparedRoundId": replacement.prepared_round_id,
                "retiredBackendId": replacement.retired_container_id.clone(),
            }),
        )
        .await?;
        crate::services::ad_vpn::deactivate_team_service(&st.db, service.id).await?;
        if let Some(container_id) = &replacement.retired_container_id {
            crate::services::traffic::destroy_container_after_capture_fence(st, container_id)
                .await?;
        }
        (replacement.prepared_round_id, replacement.current_flag)
    };
    let flag = match current_flag {
        Some(flag) => crate::utils::flag_generator::validate_stored_ad_flag(flag)?,
        None => {
            let salt = crate::utils::flag_generator::team_hash_salt(&game.private_key);
            let team_hash = crate::utils::flag_generator::team_challenge_hash(
                &salt,
                challenge.id,
                &part.token,
            );
            crate::utils::flag_generator::generate_retryable_ad_flag(
                &team_hash,
                &job.operation_id.to_string(),
            )?
        }
    };
    let mut spec = ContainerSpec::ad_service(
        image,
        ContainerResourceLimits {
            memory_limit: challenge.memory_limit.unwrap_or(256),
            cpu_count: challenge.cpu_count.unwrap_or(1),
            storage_limit: storage_limit_or_default(challenge.storage_limit),
        },
        challenge.expose_port.unwrap_or(80),
        part.team_id,
        challenge.ad_allow_egress,
        flag,
    );
    spec.operation_id = Some(format!("ad-reset:{}", job.operation_id));
    let info = st.containers.create(spec).await?;
    let backend_id = info.id.clone();
    crate::services::control_jobs::checkpoint_input(
        st.pg(),
        job.id,
        claimed.lease_token,
        json!({ "replacementBackendId": backend_id.clone() }),
    )
    .await?;
    let retained = crate::services::ad::service_lifecycle::retain_created_backend_identity(
        st.pg(),
        job.game_id,
        service.participation_id,
        service.challenge_id,
        &backend_id,
    )
    .await;
    if let Err(error) = retained {
        if let Err(destroy_error) = st.containers.destroy(&backend_id).await {
            tracing::error!(%backend_id, %destroy_error, "failed to destroy unretained reset replacement");
        }
        return Err(error);
    }
    if !retained.expect("retention error returned above") {
        st.containers.destroy(&backend_id).await?;
        return Err(AppError::conflict(
            "Service ownership disappeared while the replacement was launching",
        ));
    }
    let published = crate::services::ad_engine::publish_service_reset(
        &st.db,
        job.game_id,
        service.id,
        &info.ip,
        info.port,
        &backend_id,
        prepared_round_id,
        true,
    )
    .await;
    match published {
        Ok(true) => {}
        Ok(false) => {
            crate::services::ad::service_lifecycle::rollback_created_backend(
                st,
                service.participation_id,
                service.challenge_id,
                &backend_id,
            )
            .await?;
            return Err(AppError::conflict(
                "Service eligibility changed while the replacement was launching",
            ));
        }
        Err(error) => {
            crate::services::ad::service_lifecycle::rollback_created_backend(
                st,
                service.participation_id,
                service.challenge_id,
                &backend_id,
            )
            .await?;
            return Err(error);
        }
    }
    distributed.release().await?;
    if challenge.enable_traffic_capture {
        crate::services::traffic::start_container_capture(st, &backend_id).await?;
    }
    crate::services::ad_vpn::reconcile_for_deployment(&st.db).await?;
    Ok(json!({ "reset": true, "alreadyReplaced": false }))
}

#[cfg(test)]
mod tests {
    use super::{backend_matches, ResetJobInput};

    #[test]
    fn backend_fence_treats_empty_as_absent_and_rejects_replacement() {
        assert!(backend_matches(None, Some("")));
        assert!(backend_matches(Some("old"), Some("old")));
        assert!(!backend_matches(Some("replacement"), Some("old")));
    }

    #[test]
    fn preparation_checkpoint_distinguishes_warmup_from_no_checkpoint() {
        let input: ResetJobInput = serde_json::from_value(serde_json::json!({
            "serviceId": 7,
            "participationId": 9,
            "expectedBackendId": "old",
            "playerPolicy": true,
            "resetPrepared": true,
            "preparedRoundId": null,
            "retiredBackendId": "old"
        }))
        .unwrap();
        assert!(input.reset_prepared);
        assert_eq!(input.prepared_round_id, None);
        assert_eq!(input.retired_backend_id.as_deref(), Some("old"));
    }
}
