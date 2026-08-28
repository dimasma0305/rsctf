use super::*;
use sha2::{Digest, Sha256};

const MAX_BULK_CHALLENGES: usize = 100;

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

#[derive(Debug, Deserialize, Serialize)]
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
) -> AppResult<(i16, Vec<BulkChallengeOutcome>, Option<i64>, bool)> {
    let inserted = sqlx::query_scalar::<_, bool>(
        r#"INSERT INTO "BulkChallengeMutationOperations"
             (game_id, operation_id, actor_user_id, expected_revision, action,
              challenge_ids, request_digest)
           VALUES ($1, $2, $3, $4, $5, $6, $7)
           ON CONFLICT (game_id, operation_id) DO NOTHING
           RETURNING TRUE"#,
    )
    .bind(game_id)
    .bind(request.operation_id)
    .bind(actor_user_id)
    .bind(request.expected_revision)
    .bind(request.action.as_i16())
    .bind(&request.challenge_ids)
    .bind(digest)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .is_some();
    let row = sqlx::query_as::<_, (Uuid, Vec<u8>, i16, serde_json::Value, Option<i64>, bool)>(
        r#"SELECT actor_user_id, request_digest, state, result, result_revision,
                  lease_expires_at_utc <= clock_timestamp()
             FROM "BulkChallengeMutationOperations"
            WHERE game_id = $1 AND operation_id = $2"#,
    )
    .bind(game_id)
    .bind(request.operation_id)
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if row.0 != actor_user_id || row.1 != digest {
        return Err(AppError::conflict(
            "The operation ID is already bound to another bulk mutation",
        ));
    }
    let outcomes = serde_json::from_value(row.3)
        .map_err(|error| AppError::internal(format!("Invalid bulk mutation result: {error}")))?;
    Ok((row.2, outcomes, row.4, inserted || row.5))
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

async fn complete_desired_state(
    st: &SharedState,
    game_id: i32,
    request: &BulkChallengeMutationRequest,
) -> AppResult<BulkChallengeMutationResult> {
    let desired = request.action == BulkChallengeAction::Enable;
    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, game_id).await?;
    let game_state = sqlx::query_as::<_, (i64, bool, bool)>(
        r#"SELECT challenge_configuration_revision,
                  ad_scoring_start_round IS NOT NULL OR koth_scoring_start_round IS NOT NULL,
                  start_time_utc <= clock_timestamp() AND end_time_utc >= clock_timestamp()
             FROM "Games" WHERE id = $1 FOR UPDATE"#,
    )
    .bind(game_id)
    .fetch_optional(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Game not found"))?;
    let stored = sqlx::query_as::<_, (i16, serde_json::Value, Option<i64>)>(
        r#"SELECT state, result, result_revision
             FROM "BulkChallengeMutationOperations"
            WHERE game_id = $1 AND operation_id = $2
            FOR UPDATE"#,
    )
    .bind(game_id)
    .bind(request.operation_id)
    .fetch_one(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if stored.0 == 2 {
        let outcomes = serde_json::from_value(stored.1)
            .map_err(|error| AppError::internal(error.to_string()))?;
        control
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(BulkChallengeMutationResult {
            operation_id: request.operation_id,
            state: "Complete",
            configuration_revision: stored.2.unwrap_or(game_state.0),
            outcomes,
        });
    }
    if game_state.0 != request.expected_revision {
        drop(control);
        abandon_operation(st, game_id, request.operation_id).await;
        return Err(AppError::conflict(format!(
            "Challenge configuration changed; current revision is {}",
            game_state.0
        )));
    }

    let rows = sqlx::query_as::<_, SelectedChallenge>(
        r#"SELECT challenge.id, challenge.title, challenge."Type" AS challenge_type,
                  challenge.is_enabled, challenge.deletion_pending,
                  challenge.review_status,
                  EXISTS (SELECT 1 FROM "FlagContexts" flag
                           WHERE flag.challenge_id = challenge.id) AS has_flag,
                  challenge.ad_self_hosted
             FROM "GameChallenges" challenge
            WHERE challenge.game_id = $1 AND challenge.id = ANY($2)
            ORDER BY challenge.id
            FOR UPDATE"#,
    )
    .bind(game_id)
    .bind(&request.challenge_ids)
    .fetch_all(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let by_id = rows
        .iter()
        .map(|row| (row.id, row))
        .collect::<std::collections::HashMap<_, _>>();
    let mut changed_ids = Vec::new();
    let mut changed_titles = Vec::new();
    let mut changed_runtimes = Vec::new();
    let mut outcomes = Vec::with_capacity(request.challenge_ids.len());
    for challenge_id in &request.challenge_ids {
        let Some(row) = by_id.get(challenge_id) else {
            outcomes.push(BulkChallengeOutcome {
                challenge_id: *challenge_id,
                status: "Rejected".into(),
                message: Some("Challenge not found in this event".into()),
            });
            continue;
        };
        let engine_type = row.challenge_type == ChallengeType::AttackDefense as i16
            || row.challenge_type == ChallengeType::KingOfTheHill as i16;
        let static_type = row.challenge_type == ChallengeType::StaticAttachment as i16
            || row.challenge_type == ChallengeType::StaticContainer as i16;
        let rejection = if row.deletion_pending {
            Some("Challenge is being deleted")
        } else if row.review_status != ChallengeReviewStatus::Active as i16 {
            Some("Only active challenges can be changed")
        } else if engine_type && game_state.1 && row.is_enabled != desired {
            Some("Engine challenge state is locked after scoring started")
        } else if desired && static_type && !row.has_flag {
            Some("Cannot enable a static challenge without a flag")
        } else {
            None
        };
        if let Some(message) = rejection {
            outcomes.push(BulkChallengeOutcome {
                challenge_id: *challenge_id,
                status: "Rejected".into(),
                message: Some(message.into()),
            });
        } else if row.is_enabled == desired {
            outcomes.push(BulkChallengeOutcome {
                challenge_id: *challenge_id,
                status: "Unchanged".into(),
                message: None,
            });
        } else {
            changed_ids.push(*challenge_id);
            changed_titles.push(row.title.clone());
            changed_runtimes.push((row.id, row.challenge_type, row.ad_self_hosted));
            outcomes.push(BulkChallengeOutcome {
                challenge_id: *challenge_id,
                status: "Changed".into(),
                message: None,
            });
        }
    }

    if !changed_ids.is_empty() {
        let progress = sqlx::query(
            r#"UPDATE "GameChallenges" SET is_enabled = $3
                WHERE game_id = $1 AND id = ANY($2)"#,
        )
        .bind(game_id)
        .bind(&changed_ids)
        .bind(desired)
        .execute(&mut **control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if progress.rows_affected() != changed_ids.len() as u64 {
            return Err(AppError::conflict(
                "A selected challenge changed during the bulk mutation",
            ));
        }
    }
    let result_revision = sqlx::query_scalar::<_, i64>(
        r#"SELECT challenge_configuration_revision FROM "Games" WHERE id = $1"#,
    )
    .bind(game_id)
    .fetch_one(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let result_json =
        serde_json::to_value(&outcomes).map_err(|error| AppError::internal(error.to_string()))?;
    let completion = sqlx::query(
        r#"UPDATE "BulkChallengeMutationOperations"
              SET state = 2, result = $3, result_revision = $4,
                  completed_at_utc = clock_timestamp()
            WHERE game_id = $1 AND operation_id = $2 AND state = 0"#,
    )
    .bind(game_id)
    .bind(request.operation_id)
    .bind(result_json)
    .bind(result_revision)
    .execute(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if completion.rows_affected() != 1 {
        return Err(AppError::conflict(
            "Bulk challenge operation changed while it was running",
        ));
    }
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    cleanup_operations(st).await;
    if let Err(error) = crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await {
        tracing::warn!(%error, game_id, "bulk challenge VPN reconciliation deferred");
    }
    flush_game_scoreboards(st, game_id).await;
    if desired && game_state.2 && !changed_titles.is_empty() {
        let values = serde_json::json!(changed_titles);
        let notice = sqlx::query_as::<_, (i32, DateTime<Utc>)>(
            r#"INSERT INTO "GameNotices" (game_id, "Type", values, publish_time_utc)
               VALUES ($1, $2, $3, clock_timestamp())
               RETURNING id, publish_time_utc"#,
        )
        .bind(game_id)
        .bind(NoticeType::NewChallenge as i16)
        .bind(&values)
        .fetch_one(st.pg())
        .await;
        match notice {
            Ok((notice_id, publish_time_utc)) => st.publish_event(
                "ReceivedGameNotice",
                Some(game_id),
                serde_json::json!({
                    "type": NoticeType::NewChallenge,
                    "values": values,
                    "id": notice_id,
                    "time": publish_time_utc,
                })
                .to_string(),
            ),
            Err(error) => {
                tracing::warn!(%error, game_id, "bulk challenge notice reconciliation deferred");
            }
        }
    }
    if !desired && !changed_ids.is_empty() {
        let background = st.clone();
        tokio::spawn(async move {
            for (challenge_id, challenge_type, ad_self_hosted) in changed_runtimes {
                let is_container = matches!(
                    challenge_type,
                    value if value == ChallengeType::StaticContainer as i16
                        || value == ChallengeType::DynamicContainer as i16
                        || value == ChallengeType::AttackDefense as i16
                        || value == ChallengeType::KingOfTheHill as i16
                );
                if is_container {
                    if let Err(error) = super::lifecycle::destroy_challenge_containers_by_id(
                        &background,
                        game_id,
                        challenge_id,
                        true,
                        false,
                    )
                    .await
                    {
                        tracing::warn!(%error, challenge_id, "bulk disable cleanup deferred");
                    }
                }
                if ad_self_hosted {
                    if let Err(error) = background
                        .byoc
                        .disconnect_challenge(&background.db, challenge_id)
                        .await
                    {
                        tracing::warn!(%error, challenge_id, "bulk BYOC cleanup deferred");
                    }
                }
            }
        });
    }
    Ok(BulkChallengeMutationResult {
        operation_id: request.operation_id,
        state: "Complete",
        configuration_revision: result_revision,
        outcomes,
    })
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
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok((
        revision,
        (claimed.rows_affected() == 1).then_some(lease_token),
    ))
}

fn spawn_delete_job(st: SharedState, game_id: i32, operation_id: Uuid, lease_token: Uuid) {
    tokio::spawn(async move {
        if let Err(error) = run_delete_job(&st, game_id, operation_id, lease_token).await {
            tracing::error!(%error, game_id, %operation_id, "bulk challenge deletion paused");
        }
    });
}

async fn run_delete_job(
    st: &SharedState,
    game_id: i32,
    operation_id: Uuid,
    lease_token: Uuid,
) -> AppResult<()> {
    let operation: Option<(Vec<i32>, serde_json::Value)> = sqlx::query_as(
        r#"SELECT challenge_ids, result FROM "BulkChallengeMutationOperations"
            WHERE game_id = $1 AND operation_id = $2 AND state = 1
              AND lease_token = $3"#,
    )
    .bind(game_id)
    .bind(operation_id)
    .bind(lease_token)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((challenge_ids, completed)) = operation else {
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
        let outcome =
            match super::delete_challenge_core(st.clone(), game_id, challenge_id, false).await {
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
        let progress = sqlx::query(
            r#"UPDATE "BulkChallengeMutationOperations"
                  SET result = $3,
                      lease_expires_at_utc = clock_timestamp() + INTERVAL '5 minutes'
                WHERE game_id = $1 AND operation_id = $2 AND state = 1
                  AND lease_token = $4"#,
        )
        .bind(game_id)
        .bind(operation_id)
        .bind(result)
        .bind(lease_token)
        .execute(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if progress.rows_affected() != 1 {
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
    sqlx::query(
        r#"UPDATE "BulkChallengeMutationOperations"
              SET state = 2, result_revision = $3, lease_token = NULL,
                  completed_at_utc = clock_timestamp()
            WHERE game_id = $1 AND operation_id = $2 AND state = 1
              AND lease_token = $4"#,
    )
    .bind(game_id)
    .bind(operation_id)
    .bind(revision)
    .bind(lease_token)
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    flush_game_scoreboards(st, game_id).await;
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
    let (state, outcomes, result_revision, may_claim) =
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
        if state != 0 {
            return Err(AppError::conflict("This bulk mutation is still running"));
        }
        if !may_claim {
            return Err(AppError::conflict(
                "This bulk mutation is still running; retry later",
            ));
        }
        return Ok(RequestResponse::ok(
            complete_desired_state(&st, game_id, &request).await?,
        ));
    }

    let revision = if state == 0 {
        let (revision, lease_token) = validate_delete_job(&st, game_id, &request).await?;
        if let Some(lease_token) = lease_token {
            spawn_delete_job(st.clone(), game_id, request.operation_id, lease_token);
        }
        revision
    } else {
        if may_claim {
            let lease_token = Uuid::new_v4();
            let claimed = sqlx::query(
                r#"UPDATE "BulkChallengeMutationOperations"
                      SET lease_token = $3,
                          lease_expires_at_utc = clock_timestamp() + INTERVAL '5 minutes'
                    WHERE game_id = $1 AND operation_id = $2 AND state = 1
                      AND lease_expires_at_utc <= clock_timestamp()"#,
            )
            .bind(game_id)
            .bind(request.operation_id)
            .bind(lease_token)
            .execute(st.pg())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
            if claimed.rows_affected() == 1 {
                spawn_delete_job(st.clone(), game_id, request.operation_id, lease_token);
            }
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
mod tests {
    use super::*;

    #[test]
    fn rejects_duplicate_and_oversized_intents_before_reservation() {
        let mut duplicate = BulkChallengeMutationRequest {
            operation_id: Uuid::new_v4(),
            expected_revision: 1,
            action: BulkChallengeAction::Enable,
            challenge_ids: vec![9, 9],
        };
        assert_eq!(
            validate_request(&mut duplicate).unwrap_err().status(),
            axum::http::StatusCode::BAD_REQUEST
        );
        let mut oversized = BulkChallengeMutationRequest {
            operation_id: Uuid::new_v4(),
            expected_revision: 1,
            action: BulkChallengeAction::Delete,
            challenge_ids: (1..=101).collect(),
        };
        assert_eq!(
            validate_request(&mut oversized).unwrap_err().status(),
            axum::http::StatusCode::PAYLOAD_TOO_LARGE
        );
    }
}
