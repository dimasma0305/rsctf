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
    let am = game_notice::ActiveModel {
        game_id: Set(id),
        notice_type: Set(NoticeType::Normal),
        values: Set(serde_json::json!([model.content])),
        publish_time_utc: Set(publish),
        ..Default::default()
    };
    let created = am.insert(&st.db).await?;

    // RSCTF broadcasts (IUserClient.ReceivedGameNotice) only when the notice is
    // already live — a future-dated notice is delivered by the scheduler later.
    if publish <= now {
        st.publish_event(
            "ReceivedGameNotice",
            Some(created.game_id),
            serde_json::json!({
                "type": created.notice_type,
                "values": created.values.clone(),
                "id": created.id,
                "time": created.publish_time_utc,
            })
            .to_string(),
        );
    }

    Ok(RequestResponse::ok(GameNoticeDetailModel::from_model(
        created,
    )))
}

/// `PUT /api/edit/games/{id}/notices/{noticeId}`
pub async fn update_notice(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, notice_id)): Path<(i32, i32)>,
    Json(model): Json<GameNoticeModel>,
) -> AppResult<RequestResponse<GameNoticeDetailModel>> {
    manager_or_admin(&st, &user, id).await?;
    let notice = game_notice::Entity::find()
        .filter(game_notice::Column::Id.eq(notice_id))
        .filter(game_notice::Column::GameId.eq(id))
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Notice not found"))?;
    if notice.notice_type != NoticeType::Normal {
        return Err(AppError::bad_request("System notices are not editable"));
    }
    let mut am: game_notice::ActiveModel = notice.into();
    am.values = Set(serde_json::json!([model.content]));
    if let Some(at) = model.publish_at {
        am.publish_time_utc = Set(at);
    }
    let updated = am.update(&st.db).await?;
    Ok(RequestResponse::ok(GameNoticeDetailModel::from_model(
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
    let notice = game_notice::Entity::find()
        .filter(game_notice::Column::Id.eq(notice_id))
        .filter(game_notice::Column::GameId.eq(id))
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Notice not found"))?;
    if notice.notice_type != NoticeType::Normal {
        return Err(AppError::bad_request("System notices are not deletable"));
    }
    game_notice::Entity::delete_by_id(notice_id)
        .exec(&st.db)
        .await?;
    Ok(MessageResponse::ok(""))
}

// ============================================================================
//  Divisions
// ============================================================================
