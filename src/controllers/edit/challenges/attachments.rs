//! Challenge attachment mutation and validation.

use super::*;

#[derive(Debug, Clone)]
struct PreparedAttachment {
    file_type: FileType,
    file_hash: Option<String>,
    remote_url: Option<String>,
    upload_stage: Option<crate::services::blob_refs::StagedBlob>,
}

#[derive(Debug, PartialEq, Eq)]
struct AttachmentSwap {
    attachment_id: Option<i32>,
    deleted_hash: Option<String>,
}

/// `POST /api/edit/games/{id}/challenges/{cId}/attachment` — set the canonical
/// download attachment for a (non-dynamic) challenge. Returns the new attachment
/// id (contract: `number`, `0` when cleared). Mirrors
/// `ChallengeRepository.UpdateAttachment`.
pub async fn update_attachment(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, c_id)): Path<(i32, i32)>,
    Json(model): Json<AttachmentCreateModel>,
) -> AppResult<RequestResponse<i32>> {
    manager_or_admin(&st, &user, id).await?;
    let mut prepared =
        prepare_attachment(model.attachment_type, model.file_hash, model.remote_url)?;
    validate_upload_binding(prepared.as_ref(), model.upload_id)?;
    if let Some(upload_id) = model.upload_id {
        let prepared = prepared
            .as_mut()
            .ok_or_else(|| AppError::bad_request("An uploadId requires a local attachment"))?;
        if prepared.file_type != FileType::Local {
            return Err(AppError::bad_request(
                "An uploadId requires a local attachment",
            ));
        }
        let hash = prepared
            .file_hash
            .as_deref()
            .ok_or_else(|| AppError::bad_request("A local attachment requires fileHash"))?;
        prepared.upload_stage = Some(
            crate::services::blob_refs::load_ready_upload_stage(st.pg(), upload_id, user.id, hash)
                .await?,
        );
    }
    let mut definition_lock =
        crate::services::challenge_workloads::acquire_definition_lock(st.pg(), id, c_id).await?;
    let swap = match replace_attachment_locked(
        definition_lock.transaction_mut(),
        id,
        c_id,
        prepared.as_ref(),
    )
    .await
    {
        Ok(swap) => swap,
        Err(error) => {
            if let Err(rollback_error) = definition_lock.rollback().await {
                tracing::warn!(%rollback_error, c_id, "challenge attachment rollback failed");
            }
            return Err(error);
        }
    };
    definition_lock
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    // Object deletion is retryable through the durable zero-reference Files
    // tombstone. A storage outage after commit must not turn a successful,
    // visible swap into an error response that an operator retries.
    if let Some(hash) = swap.deleted_hash.as_deref() {
        purge_replaced_attachment(st.pg(), st.storage.as_ref(), c_id, hash).await;
    }
    Ok(RequestResponse::ok(swap.attachment_id.unwrap_or(0)))
}

fn validate_upload_binding(
    prepared: Option<&PreparedAttachment>,
    upload_id: Option<Uuid>,
) -> AppResult<()> {
    match (prepared.map(|value| value.file_type), upload_id) {
        (Some(FileType::Local), None) => Err(AppError::bad_request(
            "A local attachment requires its uploadId",
        )),
        (Some(FileType::Local), Some(_)) | (_, None) => Ok(()),
        (_, Some(_)) => Err(AppError::bad_request(
            "An uploadId requires a local attachment",
        )),
    }
}

async fn purge_replaced_attachment(
    pool: &sqlx::PgPool,
    storage: &dyn crate::storage::BlobStorage,
    challenge_id: i32,
    hash: &str,
) {
    if let Err(error) = crate::services::blob_refs::purge_if_unreferenced(pool, storage, hash).await
    {
        tracing::warn!(%error, %hash, challenge_id, "replaced attachment blob purge deferred");
    }
}

fn prepare_attachment(
    file_type: Option<FileType>,
    file_hash: Option<String>,
    remote_url: Option<String>,
) -> AppResult<Option<PreparedAttachment>> {
    let file_type = file_type.unwrap_or(FileType::None);
    if file_type == FileType::None {
        return Ok(None);
    }
    let remote_url = match file_type {
        FileType::Remote => Some(validate_remote_attachment_url(
            remote_url.as_deref().unwrap_or_default(),
        )?),
        _ => None,
    };
    Ok(Some(PreparedAttachment {
        file_type,
        file_hash,
        remote_url,
        upload_stage: None,
    }))
}

/// Swap the owner FK, attachment row, and old local-file reference as one
/// definition transaction. The retained game/challenge row locks linearize the
/// mutation with durable hard-deletion fences on either row.
async fn replace_attachment_locked(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    game_id: i32,
    challenge_id: i32,
    prepared: Option<&PreparedAttachment>,
) -> AppResult<AttachmentSwap> {
    super::deletion::reject_pending_mutation(&mut **transaction, game_id, challenge_id).await?;
    let (challenge_type, old_attachment_id, old_file_type, old_hash) =
        sqlx::query_as::<_, (i16, Option<i32>, Option<i16>, Option<String>)>(
            r#"SELECT challenge."Type", challenge.attachment_id,
                      attachment."Type", file.hash
                 FROM "GameChallenges" challenge
                 LEFT JOIN "Attachments" attachment
                   ON attachment.id = challenge.attachment_id
                 LEFT JOIN "Files" file ON file.id = attachment.local_file_id
                WHERE challenge.id = $1 AND challenge.game_id = $2
                FOR UPDATE OF challenge"#,
        )
        .bind(challenge_id)
        .bind(game_id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .ok_or_else(|| AppError::not_found("Challenge not found"))?;
    if challenge_type == ChallengeType::DynamicAttachment as i16 {
        return Err(AppError::bad_request(
            "Use the assets API for dynamic-attachment challenges",
        ));
    }

    if let (Some(old_attachment_id), Some(prepared), Some(stage)) = (
        old_attachment_id,
        prepared,
        prepared.and_then(|value| value.upload_stage.as_ref()),
    ) {
        let same_local_content = prepared.file_type == FileType::Local
            && old_file_type == Some(FileType::Local as i16)
            && old_hash.as_deref() == Some(stage.blob.hash.as_str());
        if same_local_content {
            stage
                .consume_with_existing_reference_as(
                    transaction,
                    &attachment_publication_scope(old_attachment_id),
                )
                .await?;
            return Ok(AttachmentSwap {
                attachment_id: Some(old_attachment_id),
                deleted_hash: None,
            });
        }
    }

    let new_attachment_id = if let Some(prepared) = prepared {
        let attachment_id = sqlx::query_scalar::<_, i32>(
            r#"INSERT INTO "Attachments" ("Type", remote_url, local_file_id)
               VALUES ($1, $2, NULL)
               RETURNING id"#,
        )
        .bind(prepared.file_type as i16)
        .bind(prepared.remote_url.as_deref())
        .fetch_one(&mut **transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if let (FileType::Local, Some(stage)) = (prepared.file_type, prepared.upload_stage.as_ref())
        {
            let local_file_id = crate::services::blob_refs::publish_staged_blob_for_owner(
                transaction,
                stage,
                &attachment_publication_scope(attachment_id),
            )
            .await?;
            sqlx::query(r#"UPDATE "Attachments" SET local_file_id = $2 WHERE id = $1"#)
                .bind(attachment_id)
                .bind(local_file_id)
                .execute(&mut **transaction)
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
        }
        Some(attachment_id)
    } else {
        None
    };

    let updated = sqlx::query(
        r#"UPDATE "GameChallenges"
              SET attachment_id = $3
            WHERE id = $1 AND game_id = $2
              AND deletion_pending = FALSE"#,
    )
    .bind(challenge_id)
    .bind(game_id)
    .bind(new_attachment_id)
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if updated.rows_affected() != 1 {
        return Err(AppError::conflict("Challenge is being deleted"));
    }

    let deleted_hash = match old_attachment_id {
        Some(old_attachment_id) if Some(old_attachment_id) != new_attachment_id => {
            crate::services::blob_refs::delete_attachment_locked(transaction, old_attachment_id)
                .await?
        }
        _ => None,
    };
    Ok(AttachmentSwap {
        attachment_id: new_attachment_id,
        deleted_hash,
    })
}

fn attachment_publication_scope(attachment_id: i32) -> String {
    format!("attachment:{attachment_id}")
}

pub(crate) fn validate_remote_attachment_url(raw: &str) -> AppResult<String> {
    let raw = raw.trim();
    let parsed = reqwest::Url::parse(raw)
        .map_err(|_| AppError::bad_request("remote attachment URL must be absolute http(s)"))?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none_or(str::is_empty)
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(AppError::bad_request(
            "remote attachment URL must be absolute http(s) without userinfo",
        ));
    }
    Ok(raw.to_string())
}

#[cfg(test)]
#[path = "attachments_tests.rs"]
mod tests;
