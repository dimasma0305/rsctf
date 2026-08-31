use super::*;
use sha2::{Digest, Sha256};
use std::sync::{Arc, LazyLock};

#[path = "bulk_desired.rs"]
mod desired;
use desired::{
    claim_desired_state_operation, complete_desired_state, expire_desired_state_lease,
    recover_desired_state_jobs, spawn_desired_state_job_with_permit,
};

const MAX_BULK_CHALLENGES: usize = 100;
const MAX_ACTIVE_BULK_OPERATIONS: i64 = 64;
const BULK_DELETE_CONCURRENCY: usize = 2;
const BULK_DESIRED_STATE_CONCURRENCY: usize = 4;
const BULK_DELETE_STEP_BUDGET: std::time::Duration = std::time::Duration::from_secs(4 * 60);
static BULK_DELETE_SLOTS: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(BULK_DELETE_CONCURRENCY)));
static BULK_DESIRED_STATE_SLOTS: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(BULK_DESIRED_STATE_CONCURRENCY)));

const EFFECT_VPN_RECONCILED: i16 = 1;
const EFFECT_SCOREBOARDS_FLUSHED: i16 = 2;
const EFFECT_NOTICE_PUBLISHED: i16 = 4;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct DesiredRuntimeEffect {
    challenge_id: i32,
    challenge_type: i16,
    ad_self_hosted: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct DesiredStateEffects {
    changed_runtimes: Vec<DesiredRuntimeEffect>,
    notice_id: Option<i32>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum BulkChallengeAction {
    Enable,
    Disable,
    Delete,
}

impl BulkChallengeAction {
    fn as_i16(self) -> i16 {
        match self {
            Self::Enable => 0,
            Self::Disable => 1,
            Self::Delete => 2,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkChallengeMutationRequest {
    pub operation_id: Uuid,
    pub expected_revision: i64,
    pub action: BulkChallengeAction,
    pub challenge_ids: Vec<i32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkChallengeOutcome {
    pub challenge_id: i32,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkChallengeMutationResult {
    pub operation_id: Uuid,
    pub state: &'static str,
    pub configuration_revision: i64,
    pub outcomes: Vec<BulkChallengeOutcome>,
}

#[derive(sqlx::FromRow)]
struct SelectedChallenge {
    id: i32,
    title: String,
    challenge_type: i16,
    is_enabled: bool,
    deletion_pending: bool,
    review_status: i16,
    has_flag: bool,
    has_invalid_flag: bool,
    ad_self_hosted: bool,
}

fn validate_request(request: &mut BulkChallengeMutationRequest) -> AppResult<()> {
    if request.operation_id.is_nil() || request.expected_revision < 1 {
        return Err(AppError::bad_request(
            "Bulk challenge mutation requires an operation ID and observed revision",
        ));
    }
    if request.challenge_ids.is_empty() || request.challenge_ids.len() > MAX_BULK_CHALLENGES {
        return Err(AppError::payload_too_large(format!(
            "Select 1 to {MAX_BULK_CHALLENGES} challenges"
        )));
    }
    if request.challenge_ids.iter().any(|id| *id <= 0) {
        return Err(AppError::bad_request("Challenge IDs must be positive"));
    }
    request.challenge_ids.sort_unstable();
    if request
        .challenge_ids
        .windows(2)
        .any(|pair| pair[0] == pair[1])
    {
        return Err(AppError::bad_request(
            "Duplicate challenge IDs are not allowed",
        ));
    }
    Ok(())
}

async fn reserve_operation(
    st: &SharedState,
    actor_user_id: Uuid,
    game_id: i32,
    request: &BulkChallengeMutationRequest,
    digest: &[u8],
) -> AppResult<(i16, Vec<BulkChallengeOutcome>, Option<i64>)> {
    let mut transaction = st
        .pg()
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let admission_owner: bool = sqlx::query_scalar(
        "SELECT pg_try_advisory_xact_lock(hashtextextended('rsctf:bulk-challenge-admission', 0))",
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !admission_owner {
        return Err(AppError::too_many_requests(1));
    }
    let mut row = sqlx::query_as::<_, (Uuid, Vec<u8>, i16, serde_json::Value, Option<i64>)>(
        r#"SELECT actor_user_id, request_digest, state, result, result_revision
             FROM "BulkChallengeMutationOperations"
            WHERE game_id = $1 AND operation_id = $2"#,
    )
    .bind(game_id)
    .bind(request.operation_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if row.is_none() {
        let active = sqlx::query_scalar::<_, i64>(
            r#"SELECT COUNT(*)::bigint FROM "BulkChallengeMutationOperations"
                WHERE state IN (0, 1)"#,
        )
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if active >= MAX_ACTIVE_BULK_OPERATIONS {
            return Err(AppError::too_many_requests(1));
        }
        sqlx::query(
            r#"INSERT INTO "BulkChallengeMutationOperations"
             (game_id, operation_id, actor_user_id, expected_revision, action,
              challenge_ids, request_digest)
           VALUES ($1, $2, $3, $4, $5, $6, $7)
           ON CONFLICT (game_id, operation_id) DO NOTHING"#,
        )
        .bind(game_id)
        .bind(request.operation_id)
        .bind(actor_user_id)
        .bind(request.expected_revision)
        .bind(request.action.as_i16())
        .bind(&request.challenge_ids)
        .bind(digest)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        row = sqlx::query_as::<_, (Uuid, Vec<u8>, i16, serde_json::Value, Option<i64>)>(
            r#"SELECT actor_user_id, request_digest, state, result, result_revision
                 FROM "BulkChallengeMutationOperations"
                WHERE game_id = $1 AND operation_id = $2"#,
        )
        .bind(game_id)
        .bind(request.operation_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    let row = row.ok_or_else(|| AppError::internal("Bulk operation reservation disappeared"))?;
    if row.0 != actor_user_id || row.1 != digest {
        return Err(AppError::conflict(
            "The operation ID is already bound to another bulk mutation",
        ));
    }
    let outcomes = serde_json::from_value(row.3)
        .map_err(|error| AppError::internal(format!("Invalid bulk mutation result: {error}")))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok((row.2, outcomes, row.4))
}

async fn abandon_operation(st: &SharedState, game_id: i32, operation_id: Uuid) {
    let _ = sqlx::query(
        r#"DELETE FROM "BulkChallengeMutationOperations"
            WHERE game_id = $1 AND operation_id = $2 AND state = 0"#,
    )
    .bind(game_id)
    .bind(operation_id)
    .execute(st.pg())
    .await;
}

async fn abandon_claimed_operation(
    st: &SharedState,
    game_id: i32,
    operation_id: Uuid,
    lease_token: Uuid,
) {
    let _ = sqlx::query(
        r#"WITH removed AS (
               DELETE FROM "BulkChallengeMutationOperations"
                WHERE game_id = $1 AND operation_id = $2 AND state = 1
                  AND lease_token = $3 AND result_revision IS NULL
              RETURNING 1
           )
           UPDATE "BulkChallengeDesiredStateSlots"
              SET lease_token = NULL, expires_at_utc = NULL
            WHERE lease_token = $3 AND EXISTS (SELECT 1 FROM removed)"#,
    )
    .bind(game_id)
    .bind(operation_id)
    .bind(lease_token)
    .execute(st.pg())
    .await;
}

async fn reclaim_delete_operation(
    pool: &sqlx::PgPool,
    game_id: i32,
    operation_id: Uuid,
) -> AppResult<Option<Uuid>> {
    let lease_token = Uuid::new_v4();
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if !claim_delete_slot(&mut transaction, lease_token).await? {
        transaction
            .rollback()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(None);
    }
    let claimed = sqlx::query_scalar::<_, Uuid>(
        r#"UPDATE "BulkChallengeMutationOperations"
              SET lease_token = $3,
                  lease_expires_at_utc = clock_timestamp() + INTERVAL '5 minutes'
            WHERE game_id = $1 AND operation_id = $2 AND state = 1 AND action = 2
              AND lease_expires_at_utc <= clock_timestamp()
          RETURNING lease_token"#,
    )
    .bind(game_id)
    .bind(operation_id)
    .bind(lease_token)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if claimed.is_none() {
        transaction
            .rollback()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(None);
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(claimed)
}

async fn claim_delete_slot(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    lease_token: Uuid,
) -> AppResult<bool> {
    let claimed = sqlx::query_scalar::<_, i16>(
        r#"WITH candidate AS (
               SELECT slot_id FROM "BulkChallengeDeletionSlots"
                WHERE lease_token IS NULL OR expires_at_utc <= clock_timestamp()
                ORDER BY slot_id FOR UPDATE SKIP LOCKED LIMIT 1
           )
           UPDATE "BulkChallengeDeletionSlots" slot
              SET lease_token = $1,
                  expires_at_utc = clock_timestamp() + INTERVAL '5 minutes'
             FROM candidate
            WHERE slot.slot_id = candidate.slot_id
           RETURNING slot.slot_id"#,
    )
    .bind(lease_token)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(claimed.is_some())
}

async fn expire_delete_lease(
    pool: &sqlx::PgPool,
    game_id: i32,
    operation_id: Uuid,
    lease_token: Uuid,
) -> AppResult<()> {
    sqlx::query(
        r#"WITH operation AS (
               UPDATE "BulkChallengeMutationOperations"
                  SET lease_expires_at_utc = clock_timestamp()
                WHERE game_id = $1 AND operation_id = $2 AND state = 1
                  AND lease_token = $3
           )
           UPDATE "BulkChallengeDeletionSlots"
              SET lease_token = NULL, expires_at_utc = NULL
            WHERE lease_token = $3"#,
    )
    .bind(game_id)
    .bind(operation_id)
    .bind(lease_token)
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

async fn renew_delete_lease(
    pool: &sqlx::PgPool,
    game_id: i32,
    operation_id: Uuid,
    lease_token: Uuid,
) -> AppResult<bool> {
    let renewed = sqlx::query_scalar::<_, i64>(
        r#"WITH slot AS (
               UPDATE "BulkChallengeDeletionSlots"
                  SET expires_at_utc = clock_timestamp() + INTERVAL '5 minutes'
                WHERE lease_token = $3
              RETURNING 1
           ), operation AS (
               UPDATE "BulkChallengeMutationOperations"
                  SET lease_expires_at_utc = clock_timestamp() + INTERVAL '5 minutes'
                WHERE game_id = $1 AND operation_id = $2 AND state = 1
                  AND lease_token = $3 AND EXISTS (SELECT 1 FROM slot)
              RETURNING 1
           ) SELECT COUNT(*)::bigint FROM operation"#,
    )
    .bind(game_id)
    .bind(operation_id)
    .bind(lease_token)
    .fetch_one(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(renewed == 1)
}

async fn record_delete_effect(
    pool: &sqlx::PgPool,
    game_id: i32,
    operation_id: Uuid,
    lease_token: Uuid,
    effect: i16,
) -> AppResult<bool> {
    let recorded = sqlx::query_scalar::<_, i64>(
        r#"WITH slot AS (
               UPDATE "BulkChallengeDeletionSlots"
                  SET expires_at_utc = clock_timestamp() + INTERVAL '5 minutes'
                WHERE lease_token = $3
              RETURNING 1
           ), operation AS (
               UPDATE "BulkChallengeMutationOperations"
                  SET effect_progress = effect_progress | $4,
                      lease_expires_at_utc = clock_timestamp() + INTERVAL '5 minutes'
                WHERE game_id = $1 AND operation_id = $2 AND state = 1
                  AND lease_token = $3 AND EXISTS (SELECT 1 FROM slot)
              RETURNING 1
           ) SELECT COUNT(*)::bigint FROM operation"#,
    )
    .bind(game_id)
    .bind(operation_id)
    .bind(lease_token)
    .bind(effect)
    .fetch_one(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(recorded == 1)
}

async fn cleanup_operations(st: &SharedState) {
    let result = sqlx::query(
        r#"WITH expired AS (
               SELECT game_id, operation_id
                 FROM "BulkChallengeMutationOperations"
                WHERE state = 2
                  AND completed_at_utc < clock_timestamp() - INTERVAL '30 days'
                ORDER BY completed_at_utc, game_id, operation_id
                LIMIT 128
           )
           DELETE FROM "BulkChallengeMutationOperations" operation
            USING expired
            WHERE operation.game_id = expired.game_id
              AND operation.operation_id = expired.operation_id"#,
    )
    .execute(st.pg())
    .await;
    if let Err(error) = result {
        tracing::warn!(%error, "bulk challenge operation retention cleanup deferred");
    }
}

async fn validate_delete_job(
    st: &SharedState,
    game_id: i32,
    request: &BulkChallengeMutationRequest,
) -> AppResult<(i64, Option<Uuid>)> {
    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, game_id).await?;
    let revision = sqlx::query_scalar::<_, i64>(
        r#"SELECT challenge_configuration_revision FROM "Games"
            WHERE id = $1 FOR UPDATE"#,
    )
    .bind(game_id)
    .fetch_optional(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Game not found"))?;
    let operation_state = sqlx::query_scalar::<_, i16>(
        r#"SELECT state FROM "BulkChallengeMutationOperations"
            WHERE game_id = $1 AND operation_id = $2 FOR UPDATE"#,
    )
    .bind(game_id)
    .bind(request.operation_id)
    .fetch_one(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if operation_state != 0 {
        control
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok((revision, None));
    }
    if revision != request.expected_revision {
        drop(control);
        abandon_operation(st, game_id, request.operation_id).await;
        return Err(AppError::conflict(format!(
            "Challenge configuration changed; current revision is {revision}"
        )));
    }
    let count = sqlx::query_scalar::<_, i64>(
        r#"SELECT COUNT(*)::bigint FROM "GameChallenges"
            WHERE game_id = $1 AND id = ANY($2)"#,
    )
    .bind(game_id)
    .bind(&request.challenge_ids)
    .fetch_one(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if count != request.challenge_ids.len() as i64 {
        drop(control);
        abandon_operation(st, game_id, request.operation_id).await;
        return Err(AppError::bad_request(
            "Every selected challenge must belong to this event",
        ));
    }
    let lease_token = Uuid::new_v4();
    let claimed = sqlx::query(
        r#"UPDATE "BulkChallengeMutationOperations"
              SET state = 1, lease_token = $3,
                  lease_expires_at_utc = clock_timestamp() + INTERVAL '5 minutes'
            WHERE game_id = $1 AND operation_id = $2 AND state = 0"#,
    )
    .bind(game_id)
    .bind(request.operation_id)
    .bind(lease_token)
    .execute(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let slot_claimed = if claimed.rows_affected() == 1 {
        claim_delete_slot(control.transaction_mut(), lease_token).await?
    } else {
        false
    };
    if claimed.rows_affected() == 1 && !slot_claimed {
        sqlx::query(
            r#"UPDATE "BulkChallengeMutationOperations"
                  SET lease_expires_at_utc = clock_timestamp()
                WHERE game_id = $1 AND operation_id = $2 AND lease_token = $3"#,
        )
        .bind(game_id)
        .bind(request.operation_id)
        .bind(lease_token)
        .execute(&mut **control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok((revision, slot_claimed.then_some(lease_token)))
}

fn spawn_delete_job_with_permit(
    st: SharedState,
    game_id: i32,
    operation_id: Uuid,
    lease_token: Uuid,
    permit: tokio::sync::OwnedSemaphorePermit,
) {
    tokio::spawn(async move {
        let _permit = permit;
        if let Err(error) = run_delete_job(&st, game_id, operation_id, lease_token).await {
            tracing::error!(%error, game_id, %operation_id, "bulk challenge deletion paused");
            if let Err(expire_error) =
                expire_delete_lease(st.pg(), game_id, operation_id, lease_token).await
            {
                tracing::warn!(%expire_error, game_id, %operation_id, "bulk deletion lease expiry deferred");
            }
        }
    });
}

async fn schedule_delete_job(
    st: &SharedState,
    game_id: i32,
    operation_id: Uuid,
    lease_token: Uuid,
) -> AppResult<bool> {
    let Ok(permit) = BULK_DELETE_SLOTS.clone().try_acquire_owned() else {
        // Do not create one waiting Tokio task per accepted request. Make this
        // lease immediately recoverable by the bounded cron dispatcher.
        expire_delete_lease(st.pg(), game_id, operation_id, lease_token).await?;
        return Ok(false);
    };
    spawn_delete_job_with_permit(st.clone(), game_id, operation_id, lease_token, permit);
    Ok(true)
}

/// Recover at most the locally available number of expired deletion workers.
/// The row claim is replica-safe; the process semaphore bounds live teardown
/// tasks without retaining one waiter per accepted HTTP request.
pub(crate) async fn recover_delete_jobs(st: &SharedState) -> AppResult<u64> {
    let mut started = recover_desired_state_jobs(st).await?;
    loop {
        let Ok(permit) = BULK_DELETE_SLOTS.clone().try_acquire_owned() else {
            break;
        };
        let pending = sqlx::query_as::<_, (i32, Uuid, i64, Vec<i32>)>(
            r#"SELECT game_id, operation_id, expected_revision, challenge_ids
                 FROM "BulkChallengeMutationOperations"
                WHERE state = 0 AND action = 2
                  AND lease_expires_at_utc <= clock_timestamp()
                ORDER BY lease_expires_at_utc, game_id, operation_id
                LIMIT 1"#,
        )
        .fetch_optional(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if let Some((game_id, operation_id, expected_revision, challenge_ids)) = pending {
            let request = BulkChallengeMutationRequest {
                operation_id,
                expected_revision,
                action: BulkChallengeAction::Delete,
                challenge_ids,
            };
            match validate_delete_job(st, game_id, &request).await {
                Ok((_, Some(lease_token))) => {
                    spawn_delete_job_with_permit(
                        st.clone(),
                        game_id,
                        operation_id,
                        lease_token,
                        permit,
                    );
                    started = started.saturating_add(1);
                }
                Ok((_, None)) => drop(permit),
                Err(error) => {
                    tracing::warn!(
                        %error,
                        game_id,
                        %operation_id,
                        "abandoned bulk deletion failed recovery validation"
                    );
                    drop(permit);
                    break;
                }
            }
            continue;
        }
        let lease_token = Uuid::new_v4();
        let mut transaction = st
            .pg()
            .begin()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        let candidate = sqlx::query_as::<_, (i32, Uuid)>(
            r#"SELECT game_id, operation_id
                 FROM "BulkChallengeMutationOperations"
                WHERE state = 1 AND action = 2
                  AND lease_expires_at_utc <= clock_timestamp()
                ORDER BY lease_expires_at_utc, game_id, operation_id
                FOR UPDATE SKIP LOCKED
                LIMIT 1"#,
        )
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        let claimed = if let Some((game_id, operation_id)) = candidate {
            if claim_delete_slot(&mut transaction, lease_token).await? {
                sqlx::query_as::<_, (i32, Uuid)>(
                    r#"UPDATE "BulkChallengeMutationOperations"
                          SET lease_token = $3,
                              lease_expires_at_utc = clock_timestamp() + INTERVAL '5 minutes'
                        WHERE game_id = $1 AND operation_id = $2 AND state = 1
                          AND action = 2 AND lease_expires_at_utc <= clock_timestamp()
                      RETURNING game_id, operation_id"#,
                )
                .bind(game_id)
                .bind(operation_id)
                .bind(lease_token)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(|error| AppError::internal(error.to_string()))?
            } else {
                None
            }
        } else {
            None
        };
        let Some((game_id, operation_id)) = claimed else {
            transaction
                .rollback()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            drop(permit);
            break;
        };
        transaction
            .commit()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        spawn_delete_job_with_permit(st.clone(), game_id, operation_id, lease_token, permit);
        started = started.saturating_add(1);
    }
    Ok(started)
}

async fn run_delete_job(
    st: &SharedState,
    game_id: i32,
    operation_id: Uuid,
    lease_token: Uuid,
) -> AppResult<()> {
    let operation: Option<(Vec<i32>, serde_json::Value, i16)> = sqlx::query_as(
        r#"SELECT challenge_ids, result, effect_progress
             FROM "BulkChallengeMutationOperations"
            WHERE game_id = $1 AND operation_id = $2 AND state = 1
              AND lease_token = $3"#,
    )
    .bind(game_id)
    .bind(operation_id)
    .bind(lease_token)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((challenge_ids, completed, mut effect_progress)) = operation else {
        return Ok(());
    };
    let mut outcomes: Vec<BulkChallengeOutcome> =
        serde_json::from_value(completed).map_err(|error| AppError::internal(error.to_string()))?;
    let completed_ids = outcomes
        .iter()
        .map(|row| row.challenge_id)
        .collect::<std::collections::HashSet<_>>();
    for challenge_id in challenge_ids {
        if completed_ids.contains(&challenge_id) {
            continue;
        }
        // Renew immediately before each irreversible child teardown. A cron
        // reclaimer and this CAS serialize on the operation row: only the
        // current token may enter the bounded external deletion window, and
        // that window is shorter than the five-minute lease.
        if !renew_delete_lease(st.pg(), game_id, operation_id, lease_token).await? {
            return Ok(());
        }
        let deletion = tokio::time::timeout(
            BULK_DELETE_STEP_BUDGET,
            super::delete_challenge_core(st.clone(), game_id, challenge_id, false, false),
        )
        .await
        .map_err(|_| {
            AppError::unavailable(
                "Bulk challenge deletion step timed out and will resume from durable progress",
            )
        })?;
        let outcome = match deletion {
            Ok(_) => BulkChallengeOutcome {
                challenge_id,
                status: "Deleted".into(),
                message: None,
            },
            Err(error) if error.status() == axum::http::StatusCode::NOT_FOUND => {
                BulkChallengeOutcome {
                    challenge_id,
                    status: "Deleted".into(),
                    message: Some("Deletion was already completed".into()),
                }
            }
            Err(error) if error.status().is_server_error() => return Err(error),
            Err(error) => BulkChallengeOutcome {
                challenge_id,
                status: "Rejected".into(),
                message: Some(error.to_string()),
            },
        };
        outcomes.push(outcome);
        let result = serde_json::to_value(&outcomes)
            .map_err(|error| AppError::internal(error.to_string()))?;
        let progress = sqlx::query_scalar::<_, i64>(
            r#"WITH slot AS (
                   UPDATE "BulkChallengeDeletionSlots"
                      SET expires_at_utc = clock_timestamp() + INTERVAL '5 minutes'
                    WHERE lease_token = $4
                  RETURNING 1
               ), operation AS (
                   UPDATE "BulkChallengeMutationOperations"
                      SET result = $3,
                          lease_expires_at_utc = clock_timestamp() + INTERVAL '5 minutes'
                    WHERE game_id = $1 AND operation_id = $2 AND state = 1
                      AND lease_token = $4 AND EXISTS (SELECT 1 FROM slot)
                  RETURNING 1
               ) SELECT COUNT(*)::bigint FROM operation"#,
        )
        .bind(game_id)
        .bind(operation_id)
        .bind(result)
        .bind(lease_token)
        .fetch_one(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if progress != 1 {
            return Ok(());
        }
    }
    let revision = sqlx::query_scalar::<_, i64>(
        r#"SELECT challenge_configuration_revision FROM "Games" WHERE id = $1"#,
    )
    .bind(game_id)
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    // These event-wide effects remain recoverable while the operation is in
    // state 1. Only after both have completed do we publish the terminal replay
    // result; a crash or VPN outage therefore resumes instead of silently
    // freezing a partially reconciled delete batch.
    if effect_progress & EFFECT_VPN_RECONCILED == 0 {
        if !renew_delete_lease(st.pg(), game_id, operation_id, lease_token).await? {
            return Ok(());
        }
        crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await?;
        if !record_delete_effect(
            st.pg(),
            game_id,
            operation_id,
            lease_token,
            EFFECT_VPN_RECONCILED,
        )
        .await?
        {
            return Ok(());
        }
        effect_progress |= EFFECT_VPN_RECONCILED;
    }
    if effect_progress & EFFECT_SCOREBOARDS_FLUSHED == 0 {
        if !renew_delete_lease(st.pg(), game_id, operation_id, lease_token).await? {
            return Ok(());
        }
        flush_game_scoreboards(st, game_id).await;
        if !record_delete_effect(
            st.pg(),
            game_id,
            operation_id,
            lease_token,
            EFFECT_SCOREBOARDS_FLUSHED,
        )
        .await?
        {
            return Ok(());
        }
        effect_progress |= EFFECT_SCOREBOARDS_FLUSHED;
    }
    debug_assert_eq!(
        effect_progress & (EFFECT_VPN_RECONCILED | EFFECT_SCOREBOARDS_FLUSHED),
        EFFECT_VPN_RECONCILED | EFFECT_SCOREBOARDS_FLUSHED
    );
    let completion = sqlx::query_scalar::<_, i64>(
        r#"WITH completed AS (
               UPDATE "BulkChallengeMutationOperations" operation
                  SET state = 2, result_revision = $3, lease_token = NULL,
                      completed_at_utc = clock_timestamp()
                WHERE game_id = $1 AND operation_id = $2 AND state = 1
                  AND lease_token = $4
                  AND (effect_progress & $5) = $5
                  AND EXISTS (
                      SELECT 1 FROM "BulkChallengeDeletionSlots" slot
                       WHERE slot.lease_token = $4
                  )
              RETURNING 1
           ), released AS (
               UPDATE "BulkChallengeDeletionSlots"
                  SET lease_token = NULL, expires_at_utc = NULL
                WHERE lease_token = $4 AND EXISTS (SELECT 1 FROM completed)
              RETURNING 1
           ) SELECT COUNT(*)::bigint FROM completed"#,
    )
    .bind(game_id)
    .bind(operation_id)
    .bind(revision)
    .bind(lease_token)
    .bind(EFFECT_VPN_RECONCILED | EFFECT_SCOREBOARDS_FLUSHED)
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if completion != 1 {
        return Ok(());
    }
    cleanup_operations(st).await;
    Ok(())
}

pub async fn mutate_challenges_bulk(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(game_id): Path<i32>,
    Json(mut request): Json<BulkChallengeMutationRequest>,
) -> AppResult<RequestResponse<BulkChallengeMutationResult>> {
    manager_or_admin(&st, &user, game_id).await?;
    validate_request(&mut request)?;
    let digest = Sha256::digest(
        serde_json::to_vec(&(
            request.expected_revision,
            request.action,
            &request.challenge_ids,
        ))
        .map_err(|error| AppError::internal(error.to_string()))?,
    )
    .to_vec();
    let (state, outcomes, result_revision) =
        reserve_operation(&st, user.id, game_id, &request, &digest).await?;
    if state == 2 {
        return Ok(RequestResponse::ok(BulkChallengeMutationResult {
            operation_id: request.operation_id,
            state: "Complete",
            configuration_revision: result_revision.unwrap_or(request.expected_revision),
            outcomes,
        }));
    }
    if request.action != BulkChallengeAction::Delete {
        let lease_token =
            claim_desired_state_operation(st.pg(), game_id, request.operation_id, request.action)
                .await?;
        if let Some(lease_token) = lease_token {
            let Ok(permit) = BULK_DESIRED_STATE_SLOTS.clone().try_acquire_owned() else {
                expire_desired_state_lease(st.pg(), game_id, request.operation_id, lease_token)
                    .await?;
                return Ok(RequestResponse::ok(BulkChallengeMutationResult {
                    operation_id: request.operation_id,
                    state: "Pending",
                    configuration_revision: result_revision.unwrap_or(request.expected_revision),
                    outcomes,
                }));
            };
            let prepared = complete_desired_state(&st, game_id, &request, lease_token, false).await;
            if let Err(error) = prepared {
                let _ =
                    expire_desired_state_lease(st.pg(), game_id, request.operation_id, lease_token)
                        .await;
                return Err(error);
            }
            spawn_desired_state_job_with_permit(
                st.clone(),
                game_id,
                request.clone(),
                lease_token,
                permit,
            );
        }
        return Ok(RequestResponse::ok(BulkChallengeMutationResult {
            operation_id: request.operation_id,
            state: "Pending",
            configuration_revision: result_revision.unwrap_or(request.expected_revision),
            outcomes,
        }));
    }

    let revision = if state == 0 {
        let (revision, lease_token) = validate_delete_job(&st, game_id, &request).await?;
        if let Some(lease_token) = lease_token {
            schedule_delete_job(&st, game_id, request.operation_id, lease_token).await?;
        }
        revision
    } else {
        if let Some(lease_token) =
            reclaim_delete_operation(st.pg(), game_id, request.operation_id).await?
        {
            schedule_delete_job(&st, game_id, request.operation_id, lease_token).await?;
        }
        result_revision.unwrap_or(request.expected_revision)
    };
    Ok(RequestResponse::ok(BulkChallengeMutationResult {
        operation_id: request.operation_id,
        state: "Pending",
        configuration_revision: revision,
        outcomes,
    }))
}

#[cfg(test)]
#[path = "bulk_tests.rs"]
mod tests;
