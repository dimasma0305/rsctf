//! edit: posts CRUD (see edit/mod.rs for the router + shared DTOs/helpers).
use super::*;

/// `POST /api/edit/posts`
pub async fn add_post(
    State(st): State<SharedState>,
    AdminUser(user): AdminUser,
    headers: HeaderMap,
    Json(model): Json<PostEditModel>,
) -> AppResult<RequestResponse<String>> {
    let operation_id = control_jobs::operation_id(&headers)?;
    let fingerprint = crate::services::mutation_operations::fingerprint("post-create", &model)?;
    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let replay = crate::services::mutation_operations::claim(
        &mut transaction,
        user.id,
        "post-create",
        "global",
        operation_id,
        fingerprint,
    )
    .await?;
    let id = if let Some(replay) = replay {
        replay.result_id
    } else {
        let now = Utc::now();
        // Post.UpdateKeyWithHash: sha256("{title}:{iso}:{uuid}")[4..12].
        let title = model.title.clone().unwrap_or_default();
        let seed = format!(
            "{}:{}:{}",
            title,
            now.format("%Y-%m-%dT%H:%M:%S"),
            uuid::Uuid::new_v4()
        );
        let id = sha256_str(&seed)[4..12].to_string();
        let tags = model
            .tags
            .as_ref()
            .map(|tags| serde_json::to_value(tags).unwrap_or(JsonValue::Null));
        sqlx::query(
            r#"INSERT INTO "Posts"
                 (id, title, summary, content, is_pinned, tags, author_id, update_time_utc)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"#,
        )
        .bind(&id)
        .bind(title)
        .bind(model.summary.clone().unwrap_or_default())
        .bind(model.content.clone().unwrap_or_default())
        .bind(model.is_pinned.unwrap_or(false))
        .bind(tags)
        .bind(user.id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        crate::services::mutation_operations::complete(
            &mut transaction,
            user.id,
            "post-create",
            "global",
            operation_id,
            &id,
            None,
        )
        .await?;
        id
    };
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(RequestResponse::ok(id))
}

/// `PUT /api/edit/posts/{id}` — returns the full `PostDetailModel`.
pub async fn update_post(
    State(st): State<SharedState>,
    AdminUser(user): AdminUser,
    Path(id): Path<String>,
    Json(model): Json<PostEditModel>,
) -> AppResult<RequestResponse<PostDetailModel>> {
    let existing = post::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Post not found"))?;

    let mut am: post::ActiveModel = existing.into();

    // Post.Update: a pin-only toggle must not disturb the other fields.
    if let Some(pinned) = model.is_pinned {
        am.is_pinned = Set(pinned);
    } else {
        if let Some(title) = model.title {
            am.title = Set(title);
        }
        if let Some(summary) = model.summary {
            am.summary = Set(summary);
        }
        if let Some(content) = model.content {
            am.content = Set(content);
        }
        if let Some(tags) = model.tags {
            am.tags = Set(Some(serde_json::to_value(tags).unwrap_or(JsonValue::Null)));
        }
        am.author_id = Set(Some(user.id));
        am.update_time_utc = Set(Utc::now());
    }

    let updated = am.update(&st.db).await?;
    Ok(RequestResponse::ok(PostDetailModel::from_post(
        &updated,
        Some(user.name.clone()),
    )))
}

/// `DELETE /api/edit/posts/{id}` — void.
pub async fn delete_post(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<String>,
) -> AppResult<MessageResponse> {
    let res = post::Entity::delete_by_id(id).exec(&st.db).await?;
    if res.rows_affected == 0 {
        return Err(AppError::not_found("Post not found"));
    }
    Ok(MessageResponse::ok(""))
}

// ============================================================================
//  Games
// ============================================================================
