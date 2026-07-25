//! Final hosted A&D service snapshot capture and retention policy.

use chrono::{DateTime, Duration, Utc};

use crate::app_state::SharedState;
use crate::services::container::ContainerBackendKind;
use crate::utils::error::{AppError, AppResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FinalSnapshotStatus {
    Available,
    NotRequired,
}

#[derive(Debug, sqlx::FromRow)]
struct SnapshotPolicy {
    participation_id: i32,
    challenge_id: i32,
    container_id: Option<String>,
    ad_self_hosted: bool,
    ad_allow_snapshot_download: bool,
    end_time_utc: DateTime<Utc>,
    ad_snapshot_retention_days: Option<i32>,
}

fn expiry(policy: &SnapshotPolicy) -> Option<DateTime<Utc>> {
    policy
        .ad_snapshot_retention_days
        .map(|days| policy.end_time_utc + Duration::days(i64::from(days)))
}

fn capture_supported(backend: ContainerBackendKind) -> bool {
    backend == ContainerBackendKind::Docker
}

/// Persist the exact final filesystem before its runtime identity is released.
///
/// Snapshot capture is intentionally restricted to Docker-backed hosted A&D
/// services: Docker exposes an engine-level filesystem export that does not
/// depend on utilities inside an untrusted challenge image. Kubernetes has no
/// equivalent portable API, so retaining a pod there would never make capture
/// succeed and would turn event teardown into a permanent resource leak.
pub(crate) async fn capture_final_service(
    state: &SharedState,
    team_service_id: i32,
    expected_container_id: &str,
) -> AppResult<FinalSnapshotStatus> {
    if crate::services::blob_refs::load_service_snapshot(state.pg(), team_service_id)
        .await?
        .is_some()
    {
        return Ok(FinalSnapshotStatus::Available);
    }

    let policy = sqlx::query_as::<_, SnapshotPolicy>(
        r#"SELECT service.participation_id, service.challenge_id,
                  service.container_id, challenge.ad_self_hosted,
                  game.ad_allow_snapshot_download, game.end_time_utc,
                  game.ad_snapshot_retention_days
             FROM "AdTeamServices" service
             JOIN "GameChallenges" challenge
               ON challenge.id = service.challenge_id
              AND challenge.game_id = service.game_id
             JOIN "Games" game ON game.id = service.game_id
            WHERE service.id = $1"#,
    )
    .bind(team_service_id)
    .fetch_optional(state.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("A&D team service not found"))?;

    if policy.ad_self_hosted
        || !policy.ad_allow_snapshot_download
        || !capture_supported(state.containers.backend_kind())
    {
        return Ok(FinalSnapshotStatus::NotRequired);
    }
    let now = Utc::now();
    if now < policy.end_time_utc {
        return Err(AppError::conflict(
            "A&D service snapshot cannot be captured before event end",
        ));
    }
    let expires_at_utc = expiry(&policy);
    if expires_at_utc.is_some_and(|expires| expires <= now) {
        return Ok(FinalSnapshotStatus::NotRequired);
    }
    if policy.container_id.as_deref() != Some(expected_container_id) {
        return Err(AppError::conflict(
            "A&D service backend changed before final snapshot capture",
        ));
    }

    let tar = state.containers.export(expected_container_id).await?;
    let name = format!(
        "ad-snapshot-team{}-challenge{}.tar",
        policy.participation_id, policy.challenge_id
    );
    let stored = crate::services::blob_refs::store_service_snapshot(
        state.pg(),
        state.storage.as_ref(),
        team_service_id,
        expected_container_id,
        expires_at_utc,
        &name,
        &tar,
    )
    .await?;
    Ok(if stored.is_some() {
        FinalSnapshotStatus::Available
    } else {
        FinalSnapshotStatus::NotRequired
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(retention: Option<i32>) -> SnapshotPolicy {
        SnapshotPolicy {
            participation_id: 1,
            challenge_id: 2,
            container_id: Some("runtime".to_string()),
            ad_self_hosted: false,
            ad_allow_snapshot_download: true,
            end_time_utc: DateTime::from_timestamp(1_800_000_000, 0).unwrap(),
            ad_snapshot_retention_days: retention,
        }
    }

    #[test]
    fn retention_is_anchored_to_event_end_and_null_is_forever() {
        assert_eq!(
            expiry(&policy(Some(7))),
            Some(policy(Some(7)).end_time_utc + Duration::days(7))
        );
        assert_eq!(expiry(&policy(None)), None);
    }

    #[test]
    fn only_engine_level_docker_export_is_claimed_as_supported() {
        assert!(capture_supported(ContainerBackendKind::Docker));
        assert!(!capture_supported(ContainerBackendKind::Kubernetes));
        assert!(!capture_supported(ContainerBackendKind::Worker));
        assert!(!capture_supported(ContainerBackendKind::None));
    }
}
