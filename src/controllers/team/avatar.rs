//! Team avatar upload handler.

use axum::extract::{Multipart, Path, State};

use super::{acquire_roster_mutation, load_team, require_captain};
use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::CurrentUser;
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::RequestResponse;

const MAX_AVATAR_BYTES: usize = crate::utils::upload::IMAGE_FILE_BYTES;

/// `PUT /api/team/{id}/avatar` (multipart, field `file`) — captain only.
pub async fn avatar(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
    mut multipart: Multipart,
) -> AppResult<RequestResponse<String>> {
    let team = load_team(&st, id).await?;
    require_captain(&team, &user)?;

    let mut data: Option<Vec<u8>> = None;
    let mut content_type: Option<String> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::bad_request(format!("multipart error: {e}")))?
    {
        if field.name() == Some("file") {
            // `content_type()` borrows the field; `bytes()` consumes it, so take
            // an owned copy of the declared type before reading the payload.
            content_type = field.content_type().map(|s| s.to_owned());
            let bytes = field
                .bytes()
                .await
                .map_err(|e| AppError::bad_request(format!("could not read file: {e}")))?;
            data = Some(bytes.to_vec());
            break;
        }
    }
    let bytes = data.ok_or_else(|| AppError::bad_request("No file provided"))?;
    if bytes.is_empty() || bytes.len() > MAX_AVATAR_BYTES {
        return Err(AppError::bad_request("Invalid avatar file size"));
    }
    // RSCTF pipes the upload through `CreateOrUpdateImage`, which returns null
    // (→ 400) for anything it cannot decode as an image. We have no image
    // decoder here, so at minimum require the part to declare an `image/*`
    // content-type and reject everything else.
    if !content_type
        .as_deref()
        .is_some_and(|ct| ct.starts_with("image/"))
    {
        return Err(AppError::bad_request("Avatar must be an image"));
    }

    // Multipart ingestion happens before retaining a pooled connection. Recheck
    // captaincy and the deletion fence under the same roster lock used by the
    // final team cascade, then commit the blob reference and avatar hash in that
    // transaction.
    let mut roster = acquire_roster_mutation(st.pg(), id).await?;
    let live = sqlx::query_as::<_, (Option<String>, String, uuid::Uuid, bool)>(
        r#"SELECT avatar_hash, name, captain_id, deletion_pending
              FROM "Teams" WHERE id = $1"#,
    )
    .bind(id)
    .fetch_optional(&mut **roster.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Team not found"))?;
    let (old_hash, team_name, captain_id, deletion_pending) = live;
    if captain_id != user.id {
        return Err(AppError::Forbidden);
    }
    if deletion_pending {
        return Err(AppError::conflict("Team is being deleted"));
    }
    let (blob, _) = crate::services::blob_refs::store_and_acquire_in_transaction(
        st.storage.as_ref(),
        roster.transaction_mut(),
        "avatar",
        &bytes,
    )
    .await?;
    sqlx::query(r#"UPDATE "Teams" SET avatar_hash = $2 WHERE id = $1"#)
        .bind(id)
        .bind(&blob.hash)
        .execute(&mut **roster.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    roster.release().await?;
    if let Some(old_hash) = old_hash {
        if let Err(error) =
            crate::services::blob_refs::release_and_purge(st.pg(), st.storage.as_ref(), &old_hash)
                .await
        {
            tracing::warn!(%error, hash = %old_hash, "old team avatar purge failed");
        }
    }

    // RSCTF `Team_AvatarUpdated` — "Team {name} changed avatar: [{hash8}]"
    // (TeamController, Success). The C# logs the first 8 chars of the blob hash.
    let hash8: String = blob.hash.chars().take(8).collect();
    crate::services::audit::info(
        &st,
        "TeamController",
        Some(user.name.clone()),
        None,
        format!("Team {} changed avatar: [{}]", team_name, hash8),
    )
    .await;

    Ok(RequestResponse::ok(format!("/assets/{}/avatar", blob.hash)))
}
