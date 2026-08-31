//! Account avatar upload and ref-counted blob replacement.

use super::*;
use axum::extract::Multipart;

/// `PUT /api/account/avatar` (multipart, field `file`) -> raw avatar URL string.
pub async fn avatar(
    State(st): State<SharedState>,
    user: CurrentUser,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> AppResult<RequestResponse<String>> {
    let operation_root = crate::utils::upload::required_operation_id(&headers)?;
    let _upload_reservation =
        crate::utils::upload::reserve_buffered(crate::utils::upload::IMAGE_BODY_BYTES)?;
    let mut data: Option<Vec<u8>> = None;
    let mut field_count = 0usize;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::bad_request(format!("multipart error: {e}")))?
    {
        field_count += 1;
        if field_count > crate::utils::upload::SINGLE_FILE_FIELD_COUNT {
            return Err(AppError::bad_request("Too many multipart fields"));
        }
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

    let staged = crate::services::blob_refs::stage_blob(
        st.pg(),
        st.storage.as_ref(),
        crate::services::blob_refs::scoped_operation_id(operation_root, "account-avatar", 0),
        "account-avatar",
        Some(user.id),
        "avatar",
        &bytes,
    )
    .await?;

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
    crate::services::blob_refs::lock_direct_hashes_locked(
        &mut transaction,
        std::iter::once(staged.blob.hash.as_str()).chain(old_hash.as_deref()),
    )
    .await?;
    let blob = staged.blob.clone();
    let replaced_hash = if old_hash.as_deref() == Some(blob.hash.as_str()) {
        staged
            .consume_with_existing_reference(&mut transaction)
            .await?;
        None
    } else {
        crate::services::blob_refs::publish_staged_blob(&mut transaction, &staged).await?;
        sqlx::query(r#"UPDATE "AspNetUsers" SET avatar_hash = $2 WHERE id = $1"#)
            .bind(user.id)
            .bind(&blob.hash)
            .execute(&mut *transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        if let Some(old_hash) = old_hash.as_deref() {
            crate::services::blob_refs::release_direct_hash_locked(&mut transaction, old_hash)
                .await?;
        }
        old_hash
    };
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    crate::controllers::assets::invalidate_asset_gate(&st, &blob.hash).await;
    if let Some(old_hash) = replaced_hash {
        crate::controllers::assets::invalidate_asset_gate(&st, &old_hash).await;
        if let Err(error) = crate::services::blob_refs::purge_if_unreferenced(
            st.pg(),
            st.storage.as_ref(),
            &old_hash,
        )
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
