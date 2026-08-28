//! edit: game notices (see edit/mod.rs for the router + shared DTOs/helpers).
use super::*;
use sha2::{Digest, Sha256};

pub const MAX_NORMAL_NOTICE_BYTES: usize = 48 * 1024;

const NOTICE_EVENT_PUBLISH: i16 = 0;
const NOTICE_EVENT_CHANGED: i16 = 1;

/// RSCTF `GameNotice` (Api.ts) — camelCase wire shape with a Unix-millis `time`.
/// The raw `game_notice::Model` is snake_case, leaks `gameId`, and emits an
/// ISO-8601 date, so every notice handler maps through this DTO instead.
#[derive(Clone, Debug, Deserialize, Serialize)]
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

fn validated_content(content: String) -> AppResult<String> {
    if content.trim().is_empty() {
        return Err(AppError::bad_request("Notice content is required"));
    }
    if content.len() > MAX_NORMAL_NOTICE_BYTES {
        return Err(AppError::payload_too_large(format!(
            "Notice content must be at most {MAX_NORMAL_NOTICE_BYTES} UTF-8 bytes"
        )));
    }
    Ok(content)
}

fn operation_fingerprint(
    mutation: u8,
    notice_id: Option<i32>,
    content: &str,
    publish_at: &Option<Option<DateTime<Utc>>>,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"rsctf:normal-notice-operation:v1\0");
    digest.update([mutation]);
    digest.update(notice_id.unwrap_or_default().to_be_bytes());
    digest.update((content.len() as u64).to_be_bytes());
    digest.update(content.as_bytes());
    match publish_at {
        None => digest.update([0]),
        Some(None) => digest.update([1]),
        Some(Some(value)) => {
            digest.update([2]);
            digest.update(value.timestamp_millis().to_be_bytes());
        }
    }
    digest.finalize().into()
}

async fn claim_operation(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    game_id: i32,
    operation_id: Uuid,
    fingerprint: [u8; 32],
) -> AppResult<Option<GameNoticeDetailModel>> {
    let inserted = sqlx::query(
        r#"INSERT INTO "GameNoticeOperations"
             (game_id, operation_id, request_fingerprint)
           VALUES ($1, $2, $3)
           ON CONFLICT (game_id, operation_id) DO NOTHING"#,
    )
    .bind(game_id)
    .bind(operation_id)
    .bind(fingerprint.as_slice())
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if inserted.rows_affected() == 1 {
        return Ok(None);
    }
    let (stored_fingerprint, result): (Vec<u8>, Option<JsonValue>) = sqlx::query_as(
        r#"SELECT request_fingerprint, result
             FROM "GameNoticeOperations"
            WHERE game_id = $1 AND operation_id = $2
            FOR UPDATE"#,
    )
    .bind(game_id)
    .bind(operation_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if stored_fingerprint.as_slice() != fingerprint.as_slice() {
        return Err(AppError::conflict(
            "operationId was already used for different notice content",
        ));
    }
    let result = result.ok_or_else(|| AppError::conflict("Notice operation is still pending"))?;
    serde_json::from_value(result)
        .map(Some)
        .map_err(|error| AppError::internal(format!("decode notice operation result: {error}")))
}

async fn complete_operation(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    game_id: i32,
    operation_id: Uuid,
    result: &GameNoticeDetailModel,
) -> AppResult<()> {
    sqlx::query(
        r#"UPDATE "GameNoticeOperations"
              SET result = $3, completed_at_utc = clock_timestamp()
            WHERE game_id = $1 AND operation_id = $2 AND result IS NULL"#,
    )
    .bind(game_id)
    .bind(operation_id)
    .bind(
        serde_json::to_value(result)
            .map_err(|error| AppError::internal(format!("encode notice result: {error}")))?,
    )
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

async fn cancel_pending_notice_delivery(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    game_id: i32,
    notice_id: i32,
) -> AppResult<()> {
    sqlx::query(
        r#"UPDATE "GameNoticeOutbox"
              SET delivered_at_utc = clock_timestamp(),
                  claim_token = NULL, claimed_at_utc = NULL
            WHERE game_id = $1 AND notice_id = $2 AND delivered_at_utc IS NULL"#,
    )
    .bind(game_id)
    .bind(notice_id)
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

async fn enqueue_notice_event(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    game_id: i32,
    notice_id: Option<i32>,
    operation_id: Uuid,
    event_kind: i16,
    payload: JsonValue,
    available_at: DateTime<Utc>,
) -> AppResult<()> {
    sqlx::query(
        r#"INSERT INTO "GameNoticeOutbox"
             (game_id, notice_id, operation_id, event_kind, payload, available_at_utc)
           VALUES ($1, $2, $3, $4, $5, $6)
           ON CONFLICT (game_id, operation_id, event_kind) DO NOTHING"#,
    )
    .bind(game_id)
    .bind(notice_id)
    .bind(operation_id)
    .bind(event_kind)
    .bind(payload)
    .bind(available_at)
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

async fn prompt_notice_delivery(st: &SharedState) {
    if let Err(error) = crate::services::notice_delivery::reconcile_once(st).await {
        tracing::debug!(%error, "normal-notice prompt delivery deferred to reconciler");
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
    let content = validated_content(model.content)?;
    let operation_id = model.operation_id;
    if operation_id.is_nil() {
        return Err(AppError::bad_request("operationId must be opaque"));
    }
    let fingerprint = operation_fingerprint(0, None, &content, &model.publish_at);
    let now = Utc::now();
    let publish = match model.publish_at.flatten() {
        Some(at) if at > now => at,
        _ => now,
    };
    let values = serde_json::json!([content]);
    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await?;
    require_game_mutable(control.transaction_mut(), id).await?;
    if let Some(replayed) =
        claim_operation(control.transaction_mut(), id, operation_id, fingerprint).await?
    {
        control
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(RequestResponse::ok(replayed));
    }
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
    let created = GameNoticeDetailModel::from_normal_row(created);
    enqueue_notice_event(
        control.transaction_mut(),
        id,
        Some(created.id),
        operation_id,
        NOTICE_EVENT_PUBLISH,
        serde_json::to_value(&created)
            .map_err(|error| AppError::internal(format!("encode notice event: {error}")))?,
        publish,
    )
    .await?;
    complete_operation(control.transaction_mut(), id, operation_id, &created).await?;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if publish <= now {
        prompt_notice_delivery(&st).await;
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
    let content = validated_content(model.content)?;
    let operation_id = model.operation_id;
    if operation_id.is_nil() {
        return Err(AppError::bad_request("operationId must be opaque"));
    }
    let fingerprint = operation_fingerprint(1, Some(notice_id), &content, &model.publish_at);
    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await?;
    require_game_mutable(control.transaction_mut(), id).await?;
    if let Some(replayed) =
        claim_operation(control.transaction_mut(), id, operation_id, fingerprint).await?
    {
        control
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(RequestResponse::ok(replayed));
    }
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
    cancel_pending_notice_delivery(control.transaction_mut(), id, notice_id).await?;
    let schedule_present = model.publish_at.is_some();
    let publish_at = model.publish_at.flatten();
    let updated: (i32, JsonValue, DateTime<Utc>) = sqlx::query_as(
        r#"UPDATE "GameNotices"
              SET values = $3,
                  publish_time_utc = CASE
                      WHEN $4 THEN COALESCE($5, clock_timestamp())
                      ELSE publish_time_utc
                  END
            WHERE id = $1 AND game_id = $2
        RETURNING id, values, publish_time_utc"#,
    )
    .bind(notice_id)
    .bind(id)
    .bind(serde_json::json!([content]))
    .bind(schedule_present)
    .bind(publish_at)
    .fetch_one(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let updated = GameNoticeDetailModel::from_normal_row(updated);
    let now = Utc::now();
    enqueue_notice_event(
        control.transaction_mut(),
        id,
        Some(notice_id),
        operation_id,
        NOTICE_EVENT_CHANGED,
        serde_json::json!({ "id": notice_id }),
        now,
    )
    .await?;
    enqueue_notice_event(
        control.transaction_mut(),
        id,
        Some(notice_id),
        operation_id,
        NOTICE_EVENT_PUBLISH,
        serde_json::to_value(&updated)
            .map_err(|error| AppError::internal(format!("encode notice event: {error}")))?,
        updated.time.max(now),
    )
    .await?;
    complete_operation(control.transaction_mut(), id, operation_id, &updated).await?;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    prompt_notice_delivery(&st).await;
    Ok(RequestResponse::ok(updated))
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
    cancel_pending_notice_delivery(control.transaction_mut(), id, notice_id).await?;
    sqlx::query(r#"DELETE FROM "GameNotices" WHERE id = $1 AND game_id = $2"#)
        .bind(notice_id)
        .bind(id)
        .execute(&mut **control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    enqueue_notice_event(
        control.transaction_mut(),
        id,
        None,
        Uuid::new_v4(),
        NOTICE_EVENT_CHANGED,
        serde_json::json!({ "id": notice_id }),
        Utc::now(),
    )
    .await?;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    prompt_notice_delivery(&st).await;
    Ok(MessageResponse::ok(""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_notice_limit_counts_utf8_bytes() {
        assert!(validated_content("x".repeat(MAX_NORMAL_NOTICE_BYTES)).is_ok());
        assert!(validated_content("x".repeat(MAX_NORMAL_NOTICE_BYTES + 1)).is_err());
        assert!(validated_content("é".repeat(MAX_NORMAL_NOTICE_BYTES / 2)).is_ok());
        assert!(
            validated_content(format!("{}é", "x".repeat(MAX_NORMAL_NOTICE_BYTES - 1))).is_err()
        );
        assert!(validated_content("   ".to_owned()).is_err());
    }

    #[test]
    fn operation_fingerprint_binds_mutation_schedule_and_content() {
        let immediate = operation_fingerprint(0, None, "notice", &Some(None));
        assert_eq!(
            immediate,
            operation_fingerprint(0, None, "notice", &Some(None))
        );
        assert_ne!(
            immediate,
            operation_fingerprint(1, Some(7), "notice", &Some(None))
        );
        assert_ne!(
            immediate,
            operation_fingerprint(0, None, "changed", &Some(None))
        );
        assert_ne!(immediate, operation_fingerprint(0, None, "notice", &None));
    }

    #[test]
    fn notice_wire_distinguishes_missing_and_cleared_schedule() {
        let missing: GameNoticeModel = serde_json::from_str(
            r#"{"content":"n","operationId":"00000000-0000-4000-8000-000000000001"}"#,
        )
        .unwrap();
        assert_eq!(missing.publish_at, None);
        let cleared: GameNoticeModel =
            serde_json::from_str(
                r#"{"content":"n","publishAt":null,"operationId":"00000000-0000-4000-8000-000000000001"}"#,
            )
            .unwrap();
        assert_eq!(cleared.publish_at, Some(None));
        let scheduled: GameNoticeModel = serde_json::from_str(
            r#"{"content":"n","publishAt":1787904000000,"operationId":"00000000-0000-4000-8000-000000000001"}"#,
        )
        .unwrap();
        assert!(scheduled.publish_at.flatten().is_some());
        assert!(!scheduled.operation_id.is_nil());
    }
}

// ============================================================================
//  Divisions
// ============================================================================
