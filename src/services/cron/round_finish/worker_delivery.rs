use std::time::Duration;

use rsctf_worker_protocol::{FlagTarget, SessionFence, WorkloadFence, WriteFlag};
use sqlx::{types::Json, FromRow, PgPool};
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::services::ad::service_lifecycle::service_lock_key;
use crate::services::ad_engine::AdvancedRoundFlag;
use crate::services::worker::WorkerHandle;
use crate::utils::enums::{ChallengeReviewStatus, ChallengeType, ParticipationStatus};

#[cfg(test)]
mod tests;

#[derive(FromRow, PartialEq)]
struct Binding {
    worker_id: Uuid,
    session_id: Uuid,
    session_epoch: i64,
    target: Json<FlagTarget>,
    official_flag: String,
}

/// Resolve the official value, the published team endpoint, the worker's owner
/// record and the currently observed generation in the same database snapshot.
async fn binding(
    pool: &PgPool,
    round_id: i32,
    planted: &AdvancedRoundFlag,
    handle: WorkerHandle,
) -> Result<Option<Binding>, sqlx::Error> {
    let selected = sqlx::query_as::<_, Binding>(
        r#"SELECT node.id AS worker_id, node.session_id, node.session_epoch,
                  work.spec -> 'flagTarget' AS target, flag.flag AS official_flag
             FROM "AdTeamServices" service
             JOIN "AdFlags" flag ON flag.team_service_id = service.id AND flag.round_id = $1
             JOIN "AdRounds" round ON round.id = flag.round_id AND round.game_id = service.game_id
             JOIN "Games" game ON game.id = service.game_id AND NOT game.deletion_pending
                   AND NOT game.ad_scoring_paused AND game.start_time_utc <= clock_timestamp()
                   AND game.end_time_utc > clock_timestamp()
             JOIN "Participations" participant ON participant.id = service.participation_id
                   AND participant.game_id = service.game_id AND participant.status = $9
             JOIN "Teams" team ON team.id = participant.team_id AND NOT team.deletion_pending
             JOIN "GameChallenges" challenge ON challenge.id = service.challenge_id
                   AND challenge.game_id = service.game_id AND challenge.is_enabled
                   AND challenge.review_status = $10 AND challenge."Type" = $11
                   AND NOT challenge.ad_self_hosted AND NOT challenge.deletion_pending
             JOIN "WorkerWorkloads" work ON work.id = $5 AND work.assignment_id = $6
                   AND work.generation = $7 AND work.owner_kind = 'ad-team-service'
                   AND work.owner_key = $8 AND work.desired_state = 'Present'
                   AND work.observed_state = 'Ready' AND work.spec ->> 'gameKind' = 'attackDefense'
                   AND jsonb_typeof(work.spec -> 'flagTarget') = 'object'
             JOIN "WorkerNodes" node ON node.id = work.worker_id
                   AND node.session_epoch = work.observed_session_epoch
                   AND node.session_id IS NOT NULL AND node.administrative_state <> 'Disabled'
                   AND node.lease_expires_at > clock_timestamp()
                   AND node.certificate_expires_at > clock_timestamp()
                   AND node.capabilities @> '{"verifiedFlagDelivery":true,"writeFlag":true}'::jsonb
                   AND (work.spec ->> 'teamDataMount' IS NULL
                        OR node.capabilities @> '{"teamDataMount":true}'::jsonb)
            WHERE service.id = $2 AND service.participation_id = $3 AND service.challenge_id = $4
              AND service.container_id = $12 AND NOT round.finalized
              AND round.pipeline_completed_at IS NULL AND round.end_time_utc > clock_timestamp()
              AND round.id = (SELECT id FROM "AdRounds" WHERE game_id = service.game_id
                               ORDER BY number DESC, id DESC LIMIT 1)"#,
    )
    .bind(round_id)
    .bind(planted.team_service_id)
    .bind(planted.participation_id)
    .bind(planted.challenge_id)
    .bind(handle.workload_id)
    .bind(handle.assignment_id)
    .bind(handle.generation)
    .bind(service_lock_key(
        planted.participation_id,
        planted.challenge_id,
    ))
    .bind(ParticipationStatus::Accepted as i16)
    .bind(ChallengeReviewStatus::Active as i16)
    .bind(ChallengeType::AttackDefense as i16)
    .bind(handle.encode())
    .fetch_optional(pool)
    .await?;
    Ok(selected.filter(|binding| binding.official_flag == planted.flag))
}

pub(super) async fn attempt(
    state: &SharedState,
    round_id: i32,
    planted: &AdvancedRoundFlag,
    handle: WorkerHandle,
    timeout: Duration,
    tracker: &super::DeliveryAttemptTracker,
) -> super::DeliveryAttempt {
    if deliver(state, round_id, planted, handle, timeout).await {
        tracker.participant_completed(planted.team_service_id);
        super::DeliveryAttempt::Delivered
    } else {
        // A missing worker receipt alone is insufficient evidence for a team penalty.
        super::DeliveryAttempt::PlatformFailure
    }
}

async fn deliver(
    state: &SharedState,
    round_id: i32,
    planted: &AdvancedRoundFlag,
    handle: WorkerHandle,
    timeout: Duration,
) -> bool {
    let Some(workers) = &state.workers else {
        return false;
    };
    let Ok(Some(before)) = binding(state.pg(), round_id, planted, handle).await else {
        return false;
    };
    let Ok(session_epoch) = u64::try_from(before.session_epoch) else {
        return false;
    };
    let Ok(flag_sequence) = u64::try_from(round_id) else {
        return false;
    };
    let session = SessionFence {
        session_id: before.session_id,
        session_epoch,
    };
    let write = WriteFlag {
        command_id: Uuid::new_v4(),
        fence: WorkloadFence {
            workload_id: handle.workload_id,
            assignment_id: handle.assignment_id,
            generation: handle.generation as u64,
        },
        flag_sequence,
        target: before.target.0.clone(),
        value: before.official_flag.clone(),
        timeout_ms: timeout.as_millis() as u64,
    };
    if workers
        .write_flag_confirmed(before.worker_id, &session, write)
        .await
        .is_err()
    {
        return false;
    }
    // A team reset, a new round or revoked membership invalidates a late receipt.
    matches!(binding(state.pg(), round_id, planted, handle).await, Ok(Some(after)) if before == after)
}
