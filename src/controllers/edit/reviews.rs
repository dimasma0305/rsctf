//! edit: poster/admins/reviews (see edit/mod.rs for the router + shared DTOs/helpers).
use super::*;

/// `PUT /api/edit/games/{id}/poster` — multipart image upload stored to blob
/// storage; the returned string is the poster asset URL.
pub async fn update_poster(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
    mut multipart: Multipart,
) -> AppResult<RequestResponse<String>> {
    manager_or_admin(&st, &user, id).await?;
    let game = load_game(&st, id).await?;

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
    if bytes.is_empty() {
        return Err(AppError::bad_request("File size is zero"));
    }
    if bytes.len() > 3 * 1024 * 1024 {
        return Err(AppError::bad_request("File is too large"));
    }

    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await?;
    require_game_mutable(control.transaction_mut(), id).await?;
    let old_hash = sqlx::query_as::<_, (Option<String>,)>(
        r#"SELECT poster_hash FROM "Games" WHERE id = $1 FOR UPDATE"#,
    )
    .bind(game.id)
    .fetch_optional(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Game not found"))?
    .0;
    let (blob, _) = crate::services::blob_refs::store_and_acquire_in_transaction(
        st.storage.as_ref(),
        control.transaction_mut(),
        "poster",
        &bytes,
    )
    .await?;
    sqlx::query(r#"UPDATE "Games" SET poster_hash = $2 WHERE id = $1"#)
        .bind(game.id)
        .bind(&blob.hash)
        .execute(&mut **control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if let Some(old_hash) = old_hash {
        if let Err(error) =
            crate::services::blob_refs::release_and_purge(st.pg(), st.storage.as_ref(), &old_hash)
                .await
        {
            tracing::warn!(%error, hash = %old_hash, "old game poster purge failed");
        }
    }

    Ok(RequestResponse::ok(format!("/assets/{}/poster", blob.hash)))
}

/// `POST /api/edit/games/{id}/scoreboard/flush` — evict any cached scoreboard
/// renderings for the game; void. Cache eviction is best-effort.
pub async fn flush_scoreboard(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<MessageResponse> {
    manager_or_admin(&st, &user, id).await?;
    load_game(&st, id).await?;
    flush_game_scoreboards(&st, id).await;
    Ok(MessageResponse::ok(""))
}

// ============================================================================
//  Co-managers (RSCTF EventManager / Game.Managers)
// ============================================================================

/// `GET /api/edit/games/{id}/admins` — list the game's co-organizers, joining
/// `game_manager -> user`. Contract: `ProfileUserInfoModel[]`. Managing the
/// manager roster stays platform-admin-only (`[RequireAdmin]`).
pub async fn get_game_admins(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<Vec<ManagerInfoModel>>> {
    load_game(&st, id).await?;
    let managers = game_manager::Entity::find()
        .filter(game_manager::Column::GameId.eq(id))
        .all(&st.db)
        .await?;
    let user_ids: Vec<Uuid> = managers.iter().map(|m| m.user_id).collect();
    let users = user::Entity::find()
        .filter(user::Column::Id.is_in(user_ids))
        .all(&st.db)
        .await?;
    let data = users.iter().map(ManagerInfoModel::from_user).collect();
    Ok(RequestResponse::ok(data))
}

/// `POST /api/edit/games/{id}/admins/{userId}` — grant a user co-organizer
/// rights (insert a `game_manager` row). Idempotent. Void.
pub async fn add_game_admin(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path((id, user_id)): Path<(i32, Uuid)>,
) -> AppResult<MessageResponse> {
    load_game(&st, id).await?;
    if user::Entity::find_by_id(user_id)
        .one(&st.db)
        .await?
        .is_none()
    {
        return Err(AppError::not_found("User not found"));
    }
    let already = game_manager::Entity::find()
        .filter(game_manager::Column::GameId.eq(id))
        .filter(game_manager::Column::UserId.eq(user_id))
        .count(&st.db)
        .await?
        > 0;
    if !already {
        let am = game_manager::ActiveModel {
            game_id: Set(id),
            user_id: Set(user_id),
            ..Default::default()
        };
        am.insert(&st.db).await?;
    }
    Ok(MessageResponse::ok(""))
}

/// `DELETE /api/edit/games/{id}/admins/{userId}` — revoke co-organizer rights
/// (delete the `game_manager` row). Void.
pub async fn remove_game_admin(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path((id, user_id)): Path<(i32, Uuid)>,
) -> AppResult<MessageResponse> {
    load_game(&st, id).await?;
    let res = game_manager::Entity::delete_many()
        .filter(game_manager::Column::GameId.eq(id))
        .filter(game_manager::Column::UserId.eq(user_id))
        .exec(&st.db)
        .await?;
    if res.rows_affected == 0 {
        return Err(AppError::not_found("User not found"));
    }
    Ok(MessageResponse::ok(""))
}

// ============================================================================
//  Reviews / pending queue
// ============================================================================

/// Optional filters for the review list, mirroring RSCTF
/// `ChallengeReviewRepository.GetReviewsAsync` (`?search=` + `?rating=`).
/// `rating` is the numeric `ReviewRating` wire value (compared against the
/// review's cast discriminant, sidestepping the enum name/wire mismatch).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewFilterParams {
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default)]
    pub rating: Option<i16>,
}

/// `GET /api/edit/games/{id}/reviews`
pub async fn get_reviews(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
    axum::extract::Query(page): axum::extract::Query<PageParams>,
    axum::extract::Query(filter): axum::extract::Query<ReviewFilterParams>,
) -> AppResult<ArrayResponse<ChallengeReviewDetailModel>> {
    manager_or_admin(&st, &user, id).await?;
    let game = load_game(&st, id).await?;

    // Load the full game review set, then apply the search/rating filters in
    // memory (search spans the joined challenge title + user name), so the
    // returned total reflects the FILTERED count before paging.
    let rows = challenge_review::Entity::find()
        .filter(challenge_review::Column::GameId.eq(id))
        .order_by_desc(challenge_review::Column::SubmitTimeUtc)
        .all(&st.db)
        .await?;

    // Resolve the display-name joins in a single batched query per relation.
    let challenge_ids: Vec<i32> = rows.iter().map(|r| r.challenge_id).collect();
    let challenge_titles = load_challenge_titles(&st, challenge_ids).await?;
    let user_ids: Vec<Uuid> = rows.iter().map(|r| r.user_id).collect();
    let user_names = load_user_names(&st, user_ids).await?;

    // RSCTF filters via Postgres `LIKE` (case-sensitive `Contains`).
    let search = filter
        .search
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let filtered: Vec<ChallengeReviewDetailModel> = rows
        .iter()
        .filter(|r| filter.rating.is_none_or(|want| r.rating as i16 == want))
        .filter_map(|r| {
            let challenge_name = challenge_titles.get(&r.challenge_id).cloned();
            let user_name = user_names.get(&r.user_id).cloned();
            if let Some(needle) = search {
                let hit = challenge_name
                    .as_deref()
                    .is_some_and(|t| t.contains(needle))
                    || user_name.as_deref().is_some_and(|u| u.contains(needle));
                if !hit {
                    return None;
                }
            }
            Some(ChallengeReviewDetailModel::from_review(
                r,
                challenge_name,
                Some(game.title.clone()),
                user_name,
            ))
        })
        .collect();

    let total = filtered.len() as i64;
    let data = filtered
        .into_iter()
        .skip(page.skip as usize)
        .take(page.limit() as usize)
        .collect();
    Ok(ArrayResponse::new(data, total))
}

/// `GET /api/edit/games/{id}/reviews/analytics` — aggregate persisted reviews
/// into like/dislike tallies + the top-5 liked/disliked challenges. Mirrors
/// `ChallengeReviewRepository.GetAnalyticsAsync` (`ReviewAnalyticsModel`).
///
/// NOTE: the port's `ReviewRating` enum reuses the discriminants the wire
/// contract assigns to Dislike (`1`) / Like (`2`), so we tally on the numeric
/// value rather than the (differently-named) Rust variants.
pub async fn get_review_analytics(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<JsonValue>> {
    manager_or_admin(&st, &user, id).await?;
    load_game(&st, id).await?;
    let reviews = challenge_review::Entity::find()
        .filter(challenge_review::Column::GameId.eq(id))
        .all(&st.db)
        .await?;

    // Challenge id -> title for the top lists.
    let titles: std::collections::HashMap<i32, String> = game_challenge::Entity::find()
        .filter(game_challenge::Column::GameId.eq(id))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|c| (c.id, c.title))
        .collect();

    const DISLIKE: i16 = 1;
    const LIKE: i16 = 2;
    let total = reviews.len() as i64;
    let likes = reviews.iter().filter(|r| r.rating as i16 == LIKE).count() as i64;
    let dislikes = reviews
        .iter()
        .filter(|r| r.rating as i16 == DISLIKE)
        .count() as i64;

    let top = |want: i16| -> Vec<JsonValue> {
        let mut counts: std::collections::HashMap<i32, i64> = std::collections::HashMap::new();
        for r in reviews.iter().filter(|r| r.rating as i16 == want) {
            *counts.entry(r.challenge_id).or_insert(0) += 1;
        }
        let mut rows: Vec<(i32, i64)> = counts.into_iter().collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        rows.into_iter()
            .take(5)
            .map(|(cid, count)| {
                json!({
                    "id": cid,
                    "title": titles.get(&cid).cloned().unwrap_or_default(),
                    "count": count
                })
            })
            .collect()
    };

    Ok(RequestResponse::ok(json!({
        "total": total,
        "likes": likes,
        "dislikes": dislikes,
        "topLiked": top(LIKE),
        "topDisliked": top(DISLIKE)
    })))
}
