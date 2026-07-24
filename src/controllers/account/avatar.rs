//! Account avatar upload and ref-counted blob replacement.

use super::*;
use axum::extract::Multipart;

/// `PUT /api/account/avatar` (multipart, field `file`) -> raw avatar URL string.
pub async fn avatar(
    State(st): State<SharedState>,
    user: CurrentUser,
    mut multipart: Multipart,
) -> AppResult<RequestResponse<String>> {
    let mut data: Option<Vec<u8>> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::bad_request(format!("multipart error: {e}")))?
    {
        if field.name() == Some("file") {
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

    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let old_hash = sqlx::query_as::<_, (Option<String>,)>(
        r#"SELECT avatar_hash FROM "AspNetUsers" WHERE id = $1 FOR UPDATE"#,
    )
    .bind(user.id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("User not found"))?
    .0;
    let (blob, _) = crate::services::blob_refs::store_and_acquire_in_transaction(
        st.storage.as_ref(),
        &mut transaction,
        "avatar",
        &bytes,
    )
    .await?;
    sqlx::query(r#"UPDATE "AspNetUsers" SET avatar_hash = $2 WHERE id = $1"#)
        .bind(user.id)
        .bind(&blob.hash)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if let Some(old_hash) = old_hash {
        if let Err(error) =
            crate::services::blob_refs::release_and_purge(st.pg(), st.storage.as_ref(), &old_hash)
                .await
        {
            tracing::warn!(%error, hash = %old_hash, "old user avatar purge failed");
        }
    }

    crate::services::audit::info(
        &st,
        "AccountController",
        Some(user.name.clone()),
        None,
        format!("User {} updated avatar", user.name),
    )
    .await;

    Ok(RequestResponse::ok(format!("/assets/{}/avatar", blob.hash)))
}
