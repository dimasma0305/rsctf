//! edit: posts CRUD (see edit/mod.rs for the router + shared DTOs/helpers).
use super::*;
use crate::models::data::post;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PostEditModel {
    pub operation_id: Option<Uuid>,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub content: Option<String>,
    pub tags: Option<Vec<String>>,
    pub is_pinned: Option<bool>,
}

/// Outbound editor view for a post.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PostDetailModel {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub content: String,
    pub is_pinned: bool,
    pub tags: Option<Vec<String>>,
    pub author_avatar: Option<String>,
    pub author_name: Option<String>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub time: DateTime<Utc>,
}

impl PostDetailModel {
    fn from_post(p: &post::Model, author_name: Option<String>) -> Self {
        let tags = p
            .tags
            .as_ref()
            .and_then(|value| serde_json::from_value::<Vec<String>>(value.clone()).ok());
        Self {
            id: p.id.clone(),
            title: p.title.clone(),
            summary: p.summary.clone(),
            content: p.content.clone(),
            is_pinned: p.is_pinned,
            tags,
            author_avatar: None,
            author_name,
            time: p.update_time_utc,
        }
    }
}

/// `POST /api/edit/posts`
pub async fn add_post(
    State(st): State<SharedState>,
    AdminUser(user): AdminUser,
    Json(model): Json<PostEditModel>,
) -> AppResult<RequestResponse<String>> {
    let operation_id =
        crate::services::create_operations::require_operation_id(model.operation_id)?;
    let mut digest_model = model.clone();
    digest_model.operation_id = None;
    let request_digest = sha256_str(
        &serde_json::to_string(&digest_model)
            .map_err(|error| AppError::internal(error.to_string()))?,
    );
    let mut transaction = st
        .pg()
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if let Some(id) = crate::services::create_operations::claim(
        &mut transaction,
        user.id,
        "post",
        0,
        operation_id,
        &request_digest,
    )
    .await?
    {
        transaction
            .commit()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(RequestResponse::ok(id));
    }
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

    let tags = model.tags.as_ref().map(sqlx::types::Json);
    sqlx::query(
        r#"INSERT INTO "Posts"
                  (id, title, summary, content, is_pinned, tags, author_id, update_time_utc)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"#,
    )
    .bind(&id)
    .bind(title)
    .bind(model.summary.unwrap_or_default())
    .bind(model.content.unwrap_or_default())
    .bind(model.is_pinned.unwrap_or(false))
    .bind(tags)
    .bind(user.id)
    .bind(now)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    crate::services::create_operations::complete(
        &mut transaction,
        user.id,
        "post",
        0,
        operation_id,
        &id,
    )
    .await?;
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
