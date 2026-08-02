//! edit: flag CRUD (see edit/mod.rs for the router + shared DTOs/helpers).
use super::*;

async fn cleanup_staged_flag_attachments(st: &SharedState, flags: &[(String, Option<i32>)]) {
    for attachment_id in flags.iter().filter_map(|(_, attachment_id)| *attachment_id) {
        if let Err(error) = delete_attachment(st, attachment_id).await {
            tracing::warn!(
                %error,
                attachment_id,
                "failed to clean an unpublished flag attachment"
            );
        }
    }
}

/// The accepted static flags and a dynamic flag template are scoring policy,
/// not ordinary challenge content. Production callers own the game-control
/// lock and the challenge-scoped JFLG lock, making this decision linearizable
/// with both engine startup and a first accepted submit.
async fn ensure_flag_policy_mutable_locked(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<()> {
    let challenge_exists = sqlx::query_scalar::<_, bool>(
        r#"SELECT TRUE FROM "GameChallenges"
            WHERE game_id = $1 AND id = $2"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Challenge not found"))?;
    debug_assert!(challenge_exists);
    let scoring_started = competition_scoring_started_locked(connection, game_id).await?;
    if scoring_started {
        return Err(AppError::bad_request(
            "Challenge flags are locked after competition scoring has started.",
        ));
    }
    Ok(())
}

/// `POST /api/edit/games/{id}/challenges/{cId}/flags` — void.
pub async fn add_flags(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, c_id)): Path<(i32, i32)>,
    Json(models): Json<Vec<FlagCreateModel>>,
) -> AppResult<MessageResponse> {
    manager_or_admin(&st, &user, id).await?;
    challenges::reject_pending_mutation(st.pg(), id, c_id).await?;
    load_challenge(&st, id, c_id).await?;

    // Attachment creation does not alter grading policy. Materialize it before
    // taking the flag-policy lock so submissions are not held up by blob lookup.
    let mut flags = Vec::with_capacity(models.len());
    for m in models {
        // Each flag can carry its own hand-out attachment (RSCTF AddFlags).
        let attachment_id =
            match build_attachment(&st, m.attachment_type, m.file_hash, m.remote_url).await {
                Ok(attachment_id) => attachment_id,
                Err(error) => {
                    cleanup_staged_flag_attachments(&st, &flags).await;
                    return Err(error);
                }
            };
        flags.push((m.flag, attachment_id));
    }

    // Global order is game-control -> challenge definition -> JFLG. The game
    // lock prevents an A&D/KotH first round from crossing the policy check;
    // JFLG provides the corresponding first-Jeopardy-solve fence.
    let game_control = match crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await {
        Ok(lock) => lock,
        Err(error) => {
            cleanup_staged_flag_attachments(&st, &flags).await;
            return Err(error);
        }
    };
    let mut definition_lock = match crate::services::challenge_workloads::acquire_definition_lock(
        st.pg(),
        id,
        c_id,
    )
    .await
    {
        Ok(lock) => lock,
        Err(error) => {
            drop(game_control);
            cleanup_staged_flag_attachments(&st, &flags).await;
            return Err(AppError::internal(error.to_string()));
        }
    };
    let mutation: AppResult<()> = async {
        // Deletion may have won after the intentionally lock-free attachment
        // staging. Recheck both durable fences in this retained transaction so
        // their key-share row locks survive until every flag insert commits.
        challenges::reject_pending_mutation(&mut **definition_lock.transaction_mut(), id, c_id)
            .await?;
        ensure_flag_policy_mutable_locked(definition_lock.transaction_mut(), id, c_id).await?;
        crate::utils::scoring::lock_jeopardy_flags_exclusive(
            definition_lock.transaction_mut(),
            c_id,
        )
        .await?;

        for (flag, attachment_id) in &flags {
            sqlx::query(
                r#"INSERT INTO "FlagContexts"
                     (flag, is_occupied, challenge_id, attachment_id)
                   VALUES ($1, FALSE, $2, $3)"#,
            )
            .bind(flag)
            .bind(c_id)
            .bind(*attachment_id)
            .execute(&mut **definition_lock.transaction_mut())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        }
        Ok(())
    }
    .await;

    if let Err(error) = mutation {
        drop(definition_lock);
        drop(game_control);
        cleanup_staged_flag_attachments(&st, &flags).await;
        return Err(error);
    }
    if let Err(error) = definition_lock.release().await {
        drop(game_control);
        cleanup_staged_flag_attachments(&st, &flags).await;
        return Err(AppError::internal(error.to_string()));
    }
    if let Err(error) = game_control.release().await {
        tracing::warn!(%error, id, c_id, "flag-policy game lock release failed after commit");
    }
    Ok(MessageResponse::ok(""))
}

/// `DELETE /api/edit/games/{id}/challenges/{cId}/flags/{fId}` — returns a
/// `TaskStatus`. RSCTF serializes this enum as a **string**, so we emit the
/// string literal directly (the port's `TaskStatus` enum is int-repr).
pub async fn remove_flag(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, c_id, f_id)): Path<(i32, i32, i32)>,
) -> AppResult<RequestResponse<String>> {
    manager_or_admin(&st, &user, id).await?;
    let game_control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await?;
    let mut definition_lock =
        crate::services::challenge_workloads::acquire_definition_lock(st.pg(), id, c_id).await?;
    let removal = match remove_flag_locked(definition_lock.transaction_mut(), id, c_id, f_id).await
    {
        Ok(removal) => removal,
        Err(error) => {
            if let Err(rollback_error) = definition_lock.rollback().await {
                tracing::warn!(%rollback_error, f_id, "flag removal rollback failed");
            }
            drop(game_control);
            return Err(error);
        }
    };
    definition_lock
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if let Err(error) = game_control.release().await {
        tracing::warn!(%error, id, c_id, f_id, "flag-policy game lock release failed after commit");
    }
    let Some(deleted_hash) = removal else {
        return Ok(RequestResponse::ok("NotFound".to_string()));
    };
    if let Some(hash) = deleted_hash {
        if let Err(error) =
            crate::services::blob_refs::purge_if_unreferenced(st.pg(), st.storage.as_ref(), &hash)
                .await
        {
            tracing::warn!(%error, %hash, f_id, "removed flag attachment blob purge deferred");
        }
    }
    Ok(RequestResponse::ok("Success".to_string()))
}

/// Delete the flag and consume its now-orphaned attachment reference in the
/// retained definition transaction. `None` means the flag did not exist;
/// `Some(None)` means it existed without a local blob requiring purge.
pub(super) async fn remove_flag_locked(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    game_id: i32,
    challenge_id: i32,
    flag_id: i32,
) -> AppResult<Option<Option<String>>> {
    challenges::reject_pending_mutation(&mut **transaction, game_id, challenge_id).await?;
    ensure_flag_policy_mutable_locked(transaction, game_id, challenge_id).await?;
    crate::utils::scoring::lock_jeopardy_flags_exclusive(transaction, challenge_id).await?;

    // Capture the hand-out attachment in the same statement that removes the
    // flag. The exclusive advisory lock makes this deletion linearizable with
    // every authoritative submit-side grade, including static flag inserts.
    let attachment_id: Option<Option<i32>> = sqlx::query_scalar(
        r#"DELETE FROM "FlagContexts"
            WHERE id = $1 AND challenge_id = $2
            RETURNING attachment_id"#,
    )
    .bind(flag_id)
    .bind(challenge_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some(attachment_id) = attachment_id else {
        return Ok(None);
    };
    let deleted_hash = match attachment_id {
        Some(attachment_id) => {
            crate::services::blob_refs::delete_attachment_locked(transaction, attachment_id).await?
        }
        None => None,
    };
    Ok(Some(deleted_hash))
}

// ============================================================================
//  Notices
// ============================================================================
