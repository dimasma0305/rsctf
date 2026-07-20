//! edit: game notices (see edit/mod.rs for the router + shared DTOs/helpers).
use super::*;

/// RSCTF `GameNotice` (Api.ts) — camelCase wire shape with a Unix-millis `time`.
/// The raw `game_notice::Model` is snake_case, leaks `gameId`, and emits an
/// ISO-8601 date, so every notice handler maps through this DTO instead.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameNoticeDetailModel {
    pub id: i32,
    #[serde(rename = "type")]
    pub notice_type: NoticeType,
    pub values: JsonValue,
    #[serde(with = "crate::utils::datetime::millis")]
    pub time: DateTime<Utc>,
}

impl GameNoticeDetailModel {
    fn from_model(m: game_notice::Model) -> Self {
        Self {
            id: m.id,
            notice_type: m.notice_type,
            values: m.values,
            time: m.publish_time_utc,
        }
    }

    fn from_normal_row(row: (i32, JsonValue, DateTime<Utc>)) -> Self {
        Self {
            id: row.0,
            notice_type: NoticeType::Normal,
            values: row.1,
            time: row.2,
        }
    }
}

/// `GET /api/edit/games/{id}/notices`
pub async fn get_notices(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<Vec<GameNoticeDetailModel>>> {
    manager_or_admin(&st, &user, id).await?;
    load_game(&st, id).await?;
    let notices = game_notice::Entity::find()
        .filter(game_notice::Column::GameId.eq(id))
        .filter(game_notice::Column::NoticeType.eq(NoticeType::Normal))
        .order_by_desc(game_notice::Column::PublishTimeUtc)
        .all(&st.db)
        .await?;
    let dtos = notices
        .into_iter()
        .map(GameNoticeDetailModel::from_model)
        .collect();
    Ok(RequestResponse::ok(dtos))
}

/// `POST /api/edit/games/{id}/notices`
pub async fn add_notice(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
    Json(model): Json<GameNoticeModel>,
) -> AppResult<RequestResponse<GameNoticeDetailModel>> {
    manager_or_admin(&st, &user, id).await?;
    load_game(&st, id).await?;
    let now = Utc::now();
    let publish = match model.publish_at {
        Some(at) if at > now => at,
        _ => now,
    };
    let values = serde_json::json!([model.content]);
    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await?;
    require_game_mutable(control.transaction_mut(), id).await?;
    let created: (i32, JsonValue, DateTime<Utc>) = sqlx::query_as(
        r#"INSERT INTO "GameNotices" (game_id, "Type", values, publish_time_utc)
           VALUES ($1, $2, $3, $4)
           RETURNING id, values, publish_time_utc"#,
    )
    .bind(id)
    .bind(NoticeType::Normal as i16)
    .bind(&values)
    .bind(publish)
    .fetch_one(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let created = GameNoticeDetailModel::from_normal_row(created);

    // RSCTF broadcasts (IUserClient.ReceivedGameNotice) only when the notice is
    // already live — a future-dated notice is delivered by the scheduler later.
    if publish <= now {
        st.publish_event(
            "ReceivedGameNotice",
            Some(id),
            serde_json::json!({
                "type": created.notice_type,
                "values": created.values.clone(),
                "id": created.id,
                "time": created.time,
            })
            .to_string(),
        );
    }

    Ok(RequestResponse::ok(created))
}

/// `PUT /api/edit/games/{id}/notices/{noticeId}`
pub async fn update_notice(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, notice_id)): Path<(i32, i32)>,
    Json(model): Json<GameNoticeModel>,
) -> AppResult<RequestResponse<GameNoticeDetailModel>> {
    manager_or_admin(&st, &user, id).await?;
    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await?;
    require_game_mutable(control.transaction_mut(), id).await?;
    let notice_type = sqlx::query_scalar::<_, i16>(
        r#"SELECT "Type" FROM "GameNotices"
            WHERE id = $1 AND game_id = $2
            FOR UPDATE"#,
    )
    .bind(notice_id)
    .bind(id)
    .fetch_optional(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Notice not found"))?;
    if notice_type != NoticeType::Normal as i16 {
        return Err(AppError::bad_request("System notices are not editable"));
    }
    let updated: (i32, JsonValue, DateTime<Utc>) = sqlx::query_as(
        r#"UPDATE "GameNotices"
              SET values = $3,
                  publish_time_utc = COALESCE($4, publish_time_utc)
            WHERE id = $1 AND game_id = $2
        RETURNING id, values, publish_time_utc"#,
    )
    .bind(notice_id)
    .bind(id)
    .bind(serde_json::json!([model.content]))
    .bind(model.publish_at)
    .fetch_one(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(RequestResponse::ok(GameNoticeDetailModel::from_normal_row(
        updated,
    )))
}

/// `DELETE /api/edit/games/{id}/notices/{noticeId}` — void.
pub async fn delete_notice(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, notice_id)): Path<(i32, i32)>,
) -> AppResult<MessageResponse> {
    manager_or_admin(&st, &user, id).await?;
    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await?;
    require_game_mutable(control.transaction_mut(), id).await?;
    let notice_type = sqlx::query_scalar::<_, i16>(
        r#"SELECT "Type" FROM "GameNotices"
            WHERE id = $1 AND game_id = $2
            FOR UPDATE"#,
    )
    .bind(notice_id)
    .bind(id)
    .fetch_optional(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Notice not found"))?;
    if notice_type != NoticeType::Normal as i16 {
        return Err(AppError::bad_request("System notices are not deletable"));
    }
    sqlx::query(r#"DELETE FROM "GameNotices" WHERE id = $1 AND game_id = $2"#)
        .bind(notice_id)
        .bind(id)
        .execute(&mut **control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(MessageResponse::ok(""))
}

// ============================================================================
//  Divisions
// ============================================================================
