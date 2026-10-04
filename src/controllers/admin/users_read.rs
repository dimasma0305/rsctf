use super::*;

/// `POST /api/admin/users/search` — case-insensitive substring search across the
/// same identity fields RSCTF `SearchUsers` covers: username, std number, email,
/// phone, the stringified id, and real name. Mirrors RSCTF's `.ToLower().Contains`
/// by matching `LOWER(col) LIKE '%hint%'` (the id column cast to text first).
pub async fn search_users(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Query(model): Query<SearchModel>,
) -> AppResult<ArrayResponse<UserInfoModel>> {
    let hint = model.hint;
    let hint = hint.trim().to_lowercase();
    let pat = format!("%{hint}%");
    let rows = user::Entity::find()
        .filter(
            Condition::any()
                .add(Expr::expr(Func::lower(user::Column::UserName.into_expr())).like(pat.as_str()))
                .add(
                    Expr::expr(Func::lower(user::Column::StdNumber.into_expr())).like(pat.as_str()),
                )
                .add(Expr::expr(Func::lower(user::Column::Email.into_expr())).like(pat.as_str()))
                .add(
                    Expr::expr(Func::lower(user::Column::PhoneNumber.into_expr()))
                        .like(pat.as_str()),
                )
                .add(
                    Expr::expr(Func::lower(
                        user::Column::Id.into_expr().cast_as(Alias::new("text")),
                    ))
                    .like(pat.as_str()),
                )
                .add(
                    Expr::expr(Func::lower(user::Column::RealName.into_expr())).like(pat.as_str()),
                ),
        )
        .order_by_asc(user::Column::Id)
        .limit(30)
        .all(&st.db)
        .await?;

    let data: Vec<UserInfoModel> = rows.into_iter().map(UserInfoModel::from).collect();
    let total = data.len() as i64;
    Ok(ArrayResponse::new(data, total))
}

/// `GET /api/admin/users/{userid}` — single-user detail.
pub async fn user_info(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(userid): Path<Uuid>,
) -> AppResult<RequestResponse<ProfileUserInfoModel>> {
    let u = load_user(&st, userid).await?;
    let mut model: ProfileUserInfoModel = u.into();
    // RSCTF's `ProfileUserInfoModel` leaves `HasManagedGames` as a placeholder
    // the controller must fill (see the model's own comment). Populate it the
    // same way `AccountController.Profile` does: true when the user co-organizes
    // at least one game (RSCTF `Game.Managers` / `EventManager`).
    model.has_managed_games = game_manager::Entity::find()
        .filter(game_manager::Column::UserId.eq(userid))
        .count(&st.db)
        .await?
        > 0;
    Ok(RequestResponse::ok(model))
}
