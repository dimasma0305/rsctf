//! End-of-event cleanup for managed A&D and KotH workloads.

use chrono::{Duration, Utc};
use sea_orm::{ActiveModelTrait, EntityTrait, Set};

use crate::{
    app_state::SharedState,
    models::data::{ad_team_service, game, koth_target},
    utils::error::{AppError, AppResult},
};

/// Revoke and destroy A&D/KotH backends once their game window closes. These
/// workloads are not represented by expiring `Containers` rows in every path,
/// so they need an explicit end-of-game lifecycle sweep.
pub(super) async fn reap_ended_backends(state: &SharedState) -> AppResult<u64> {
    let services: Vec<(i32, i32, i32)> = sqlx::query_as(
        r#"SELECT service.id, service.participation_id, service.challenge_id
             FROM "AdTeamServices" service
             JOIN "Games" game ON game.id = service.game_id
            WHERE game.end_time_utc <= now() - ($1 * interval '1 second')
              AND (service.container_id IS NOT NULL OR service.host <> '')
            ORDER BY service.id"#,
    )
    .bind(super::ADVANCE_BUDGET_SECS as i64)
    .fetch_all(state.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let mut reaped = 0u64;
    for (service_id, participation_id, challenge_id) in services {
        let key = format!("ad-service:{participation_id}:{challenge_id}");
        let _local = crate::utils::single_flight::coalesce(&key).await;
        let distributed =
            crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(state.pg(), &key)
                .await?;
        let row = ad_team_service::Entity::find_by_id(service_id)
            .one(&state.db)
            .await?;
        if let Some(row) = row {
            let ended = game::Entity::find_by_id(row.game_id)
                .one(&state.db)
                .await?
                .is_none_or(|game| {
                    game.end_time_utc
                        <= Utc::now() - Duration::seconds(super::ADVANCE_BUDGET_SECS as i64)
                });
            if ended && (row.container_id.is_some() || !row.host.is_empty()) {
                let backend_id = row.container_id.clone();
                if let Some(backend_id) = backend_id.as_deref() {
                    if let Err(error) = crate::services::ad::snapshots::capture_final_service(
                        state, row.id, backend_id,
                    )
                    .await
                    {
                        tracing::warn!(
                            service_id = row.id,
                            %backend_id,
                            %error,
                            "cron: final A&D snapshot capture failed; retaining backend for retry"
                        );
                        distributed.release().await?;
                        continue;
                    }
                }
                crate::services::ad_vpn::deactivate_team_service(&state.db, row.id).await?;
                let cleaned = if let Some(backend_id) = backend_id {
                    match crate::services::traffic::destroy_container_after_capture_fence(
                        state,
                        &backend_id,
                    )
                    .await
                    {
                        Ok(()) => true,
                        Err(error) => {
                            tracing::warn!(
                                service_id = row.id,
                                %backend_id,
                                %error,
                                "cron: ended A&D service destroy failed; retaining identity for retry"
                            );
                            false
                        }
                    }
                } else {
                    true
                };
                reaped += u64::from(cleaned);
            }
        }
        distributed.release().await?;
    }

    let targets: Vec<(i32, i32)> = sqlx::query_as(
        r#"SELECT target.id, target.challenge_id
             FROM "KothTargets" target
             JOIN "Games" game ON game.id = target.game_id
            WHERE game.end_time_utc <= now() - ($1 * interval '1 second')
              AND (target.container_id IS NOT NULL OR target.host <> '')
            ORDER BY target.id"#,
    )
    .bind(super::ADVANCE_BUDGET_SECS as i64)
    .fetch_all(state.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    for (target_id, challenge_id) in targets {
        let key = format!("shared-container:{challenge_id}");
        let _local = crate::utils::single_flight::coalesce(&key).await;
        let distributed =
            crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(state.pg(), &key)
                .await?;
        let target = koth_target::Entity::find_by_id(target_id)
            .one(&state.db)
            .await?;
        if let Some(target) = target {
            let ended = game::Entity::find_by_id(target.game_id)
                .one(&state.db)
                .await?
                .is_none_or(|game| {
                    game.end_time_utc
                        <= Utc::now() - Duration::seconds(super::ADVANCE_BUDGET_SECS as i64)
                });
            if ended && (target.container_id.is_some() || !target.host.is_empty()) {
                let backend_id = target.container_id.clone();
                let mut active: koth_target::ActiveModel = target.into();
                active.host = Set(String::new());
                active.port = Set(0);
                active.holder_participation_id = Set(None);
                active.held_since = Set(None);
                active.update(&state.db).await?;
                crate::services::ad_vpn::ensure_hub_and_sync(&state.db).await?;
                let mut cleaned = backend_id.is_none();
                if let Some(backend_id) = backend_id {
                    match state.containers.destroy(&backend_id).await {
                        Ok(()) => {
                            sqlx::query(
                                r#"UPDATE "KothTargets" SET container_id = NULL
                                    WHERE id = $1 AND container_id = $2"#,
                            )
                            .bind(target_id)
                            .bind(&backend_id)
                            .execute(state.pg())
                            .await
                            .map_err(|error| AppError::internal(error.to_string()))?;
                            cleaned = true;
                        }
                        Err(error) => tracing::warn!(
                            target = target_id,
                            backend_id,
                            %error,
                            "cron: ended KotH backend destroy failed; retaining id for retry"
                        ),
                    }
                }
                reaped += u64::from(cleaned);
            }
        }
        distributed.release().await?;
    }
    Ok(reaped)
}
