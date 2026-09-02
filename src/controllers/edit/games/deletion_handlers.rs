use super::deletion::{
    delete_ad_game_data, delete_detached_game_history, delete_restricted_game_history,
    fence_game_for_deletion, fence_game_for_purge,
};
use super::*;

/// `DELETE /api/edit/games/{id}` — returns the deleted game (contract:
/// `GameInfoModel`, not void).
pub async fn delete_game(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<GameInfoModel>> {
    delete_game_with_policy(st, id, None).await
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GamePurgeModel {
    pub operation_id: Uuid,
    pub expected_configuration_revision: i64,
    pub confirmation_title: String,
}

struct PurgeIntent {
    operation_id: Uuid,
    actor_user_id: Uuid,
    expected_configuration_revision: i64,
    confirmation_title: String,
    request_digest: String,
}

enum PurgeReplay {
    Missing,
    Pending,
    Complete(Box<GameInfoModel>),
}

pub(super) fn validate_purge_request(model: &GamePurgeModel) -> AppResult<()> {
    if model.operation_id.is_nil() {
        return Err(AppError::bad_request(
            "A stable operationId is required to purge an event",
        ));
    }
    if !(0..=9_007_199_254_740_990).contains(&model.expected_configuration_revision) {
        return Err(AppError::bad_request(
            "expectedConfigurationRevision must be a non-negative safe integer",
        ));
    }
    if model.confirmation_title.is_empty() {
        return Err(AppError::bad_request(
            "confirmationTitle must exactly match the current event title",
        ));
    }
    Ok(())
}

pub(super) fn purge_request_digest(game_id: i32, model: &GamePurgeModel) -> AppResult<String> {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "version": 1,
        "gameId": game_id,
        "expectedConfigurationRevision": model.expected_configuration_revision,
        "confirmationTitle": model.confirmation_title,
    }))
    .map_err(|error| AppError::internal(format!("could not encode event purge: {error}")))?;
    Ok(crate::utils::codec::sha256_hex(&bytes))
}

async fn replay_purge_operation(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    actor_user_id: Uuid,
    operation_id: Uuid,
    request_digest: &str,
) -> AppResult<PurgeReplay> {
    let row = sqlx::query_as::<
        _,
        (
            i32,
            Uuid,
            String,
            i16,
            Option<sqlx::types::Json<GameInfoModel>>,
        ),
    >(
        r#"SELECT game_id, actor_user_id, request_digest, status, result
             FROM "GamePurgeOperations"
            WHERE operation_id = $1"#,
    )
    .bind(operation_id)
    .fetch_optional(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((stored_game, stored_actor, stored_digest, status, result)) = row else {
        return Ok(PurgeReplay::Missing);
    };
    if stored_game != game_id || stored_actor != actor_user_id || stored_digest != request_digest {
        return Err(AppError::conflict(
            "The purge operation ID was already used for a different request",
        ));
    }
    match (status, result) {
        (0, None) => Ok(PurgeReplay::Pending),
        (1, Some(result)) => Ok(PurgeReplay::Complete(Box::new(result.0))),
        _ => Err(AppError::internal(
            "Event purge operation has an invalid durable state",
        )),
    }
}

async fn claim_purge_operation(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    game_id: i32,
    intent: &PurgeIntent,
) -> AppResult<PurgeReplay> {
    let inserted = sqlx::query(
        r#"INSERT INTO "GamePurgeOperations"
             (operation_id, game_id, actor_user_id, request_digest,
              expected_configuration_revision, confirmation_title)
           VALUES ($1, $2, $3, $4, $5, $6)
           ON CONFLICT DO NOTHING"#,
    )
    .bind(intent.operation_id)
    .bind(game_id)
    .bind(intent.actor_user_id)
    .bind(&intent.request_digest)
    .bind(intent.expected_configuration_revision)
    .bind(&intent.confirmation_title)
    .execute(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if inserted.rows_affected() == 1 {
        return Ok(PurgeReplay::Pending);
    }
    match replay_purge_operation(
        tx,
        game_id,
        intent.actor_user_id,
        intent.operation_id,
        &intent.request_digest,
    )
    .await?
    {
        PurgeReplay::Missing => Err(AppError::conflict(
            "Another purge operation is already active for this event",
        )),
        replay => Ok(replay),
    }
}

async fn complete_purge_operation(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    intent: &PurgeIntent,
    result: &GameInfoModel,
) -> AppResult<()> {
    let completed = sqlx::query(
        r#"UPDATE "GamePurgeOperations"
              SET status = 1, result = $2, completed_at_utc = clock_timestamp()
            WHERE operation_id = $1 AND status = 0"#,
    )
    .bind(intent.operation_id)
    .bind(sqlx::types::Json(result))
    .execute(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if completed.rows_affected() != 1 {
        return Err(AppError::internal(
            "Event purge operation could not be completed atomically",
        ));
    }
    Ok(())
}

async fn cleanup_completed_purge_operations(st: &SharedState) {
    if let Err(error) = sqlx::query(
        r#"WITH expired AS (
               SELECT operation_id FROM "GamePurgeOperations"
                WHERE status = 1
                  AND completed_at_utc < clock_timestamp() - interval '30 days'
                ORDER BY completed_at_utc, operation_id
                LIMIT 100
           )
           DELETE FROM "GamePurgeOperations" operation
            USING expired
            WHERE operation.operation_id = expired.operation_id"#,
    )
    .execute(st.pg())
    .await
    {
        tracing::warn!(%error, "event purge operation retention cleanup deferred");
    }
}

/// `POST /api/edit/games/{id}/purge` — irreversibly removes a hidden,
/// administratively disabled event and all of its competition history. The
/// capability is deployment-gated and platform-admin-only; ordinary deletion
/// continues to preserve any event that started or recorded evidence.
pub async fn purge_game(
    State(st): State<SharedState>,
    admin: AdminUser,
    Path(id): Path<i32>,
    Json(model): Json<GamePurgeModel>,
) -> AppResult<RequestResponse<GameInfoModel>> {
    if !st.config.allow_competition_history_purge {
        return Err(AppError::Forbidden);
    }
    validate_purge_request(&model)?;
    let request_digest = purge_request_digest(id, &model)?;
    let intent = PurgeIntent {
        operation_id: model.operation_id,
        actor_user_id: admin.0.id,
        expected_configuration_revision: model.expected_configuration_revision,
        confirmation_title: model.confirmation_title,
        request_digest,
    };
    let mut connection = st
        .pg()
        .acquire()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if let PurgeReplay::Complete(result) = replay_purge_operation(
        &mut connection,
        id,
        intent.actor_user_id,
        intent.operation_id,
        &intent.request_digest,
    )
    .await?
    {
        return Ok(RequestResponse::ok(*result));
    }
    drop(connection);
    let response = delete_game_with_policy(st.clone(), id, Some(intent)).await?;
    cleanup_completed_purge_operations(&st).await;
    Ok(response)
}

async fn delete_game_with_policy(
    st: SharedState,
    id: i32,
    purge: Option<PurgeIntent>,
) -> AppResult<RequestResponse<GameInfoModel>> {
    // Admit before the first game transaction. The permit survives the slow
    // runtime sweep and moves into the final deletion lock guard, so queued
    // hard deletes never consume pool connections while waiting.
    let deletion_admission = super::deletion_locks::acquire_hard_deletion_admission().await?;
    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await?;
    if let Some(intent) = purge.as_ref() {
        if let PurgeReplay::Complete(result) = replay_purge_operation(
            control.transaction_mut(),
            id,
            intent.actor_user_id,
            intent.operation_id,
            &intent.request_digest,
        )
        .await?
        {
            control
                .release()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            return Ok(RequestResponse::ok(*result));
        }
    }
    let g = update_support::load_game_locked(control.transaction_mut(), id, true).await?;
    if let Some(intent) = purge.as_ref() {
        if g.configuration_revision != intent.expected_configuration_revision {
            return Err(AppError::conflict(format!(
                "Event changed; current configuration revision is {}",
                g.configuration_revision
            )));
        }
        if g.title != intent.confirmation_title {
            return Err(AppError::bad_request(
                "confirmationTitle must exactly match the current event title",
            ));
        }
        if !g.hidden {
            return Err(AppError::bad_request(
                "Hide the event before permanently purging its competition history",
            ));
        }
        let enabled_challenges = sqlx::query_scalar::<_, bool>(
            r#"SELECT EXISTS (
                   SELECT 1 FROM "GameChallenges"
                    WHERE game_id = $1 AND is_enabled = TRUE
               )"#,
        )
        .bind(id)
        .fetch_one(&mut **control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if enabled_challenges {
            return Err(AppError::bad_request(
                "Disable every challenge before permanently purging the event",
            ));
        }
        if let PurgeReplay::Complete(result) =
            claim_purge_operation(control.transaction_mut(), id, intent).await?
        {
            control
                .release()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            return Ok(RequestResponse::ok(*result));
        }
    }
    // Reject irreversible deletion before touching event state. The marker and
    // history predicate share the game transaction and all challenge submission
    // fences, so an accepted submit cannot slip between the check and commit.
    if purge.is_some() {
        fence_game_for_purge(control.transaction_mut(), id).await?;
    } else {
        fence_game_for_deletion(control.transaction_mut(), id).await?;
    }
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    // The durable fence is a point of no return even if external teardown must
    // be retried. Hide the now-partially-deleting event from every cached play
    // surface before touching Docker, VPN, or blob storage.
    crate::controllers::game::invalidate_game_row_cache(id);
    flush_game_scoreboards(&st, id).await;
    crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await?;
    // Reap every running container the game owns (per-team instances + per-
    // challenge test/shared containers) before the rows cascade away, so the
    // backend isn't left with orphans it can no longer resolve.
    destroy_game_containers(&st, id).await?;
    let mut deletion_locks =
        super::deletion_locks::acquire_game_test_deletion_locks(&st.db, id, deletion_admission)
            .await?;
    destroy_game_test_containers_locked(&st, id).await?;
    let tx = deletion_locks.game_transaction_mut();
    // A concurrent administrative/runtime writer may have committed while slow
    // backend teardown held no game lock. Re-fence before the first evidence
    // delete; a conflict leaves every durable competition row intact.
    if purge.is_some() {
        fence_game_for_purge(tx, id).await?;
    } else {
        fence_game_for_deletion(tx, id).await?;
    }
    // Match the global writer order used by update/materialization paths before
    // deleting rollups or the Games row they reference.
    crate::services::ad::scoring::lock_epoch_rollups(&mut *tx, id).await?;
    crate::controllers::game::koth::lock_epoch_rollups(&mut *tx, id).await?;
    if let Some(intent) = purge.as_ref() {
        delete_restricted_game_history(tx, id, intent.operation_id).await?;
    }
    delete_ad_game_data(tx, id).await?;
    let deleted_challenge_artifacts =
        crate::services::blob_refs::delete_game_challenges_locked(tx, id).await?;
    let poster_hash = sqlx::query_scalar::<_, Option<String>>(
        r#"SELECT poster_hash FROM "Games" WHERE id = $1 FOR UPDATE"#,
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Game not found"))?;
    let deleted = sqlx::query(r#"DELETE FROM "Games" WHERE id = $1"#)
        .bind(id)
        .execute(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if deleted.rows_affected() != 1 {
        return Err(AppError::not_found("Game not found"));
    }
    if purge.is_some() {
        delete_detached_game_history(tx, id).await?;
    }
    if let Some(hash) = poster_hash.as_deref() {
        crate::services::blob_refs::release_direct_hash_locked(tx, hash).await?;
    }
    let durable_purge_result = if let Some(intent) = purge.as_ref() {
        let result = GameInfoModel::from_game(&g);
        complete_purge_operation(tx, intent, &result).await?;
        Some(result)
    } else {
        None
    };
    deletion_locks.release().await?;
    crate::services::blob_refs::purge_deleted_challenge_artifacts(
        st.pg(),
        st.storage.as_ref(),
        &deleted_challenge_artifacts,
    )
    .await;
    for attachment_id in deleted_challenge_artifacts.attachment_ids {
        if let Err(error) = delete_attachment(&st, attachment_id).await {
            tracing::warn!(%error, attachment_id, "deleted game attachment cleanup deferred");
        }
    }
    if let Some(hash) = poster_hash {
        crate::controllers::assets::invalidate_asset_gate(&st, &hash).await;
        if let Err(error) =
            crate::services::blob_refs::purge_if_unreferenced(st.pg(), st.storage.as_ref(), &hash)
                .await
        {
            tracing::warn!(%error, %hash, "deleted game poster cleanup deferred");
        }
    }
    crate::controllers::game::invalidate_game_row_cache(id);
    flush_game_scoreboards(&st, id).await;
    // `serverTime` is a response-creation sample. Build the response model only
    // after every potentially slow container, VPN, and blob teardown completes.
    let result = match durable_purge_result {
        Some(mut result) => {
            result.server_time = Some(Utc::now());
            result
        }
        None => GameInfoModel::from_game(&g),
    };
    Ok(RequestResponse::ok(result))
}
