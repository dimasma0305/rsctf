//! edit: flag CRUD (see edit/mod.rs for the router + shared DTOs/helpers).
use super::*;

/// `POST /api/edit/games/{id}/challenges/{cId}/flags` — void.
pub async fn add_flags(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, c_id)): Path<(i32, i32)>,
    Json(models): Json<Vec<FlagCreateModel>>,
) -> AppResult<MessageResponse> {
    manager_or_admin(&st, &user, id).await?;
    load_challenge(&st, id, c_id).await?;

    // Attachment creation does not alter grading policy. Materialize it before
    // taking the flag-policy lock so submissions are not held up by blob lookup.
    let mut flags = Vec::with_capacity(models.len());
    for m in models {
        // Each flag can carry its own hand-out attachment (RSCTF AddFlags).
        let attachment_id =
            build_attachment(&st, m.attachment_type, m.file_hash, m.remote_url).await?;
        flags.push((m.flag, attachment_id));
    }

    let mut definition_lock =
        crate::services::challenge_workloads::acquire_definition_lock(st.pg(), id, c_id).await?;
    crate::utils::scoring::lock_jeopardy_flags_exclusive(
        &mut **definition_lock.transaction_mut(),
        c_id,
    )
    .await?;

    // Keep the parent alive until every insert commits. This also makes a
    // concurrent challenge delete wait instead of turning a valid edit into a
    // foreign-key error halfway through the batch.
    let challenge_exists = sqlx::query_scalar::<_, i32>(
        r#"SELECT id FROM "GameChallenges"
            WHERE id = $1 AND game_id = $2
            FOR KEY SHARE"#,
    )
    .bind(c_id)
    .bind(id)
    .fetch_optional(&mut **definition_lock.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .is_some();
    if !challenge_exists {
        return Err(AppError::not_found("Challenge not found"));
    }

    for (flag, attachment_id) in flags {
        sqlx::query(
            r#"INSERT INTO "FlagContexts"
                 (flag, is_occupied, challenge_id, attachment_id)
               VALUES ($1, FALSE, $2, $3)"#,
        )
        .bind(flag)
        .bind(c_id)
        .bind(attachment_id)
        .execute(&mut **definition_lock.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }

    definition_lock.release().await?;
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
    load_challenge(&st, id, c_id).await?;

    let mut definition_lock =
        crate::services::challenge_workloads::acquire_definition_lock(st.pg(), id, c_id).await?;
    crate::utils::scoring::lock_jeopardy_flags_exclusive(
        &mut **definition_lock.transaction_mut(),
        c_id,
    )
    .await?;

    // Capture the hand-out attachment in the same statement that removes the
    // flag. The exclusive advisory lock makes this deletion linearizable with
    // every authoritative submit-side grade, including static flag inserts.
    let attachment_id: Option<Option<i32>> = sqlx::query_scalar(
        r#"DELETE FROM "FlagContexts"
            WHERE id = $1 AND challenge_id = $2
            RETURNING attachment_id"#,
    )
    .bind(f_id)
    .bind(c_id)
    .fetch_optional(&mut **definition_lock.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some(attachment_id) = attachment_id else {
        return Ok(RequestResponse::ok("NotFound".to_string()));
    };

    definition_lock.release().await?;
    if let Some(aid) = attachment_id {
        delete_attachment(&st, aid).await?;
    }
    Ok(RequestResponse::ok("Success".to_string()))
}

// ============================================================================
//  Notices
// ============================================================================
