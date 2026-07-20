//! Writeup submission state + PDF upload, with ref-counted blob helpers.
use super::*;

// ---------------------------------------------------------------------------
// Writeup
// ---------------------------------------------------------------------------

/// `GET /api/game/{id}/writeup` — writeup submission state for the caller's team.
pub async fn get_writeup(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<BasicWriteupInfoModel>> {
    let ctx = context_info(&st, &user, id, false).await?;

    // Resolve the participation's uploaded writeup blob (LocalFile), mirroring
    // RSCTF `BasicWriteupInfoModel.FromParticipation`.
    let file = match ctx.participation.writeup_id {
        Some(fid) => local_file::Entity::find_by_id(fid).one(&st.db).await?,
        None => None,
    };
    let model = BasicWriteupInfoModel {
        submitted: file.is_some(),
        name: file
            .as_ref()
            .map(|f| f.name.clone())
            .unwrap_or_else(|| "#".to_string()),
        file_size: file.as_ref().map(|f| f.file_size).unwrap_or(0),
        note: ctx.game.writeup_note.clone(),
    };
    Ok(RequestResponse::ok(model))
}

/// `POST /api/game/{id}/writeup` — upload a writeup PDF (multipart field `file`).
///
/// Mirrors RSCTF `SubmitWriteup`: validate the upload (non-empty, ≤20 MiB, PDF),
/// verify the play context and that the game requires a writeup and its deadline
/// has not passed, replace any previously uploaded blob, store the new one via
/// `st.storage`, and point `participation.writeup_id` at the fresh `Files` row.
pub async fn submit_writeup(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
    mut multipart: Multipart,
) -> AppResult<StatusCode> {
    let now = Utc::now();

    // Pull the `file` field; capture metadata before `bytes()` consumes it.
    let mut file_name: Option<String> = None;
    let mut content_type: Option<String> = None;
    let mut body: Option<axum::body::Bytes> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::bad_request(format!("multipart error: {e}")))?
    {
        if field.name() == Some("file") {
            file_name = field.file_name().map(|s| s.to_string());
            content_type = field.content_type().map(|s| s.to_string());
            body = Some(
                field
                    .bytes()
                    .await
                    .map_err(|e| AppError::bad_request(format!("could not read file: {e}")))?,
            );
            break;
        }
    }
    let bytes = body.ok_or_else(|| AppError::bad_request("No file provided"))?;

    // Validate size + PDF type (RSCTF SubmitWriteup guards).
    if bytes.is_empty() {
        return Err(AppError::bad_request("File is empty"));
    }
    if bytes.len() > 20 * 1024 * 1024 {
        return Err(AppError::bad_request("File is too large"));
    }
    // Extension check is case-SENSITIVE to match RSCTF's `Path.GetExtension(name)
    // != ".pdf"` (rejects `foo.PDF`); the client always sends a lowercase `.pdf`.
    let is_pdf = content_type.as_deref() == Some("application/pdf")
        && file_name.as_deref().is_some_and(|n| n.ends_with(".pdf"));
    if !is_pdf {
        return Err(AppError::bad_request("Only PDF files are accepted"));
    }

    let ctx = context_info(&st, &user, id, false).await?;
    if !ctx.game.writeup_required {
        return Err(AppError::bad_request(
            "Writeup is not required for this game",
        ));
    }
    if now > ctx.game.writeup_deadline {
        return Err(AppError::bad_request("Writeup deadline has passed"));
    }

    let name = format!(
        "Writeup-{}-{}-{}.pdf",
        ctx.game.id,
        ctx.participation.team_id,
        now.format("%Y%m%d-%H.%M.%S")
    );
    let (_blob, deleted_hash) = crate::services::blob_refs::store_and_replace_writeup(
        st.pg(),
        st.storage.as_ref(),
        ctx.game.id,
        ctx.participation.id,
        user.id,
        &name,
        &bytes,
    )
    .await?;

    let team_id = ctx.participation.team_id;
    let game_title = ctx.game.title.clone();

    // The FK swap and both refcount changes are already committed. Only then
    // touch physical storage, after a fresh hash lookup protects a blob that a
    // concurrent replica has reacquired.
    if let Some(old_hash) = deleted_hash {
        if let Err(error) = crate::services::blob_refs::purge_if_unreferenced(
            st.pg(),
            st.storage.as_ref(),
            &old_hash,
        )
        .await
        {
            tracing::warn!(%error, hash = %old_hash, "writeup blob purge failed");
        }
    }

    let team_name = team::Entity::find_by_id(team_id)
        .one(&st.db)
        .await
        .ok()
        .flatten()
        .map(|t| t.name)
        .unwrap_or_default();
    crate::services::audit::info(
        &st,
        "GameController",
        Some(user.name.clone()),
        None,
        format!("{team_name} successfully submitted {game_title} Write-Up"),
    )
    .await;

    Ok(StatusCode::OK)
}
