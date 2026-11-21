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
    prepared_image_ref: Option<String>,
    #[serde(default)]
    replacement_backend_id: Option<String>,
}

fn backend_matches(actual: Option<&str>, expected: Option<&str>) -> bool {
    actual.filter(|value| !value.is_empty()) == expected.filter(|value| !value.is_empty())
}

impl ResetJobInput {
    fn can_resume(&self, host: &str, port: i32, backend_id: Option<&str>) -> bool {
        if !self.reset_prepared && self.replacement_backend_id.is_none() {
            return backend_matches(backend_id, self.expected_backend_id.as_deref());
        }
        // Teardown clears the old identity before the replacement is retained.
        // A checkpoint can span both states, but never a newly published endpoint.
        let expected = self
            .replacement_backend_id
            .as_deref()
            .or(self.retired_backend_id.as_deref());
        host.is_empty()
            && port == 0
            && (backend_matches(backend_id, None) || backend_matches(backend_id, expected))
    }
}

/// Called under the service publication lock. A reset can have committed its
/// blank endpoint and lost its process before the replacement is published.
/// Its durable checkpoint still owns recovery when its worker lease expires.
pub(super) async fn owns_unpublished_service(
    pool: &sqlx::PgPool,
    service: &ad_team_service::Model,
) -> AppResult<bool> {
    sqlx::query_scalar(
        r#"SELECT EXISTS (SELECT 1 FROM "ControlPlaneJobs"
            WHERE kind='AdReset' AND scope_key=$1 AND status IN (0,1)
              AND game_id=$2 AND challenge_id=$3 AND input @> $4)"#,
    )
    .bind(format!("ad-service:{}", service.id))
    .bind(service.game_id)
    .bind(service.challenge_id)
    .bind(json!({"serviceId":service.id,"participationId":service.participation_id,"resetPrepared":true}))
    .fetch_one(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
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
    let lock_key = crate::services::ad::service_lifecycle::service_lock_key(
        initial.participation_id,
        initial.challenge_id,
    );
    let _local = crate::utils::single_flight::coalesce(&lock_key).await;
    let distributed = crate::services::ad::service_lifecycle::acquire_publication_lock(
        st.pg(),
        job.game_id,
        initial.participation_id,
        initial.challenge_id,
    )
    .await?;

    let service = ad_team_service::Entity::find_by_id(input.service_id)
        .one(&st.db)
        .await?
        .filter(|service| {
            service.game_id == job.game_id && service.participation_id == input.participation_id
        })
        .ok_or_else(|| AppError::not_found("Service not found"))?;
    let replacement_checkpointed = input.replacement_backend_id.is_some();
    let published = input
        .replacement_backend_id
        .as_deref()
        .filter(|&replacement| {
            service.container_id.as_deref() == Some(replacement)
                && !service.host.trim().is_empty()
                && service.port > 0
        });
    let recovering_preparation = input.reset_prepared && input.replacement_backend_id.is_none();
    if published.is_none()
        && !input.can_resume(&service.host, service.port, service.container_id.as_deref())
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
    if let Some(backend_id) = published {
        // The database publication can survive a process exit before the
        // capture owner or the VPN has acknowledged the new endpoint.
        distributed.release().await?;
        finish_publication(st, backend_id, challenge.enable_traffic_capture).await?;
        return Ok(json!({ "reset": true, "alreadyReplaced": true }));
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

    let resuming = replacement_checkpointed || recovering_preparation;
    // A fresh player reset restores the organizer's original starter when
    // one was recorded. Admin restarts keep their existing rebuild behavior.
    // On recovery, the job's own preparation receipt takes precedence.
    let pinned = if resuming {
        input.prepared_image_ref
    } else if input.player_policy {
        sqlx::query_scalar::<_, String>(
            r#"SELECT runtime_image_ref FROM "AdPatchStarters"
                WHERE challenge_id=$1 AND runtime_image_ref IS NOT NULL"#,
        )
        .bind(challenge.id)
        .fetch_optional(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
    } else {
        None
    };
    let image = crate::services::challenge_images::runtime_image_with_pin(
        st,
        &challenge,
        pinned.as_deref(),
    )?;
    let retained_backend = input
        .retired_backend_id
        .as_deref()
        .filter(|retired| service.container_id.as_deref() == Some(*retired));
    // A durable preparation records which image to use. A reclaimed job may
    // run after that image disappeared, while the retired container still
    // holds the team's files. Stage the image again before removing it.
    if !resuming || (recovering_preparation && retained_backend.is_some()) {
        st.containers
            .profile_pull_image(challenge.runtime_profile.as_deref(), &image)
            .await?;
    }
    let (prepared_round_id, current_flag) = if resuming {
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
            crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await?;
            if let Some(retired_backend_id) = retained_backend {
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
            input.expected_backend_id.as_deref(),
            job.id,
            claimed.lease_token,
            &image,
            reason,
        )
        .await?;
        crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await?;
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
            let team_hash =
                crate::utils::flag_generator::team_challenge_hash(&salt, challenge.id, &part.token);
            crate::utils::flag_generator::generate_retryable_ad_flag(
                &team_hash,
                &job.operation_id.to_string(),
            )?
        }
    };
    let mut spec = ContainerSpec::ad_service(
        image.clone(),
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
    spec.runtime_profile = challenge.runtime_profile.clone();
    spec.operation_id = Some(format!("ad-reset:{}", job.operation_id));
    let info = st.containers.create(spec).await?;
    let backend_id = info.id.clone();
    crate::services::ad_engine::checkpoint_reset_replacement(
        st.pg(),
        job.game_id,
        service.id,
        job.id,
        claimed.lease_token,
        input.replacement_backend_id.as_deref(),
        &backend_id,
    )
    .await?;
    let publish = || {
        crate::services::ad_engine::publish_service_reset(
            &st.db,
            job.game_id,
            service.id,
            &info.ip,
            info.port,
            &backend_id,
            prepared_round_id,
            true,
            // A worker from before image checkpointing may already have launched
            // its replacement against a different organizer build.
            (!resuming || pinned.is_some()).then_some(image.as_str()),
        )
    };
    let published = match publish().await {
        Ok(published) => published,
        Err(error) => {
            // A dropped commit response can leave a live endpoint. The service
            // lock still fences competing publishers while we check and retry.
            tracing::warn!(service_id = service.id, %error, "confirming reset publication");
            let committed: bool = sqlx::query_scalar(
                r#"SELECT EXISTS(SELECT 1 FROM "AdTeamServices"
                    WHERE id=$1 AND game_id=$2 AND container_id=$3 AND host=$4 AND port=$5
                      AND NULLIF(BTRIM(host), '') IS NOT NULL AND port BETWEEN 1 AND 65535)"#,
            )
            .bind(service.id)
            .bind(job.game_id)
            .bind(&backend_id)
            .bind(&info.ip)
            .bind(info.port)
            .fetch_one(st.pg())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
            committed || publish().await?
        }
    };
    if !published {
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
    distributed.release().await?;
    finish_publication(st, &backend_id, challenge.enable_traffic_capture).await?;
    Ok(json!({ "reset": true, "alreadyReplaced": false }))
}

async fn finish_publication(st: &SharedState, backend_id: &str, capture: bool) -> AppResult<()> {
    if capture {
        crate::services::traffic::start_container_capture(st, backend_id).await?;
    }
    crate::services::ad_vpn::reconcile_for_deployment(&st.db).await
}

#[cfg(test)]
#[path = "reset_runtime_tests.rs"]
mod runtime_tests;

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
        assert!(input.can_resume("", 0, Some("old")));
        assert!(input.can_resume("", 0, None));
        assert!(!input.can_resume("replacement", 8080, Some("other")));
        assert!(!input.can_resume("", 0, Some("other")));
        assert!(!input.can_resume("byoc", 8080, None));
        let launched = ResetJobInput {
            replacement_backend_id: Some("new".into()),
            ..input
        };
        assert!(launched.can_resume("", 0, Some("new")));
        assert!(launched.can_resume("", 0, None));
        assert!(!launched.can_resume("", 0, Some("old")));
        assert!(!launched.can_resume("other", 8080, Some("other")));
    }
}
