//! Challenge attachment mutation and validation.

use super::*;

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
    let challenge = load_challenge(&st, id, c_id).await?;
    if challenge.challenge_type == ChallengeType::DynamicAttachment {
        return Err(AppError::bad_request(
            "Use the assets API for dynamic-attachment challenges",
        ));
    }

    let new_id = build_attachment(
        &st,
        model.attachment_type,
        model.file_hash,
        model.remote_url,
    )
    .await?;

    // Detach the challenge from its previous attachment FIRST, then release the
    // orphaned attachment + its ref-counted blob (clear-FK-first).
    let old_attachment_id = challenge.attachment_id;
    let mut am: game_challenge::ActiveModel = challenge.into();
    am.attachment_id = Set(new_id);
    am.update(&st.db).await?;

    if let Some(old) = old_attachment_id {
        if Some(old) != new_id {
            delete_attachment(&st, old).await?;
        }
    }

    Ok(RequestResponse::ok(new_id.unwrap_or(0)))
}

/// Materialize an `Attachment` row from the wire model, resolving a `Local`
/// file by hash. `None`/absent type means "no attachment" (returns `None`).
/// Mirrors `AttachmentCreateModel.ToAttachment`.
pub(crate) async fn build_attachment(
    st: &SharedState,
    file_type: Option<FileType>,
    file_hash: Option<String>,
    remote_url: Option<String>,
) -> AppResult<Option<i32>> {
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
    let local_file_id = match (file_type, file_hash) {
        (FileType::Local, Some(hash)) if !hash.is_empty() => local_file::Entity::find()
            .filter(local_file::Column::Hash.eq(hash))
            .one(&st.db)
            .await?
            .map(|f| f.id),
        _ => None,
    };
    let am = attachment::ActiveModel {
        file_type: Set(file_type),
        remote_url: Set(remote_url),
        local_file_id: Set(local_file_id),
        ..Default::default()
    };
    let created = am.insert(&st.db).await?;
    Ok(Some(created.id))
}

fn validate_remote_attachment_url(raw: &str) -> AppResult<String> {
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
mod tests {
    use super::*;

    #[test]
    fn remote_attachments_require_absolute_http_urls() {
        assert!(validate_remote_attachment_url("https://files.example/challenge.zip").is_ok());
        assert!(validate_remote_attachment_url("http://files.example/challenge.zip").is_ok());
        for invalid in [
            "javascript:alert(1)",
            "data:text/html,pwn",
            "/relative/file",
            "https://user:pass@files.example/file",
            "",
        ] {
            assert!(validate_remote_attachment_url(invalid).is_err());
        }
    }
}
