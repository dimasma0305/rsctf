//! Team avatar upload handler.

use axum::extract::{Multipart, Path, State};
use axum::http::HeaderMap;

use super::{acquire_profile_mutation, ensure_roster_change_allowed};
use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::CurrentUser;
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::RequestResponse;

const MAX_AVATAR_BYTES: usize = crate::utils::upload::IMAGE_FILE_BYTES;

/// `PUT /api/team/{id}/avatar` (multipart, field `file`) — captain only.
pub async fn avatar(
    State(st): State<SharedState>,
    user: CurrentUser,
    headers: HeaderMap,
    Path(id): Path<i32>,
    mut multipart: Multipart,
) -> AppResult<RequestResponse<String>> {
    // Reject unauthorised, deletion-fenced, known-frozen, and over-budget
    // requests before Axum reads any multipart field. This connection is not a
    // transaction and is released before body or storage I/O; the retained
    // roster transaction below repeats every authoritative check.
    let mut preflight = st
        .pg()
        .acquire()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let owner = sqlx::query_as::<_, (uuid::Uuid, bool)>(
        r#"SELECT captain_id, deletion_pending FROM "Teams" WHERE id = $1"#,
    )
    .bind(id)
    .fetch_optional(&mut *preflight)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Team not found"))?;
    if owner.0 != user.id {
        return Err(AppError::Forbidden);
    }
    if owner.1 {
        return Err(AppError::conflict("Team is being deleted"));
    }
    let header_operation_id = headers
        .get("x-rsctf-operation-id")
        .map(|value| {
            value
                .to_str()
                .ok()
                .and_then(|value| value.parse::<uuid::Uuid>().ok())
                .filter(|operation_id| !operation_id.is_nil())
                .ok_or_else(|| AppError::bad_request("Invalid avatar operation ID"))
        })
        .transpose()?;
    let known_retry = if let Some(operation_id) = header_operation_id {
        sqlx::query_scalar::<_, bool>(
            r#"SELECT EXISTS(
                   SELECT 1 FROM "TeamProfileOperations"
                    WHERE operation_id = $1 AND team_id = $2 AND actor_user_id = $3
               )"#,
        )
        .bind(operation_id)
        .bind(id)
        .bind(user.id)
        .fetch_one(&mut *preflight)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
    } else {
        false
    };
    if !known_retry {
        super::profile::enforce_mutation_budget(&mut preflight, id, user.id).await?;
    }
    super::roster_policy::preflight_roster_change_allowed(&mut preflight, id).await?;
    drop(preflight);
    let _upload_reservation =
        crate::utils::upload::reserve_buffered(crate::utils::upload::IMAGE_BODY_BYTES)?;

    let mut data: Option<Vec<u8>> = None;
    let mut content_type: Option<String> = None;
    let mut operation_id = None;
    let mut profile_revision = None;
    let mut fields = 0_usize;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::bad_request(format!("multipart error: {e}")))?
    {
        fields += 1;
        if fields > 3 {
            return Err(AppError::bad_request("Unexpected avatar upload fields"));
        }
        let name = field.name().unwrap_or_default().to_string();
        match name.as_str() {
            "file" => {
                if data.is_some() {
                    return Err(AppError::bad_request("Duplicate avatar file field"));
                }
                content_type = field.content_type().map(|s| s.to_owned());
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|e| AppError::bad_request(format!("could not read file: {e}")))?;
                data = Some(bytes.to_vec());
            }
            "operationId" => {
                if operation_id.is_some() {
                    return Err(AppError::bad_request("Duplicate operationId field"));
                }
                let value = field
                    .text()
                    .await
                    .map_err(|e| AppError::bad_request(format!("invalid operationId: {e}")))?;
                operation_id = Some(
                    value
                        .parse::<uuid::Uuid>()
                        .map_err(|_| AppError::bad_request("Invalid operationId"))?,
                );
            }
            "profileRevision" => {
                if profile_revision.is_some() {
                    return Err(AppError::bad_request("Duplicate profileRevision field"));
                }
                let value = field
                    .text()
                    .await
                    .map_err(|e| AppError::bad_request(format!("invalid profileRevision: {e}")))?;
                profile_revision = Some(
                    value
                        .parse::<i64>()
                        .ok()
                        .filter(|revision| *revision >= 0)
                        .ok_or_else(|| AppError::bad_request("Invalid profileRevision"))?,
                );
            }
            _ => return Err(AppError::bad_request("Unexpected avatar upload field")),
        }
    }
    let operation_id = operation_id
        .filter(|operation_id| !operation_id.is_nil())
        .ok_or_else(|| AppError::bad_request("operationId must be a non-zero UUID"))?;
    if header_operation_id.is_some_and(|header| header != operation_id) {
        return Err(AppError::bad_request(
            "Avatar operation header does not match the multipart operationId",
        ));
    }
    let profile_revision =
        profile_revision.ok_or_else(|| AppError::bad_request("profileRevision is required"))?;
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

    let content_hash = crate::utils::codec::sha256_hex(&bytes);
    let request_digest = avatar_request_digest(id, profile_revision, &content_hash);
    let result_url = format!("/assets/{content_hash}/avatar");

    // Reconcile exact retries and same-content no-ops before touching storage.
    let mut roster = acquire_profile_mutation(st.pg(), id).await?;
    let initial = sqlx::query_as::<_, (Option<String>, uuid::Uuid, bool, i64)>(
        r#"SELECT avatar_hash, captain_id, deletion_pending, profile_revision
              FROM "Teams" WHERE id = $1"#,
    )
    .bind(id)
    .fetch_optional(&mut **roster.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Team not found"))?;
    if initial.1 != user.id {
        return Err(AppError::Forbidden);
    }
    if initial.2 {
        return Err(AppError::conflict("Team is being deleted"));
    }
    if let Some(result) = super::profile::replay_avatar_operation(
        &mut **roster.transaction_mut(),
        operation_id,
        id,
        user.id,
        &request_digest,
    )
    .await?
    {
        roster.release().await?;
        return Ok(RequestResponse::ok(result));
    }
    ensure_roster_change_allowed(roster.transaction_mut(), id).await?;
    if initial.3 != profile_revision {
        return Err(AppError::conflict(
            "Team profile changed in another request; reload and try again",
        ));
    }
    super::profile::enforce_mutation_budget(&mut **roster.transaction_mut(), id, user.id).await?;
    if initial.0.as_deref() == Some(content_hash.as_str()) {
        super::profile::store_avatar_operation(
            &mut **roster.transaction_mut(),
            operation_id,
            id,
            user.id,
            &request_digest,
            profile_revision,
            profile_revision,
            &result_url,
        )
        .await?;
        roster.release().await?;
        return Ok(RequestResponse::ok(result_url));
    }
    roster.release().await?;

    let staged = crate::services::blob_refs::stage_blob(
        st.pg(),
        st.storage.as_ref(),
        crate::services::blob_refs::scoped_operation_id(
            operation_id,
            &format!("team-avatar:{id}"),
            0,
        ),
        &format!("team-avatar:{id}"),
        Some(user.id),
        "avatar",
        &bytes,
    )
    .await?;

    // Multipart ingestion happens before retaining a pooled connection. Recheck
    // captaincy and the deletion fence under the same roster lock used by the
    // final team cascade, then commit the blob reference and avatar hash in that
    // transaction.
    let mut roster = acquire_profile_mutation(st.pg(), id).await?;
    let live = sqlx::query_as::<_, (Option<String>, String, uuid::Uuid, bool, i64)>(
        r#"SELECT avatar_hash, name, captain_id, deletion_pending, profile_revision
              FROM "Teams" WHERE id = $1"#,
    )
    .bind(id)
    .fetch_optional(&mut **roster.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Team not found"))?;
    let (old_hash, team_name, captain_id, deletion_pending, current_revision) = live;
    if captain_id != user.id {
        return Err(AppError::Forbidden);
    }
    if deletion_pending {
        return Err(AppError::conflict("Team is being deleted"));
    }
    if let Some(result) = super::profile::replay_avatar_operation(
        &mut **roster.transaction_mut(),
        operation_id,
        id,
        user.id,
        &request_digest,
    )
    .await?
    {
        roster.release().await?;
        return Ok(RequestResponse::ok(result));
    }
    ensure_roster_change_allowed(roster.transaction_mut(), id).await?;
    if current_revision != profile_revision {
        return Err(AppError::conflict(
            "Team profile changed while the avatar was stored; reload and try again",
        ));
    }
    super::profile::enforce_mutation_budget(&mut **roster.transaction_mut(), id, user.id).await?;
    crate::services::blob_refs::lock_direct_hashes_locked(
        roster.transaction_mut(),
        std::iter::once(staged.blob.hash.as_str()).chain(old_hash.as_deref()),
    )
    .await?;
    crate::services::blob_refs::publish_staged_blob(roster.transaction_mut(), &staged).await?;
    let blob = staged.blob;
    let revision = sqlx::query_scalar::<_, i64>(
        r#"UPDATE "Teams"
              SET avatar_hash = $2, profile_revision = profile_revision + 1
            WHERE id = $1 AND profile_revision = $3
        RETURNING profile_revision"#,
    )
    .bind(id)
    .bind(&blob.hash)
    .bind(profile_revision)
    .fetch_optional(&mut **roster.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::conflict("Team profile changed; reload and try again"))?;
    super::profile::store_avatar_operation(
        &mut **roster.transaction_mut(),
        operation_id,
        id,
        user.id,
        &request_digest,
        profile_revision,
        revision,
        &result_url,
    )
    .await?;
    super::profile::enqueue_invalidation(&mut **roster.transaction_mut(), id, revision).await?;
    if let Some(old_hash) = old_hash.as_deref() {
        crate::services::blob_refs::release_direct_hash_locked(roster.transaction_mut(), old_hash)
            .await?;
    }
    roster.release().await?;
    crate::controllers::assets::invalidate_asset_gate(&st, &blob.hash).await;
    if let Some(old_hash) = old_hash {
        crate::controllers::assets::invalidate_asset_gate(&st, &old_hash).await;
        if let Err(error) = crate::services::blob_refs::purge_if_unreferenced(
            st.pg(),
            st.storage.as_ref(),
            &old_hash,
        )
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

    let effects_state = st.clone();
    tokio::spawn(async move {
        if let Err(error) = super::profile::process_profile_invalidations(&effects_state).await {
            tracing::warn!(%error, "team avatar invalidation failed");
        }
    });

    Ok(RequestResponse::ok(result_url))
}

fn avatar_request_digest(team_id: i32, profile_revision: i64, content_hash: &str) -> String {
    crate::utils::codec::sha256_str(&format!(
        "team-avatar:v1:{team_id}:{profile_revision}:{content_hash}"
    ))
}

#[cfg(test)]
mod tests {
    use super::avatar_request_digest;

    #[test]
    fn avatar_identity_binds_team_revision_and_content() {
        let hash = "a".repeat(64);
        let digest = avatar_request_digest(7, 3, &hash);
        assert_eq!(digest.len(), 64);
        assert_ne!(digest, avatar_request_digest(8, 3, &hash));
        assert_ne!(digest, avatar_request_digest(7, 4, &hash));
        assert_ne!(digest, avatar_request_digest(7, 3, &"b".repeat(64)));
    }
}
