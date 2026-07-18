//! edit: posts CRUD (see edit/mod.rs for the router + shared DTOs/helpers).
use super::*;

/// `POST /api/edit/posts`
pub async fn add_post(
    State(st): State<SharedState>,
    AdminUser(user): AdminUser,
    Json(model): Json<PostEditModel>,
) -> AppResult<RequestResponse<String>> {
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
        .map(|t| serde_json::to_value(t).unwrap_or(JsonValue::Null));

    let am = post::ActiveModel {
        id: Set(id.clone()),
        title: Set(title),
        summary: Set(model.summary.unwrap_or_default()),
        content: Set(model.content.unwrap_or_default()),
        is_pinned: Set(model.is_pinned.unwrap_or(false)),
        tags: Set(tags),
        author_id: Set(Some(user.id)),
        update_time_utc: Set(now),
    };
    am.insert(&st.db).await?;
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
