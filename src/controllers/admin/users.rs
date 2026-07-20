//! User listing / search / CRUD / batch creation.

use super::users_bulk_identity::{
    provision_explicit_user, provision_import_user, ExplicitUserWrite, ImportCredentialWrite,
    ImportProvision, ImportUserWrite,
};
use super::*;
use sea_orm::sea_query::{Alias, Expr, Func};

/// RSCTF `Models.Request.Admin.UserInfoModel`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInfoModel {
    pub id: Uuid,
    pub user_name: Option<String>,
    pub real_name: String,
    pub std_number: String,
    pub phone: Option<String>,
    pub bio: String,
    #[serde(with = "crate::utils::datetime::millis")]
    pub register_time_utc: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub last_visited_utc: DateTime<Utc>,
    pub ip: String,
    pub email: Option<String>,
    pub avatar: Option<String>,
    pub role: Role,
    pub email_confirmed: bool,
}

impl From<user::Model> for UserInfoModel {
    fn from(u: user::Model) -> Self {
        Self {
            id: u.id,
            avatar: u.avatar_url(),
            user_name: u.user_name,
            real_name: u.real_name,
            std_number: u.std_number,
            phone: u.phone_number,
            bio: u.bio,
            register_time_utc: u.register_time_utc,
            last_visited_utc: u.last_visited_utc,
            ip: u.ip,
            email: u.email,
            role: u.role,
            email_confirmed: u.email_confirmed,
        }
    }
}

/// RSCTF `ProfileUserInfoModel` (admin single-user view).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileUserInfoModel {
    pub user_id: Uuid,
    pub role: Role,
    pub user_name: Option<String>,
    pub email: Option<String>,
    pub bio: String,
    pub phone: Option<String>,
    pub real_name: String,
    pub std_number: String,
    pub avatar: Option<String>,
    pub has_managed_games: bool,
}

impl From<user::Model> for ProfileUserInfoModel {
    fn from(u: user::Model) -> Self {
        Self {
            user_id: u.id,
            avatar: u.avatar_url(),
            user_name: u.user_name,
            email: u.email,
            role: u.role,
            bio: u.bio,
            phone: u.phone_number,
            real_name: u.real_name,
            std_number: u.std_number,
            has_managed_games: false,
        }
    }
}

/// Admin user-mutation body (RSCTF `AdminUserInfoModel`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminUserInfoModel {
    #[serde(default)]
    pub user_name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub bio: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
    #[serde(default)]
    pub real_name: Option<String>,
    #[serde(default)]
    pub std_number: Option<String>,
    #[serde(default)]
    pub email_confirmed: Option<bool>,
    #[serde(default)]
    pub role: Option<Role>,
}

/// RSCTF `UserCreateModel` — one row of the batch user-creation body.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserCreateModel {
    #[serde(default)]
    pub user_name: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub real_name: Option<String>,
    #[serde(default)]
    pub std_number: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
    #[serde(default)]
    pub team_name: Option<String>,
}

// ─── CSV bulk import ─────────────────────────────────────────────────────────
//
// The frontend parses the CSV and POSTs structured JSON here (not a multipart
// CSV). The server generates each user's username and password. The request and
// response shapes are consumed by `web/src/components/admin/UserImportModal.tsx`.

/// One row of the import body (`{ email, realName, userNameOverride?, ... }`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportRow {
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub real_name: String,
    #[serde(default)]
    pub user_name_override: Option<String>,
    #[serde(default)]
    pub team_name: Option<String>,
    #[serde(default)]
    pub std_number: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
}

/// The import request (`{ rows, teamMode, singleTeamName?, emailConfirmed }`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportRequest {
    #[serde(default)]
    pub rows: Vec<ImportRow>,
    /// `"fromrow"` (per-row team), `"single"` (one team for all), or `"none"`.
    #[serde(default)]
    pub team_mode: String,
    #[serde(default)]
    pub single_team_name: Option<String>,
    #[serde(default)]
    pub email_confirmed: bool,
}

/// Per-row outcome (`CsvImportUserResult`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportUserResult {
    pub email: String,
    pub real_name: String,
    pub user_name: String,
    pub password: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_name: Option<String>,
    /// `"created"` | `"updated"` | `"skipped"`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The import response (`CsvImportResult`) — returned as the RAW model.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub total: usize,
    pub created: usize,
    pub updated: usize,
    pub skipped: usize,
    pub users: Vec<ImportUserResult>,
}

/// The default username the client previews from a real name
/// (`previewUsername`): lowercase, whitespace runs → `.`, drop anything outside
/// `[a-z0-9.]`, cap at 15, fall back to `user`. An override wins (trimmed, ≤15).
fn base_username(real_name: &str, override_name: Option<&str>) -> String {
    if let Some(o) = override_name.map(str::trim).filter(|s| !s.is_empty()) {
        return o.chars().take(15).collect();
    }
    let mut collapsed = String::new();
    let mut prev_ws = false;
    for c in real_name.to_lowercase().chars() {
        if c.is_whitespace() {
            if !prev_ws {
                collapsed.push('.');
            }
            prev_ws = true;
        } else {
            collapsed.push(c);
            prev_ws = false;
        }
    }
    let cleaned: String = collapsed
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '.')
        .take(15)
        .collect();
    if cleaned.is_empty() {
        "user".to_string()
    } else {
        cleaned
    }
}

/// Build a `"skipped"` result row (no password; carries the reason).
fn skipped_row(
    email: &str,
    real_name: &str,
    user_name: &str,
    team_name: Option<String>,
    err: &str,
) -> ImportUserResult {
    ImportUserResult {
        email: email.to_string(),
        real_name: real_name.to_string(),
        user_name: user_name.to_string(),
        password: String::new(),
        team_name,
        status: "skipped".into(),
        error: Some(err.to_string()),
    }
}

fn sanitized_import_row_error(error: &AppError) -> String {
    match error {
        AppError::BadRequest(reason)
        | AppError::Conflict(reason)
        | AppError::Validation(reason) => reason.clone(),
        AppError::ServiceUnavailable(_) => "service temporarily unavailable".to_string(),
        _ => "row could not be imported".to_string(),
    }
}

/// Convert a failing per-row step into a sanitized result while retaining the
/// full cause exclusively in server logs. Both password hashing and database
/// provisioning use this boundary, so neither can discard results committed by
/// an earlier row in the same request.
pub(super) fn import_row_step<T>(
    result: AppResult<T>,
    row_number: usize,
    stage: &'static str,
) -> Result<T, String> {
    result.map_err(|error| {
        match &error {
            AppError::Database(_) | AppError::Internal(_) => {
                tracing::error!(row_number, stage, error = %error, "CSV import row failed");
            }
            _ => {
                tracing::warn!(row_number, stage, error = %error, "CSV import row skipped");
            }
        }
        sanitized_import_row_error(&error)
    })
}

/// `POST /api/admin/users/import` — CSV bulk import (client-parsed → JSON rows).
///
/// For each row: generate a username (override or derived from real name, made
/// unique) and a random password, create the user, and — per `teamMode` — join
/// or create a team. Rows are deduped by EMAIL: a duplicate earlier in THIS batch
/// is SKIPPED, but a duplicate already in the DB UPDATES the existing user —
/// re-crediting them with a fresh password, overwriting the provided profile
/// fields, and re-adding them to their team — counted as `updated` (matching
/// RSCTF `ImportUsersFromCsv`'s upsert: `CreateAsync` DuplicateEmail →
/// `FindByEmail` + `UpdateUserInfo` + `ResetPassword`). The existing username is
/// kept (rsctf's generator is DB-unique, so it would never regenerate their own
/// name — renaming a matched user is undesirable). Each created/updated user's
/// plaintext password is cached with its immutable user id so a later
/// `credentials/send` can email it without a destructive reset or an email-key
/// reassignment race. Returns the RAW `CsvImportResult` (no envelope — the
/// client reads `result.total` directly).
pub async fn import_users(
    State(st): State<SharedState>,
    AdminUser(caller): AdminUser,
    Json(req): Json<ImportRequest>,
) -> AppResult<Response> {
    let now = Utc::now();
    let single_team = req
        .single_team_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let mut seen_emails: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut team_by_name: BTreeMap<String, i32> = BTreeMap::new();

    let mut out: Vec<ImportUserResult> = Vec::with_capacity(req.rows.len());
    let (mut created, mut updated, mut skipped) = (0usize, 0usize, 0usize);

    for (row_index, row) in req.rows.iter().enumerate() {
        let row_number = row_index.saturating_add(1);
        let email = row.email.trim().to_lowercase();
        let real_name = row.real_name.trim().to_string();

        // Resolve the row's team name up front (used in both success + skip rows).
        let team_name = match req.team_mode.as_str() {
            "single" => single_team.clone(),
            "none" => None,
            // "fromrow" (and any unknown value) → the row's own team.
            _ => row
                .team_name
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        };

        // The client-previewed username (used verbatim on skip rows; the base
        // for the unique username on success).
        let preview_name = base_username(&real_name, row.user_name_override.as_deref());

        if !email.contains('@') {
            skipped += 1;
            out.push(skipped_row(
                &email,
                &real_name,
                &preview_name,
                team_name,
                "invalid email address",
            ));
            continue;
        }
        let norm_email = email.to_uppercase();
        if seen_emails.contains(&norm_email) {
            skipped += 1;
            out.push(skipped_row(
                &email,
                &real_name,
                &preview_name,
                team_name,
                "duplicate email in this import",
            ));
            continue;
        }
        seen_emails.insert(norm_email.clone());
        let password = generate_password();
        let password_hash = match import_row_step(
            hash_password_async(password.clone()).await,
            row_number,
            "password_hash",
        ) {
            Ok(password_hash) => password_hash,
            Err(reason) => {
                skipped += 1;
                out.push(skipped_row(
                    &email,
                    &real_name,
                    &preview_name,
                    team_name,
                    &reason,
                ));
                continue;
            }
        };
        let update_std_number = row
            .std_number
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let update_phone = row
            .phone
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let cached_team_id = team_name
            .as_deref()
            .and_then(|name| team_by_name.get(name).copied());
        let provision = match import_row_step(
            provision_import_user(
                st.pg(),
                ImportUserWrite {
                    email: &email,
                    normalized_email: &norm_email,
                    base_user_name: &preview_name,
                    password_hash: &password_hash,
                    email_confirmed: req.email_confirmed,
                    create_real_name: &real_name,
                    create_std_number: row.std_number.as_deref().unwrap_or_default(),
                    create_phone: row.phone.as_deref(),
                    update_real_name: (!real_name.is_empty()).then_some(real_name.as_str()),
                    update_std_number,
                    update_phone,
                    now,
                },
                ImportCredentialWrite {
                    cache: st.cache.as_ref(),
                    password: &password,
                },
                team_name.as_deref(),
                cached_team_id,
            )
            .await,
            row_number,
            "provision",
        ) {
            Ok(provision) => provision,
            Err(reason) => {
                skipped += 1;
                out.push(skipped_row(
                    &email,
                    &real_name,
                    &preview_name,
                    team_name,
                    &reason,
                ));
                continue;
            }
        };
        let provision = match provision {
            ImportProvision::Provisioned(provision) => provision,
            ImportProvision::Skipped(reason) => {
                skipped += 1;
                out.push(skipped_row(
                    &email,
                    &real_name,
                    &preview_name,
                    team_name,
                    reason,
                ));
                continue;
            }
        };
        if let (Some(name), Some(team_id)) = (team_name.as_ref(), provision.team_id) {
            team_by_name.insert(name.clone(), team_id);
        }
        if provision.created {
            created += 1;
        } else {
            updated += 1;
        }
        out.push(ImportUserResult {
            email,
            real_name,
            user_name: provision.user_name,
            password,
            team_name,
            status: if provision.created {
                "created".into()
            } else {
                "updated".into()
            },
            error: None,
        });
    }

    crate::services::audit::info(
        &st,
        "AdminController",
        Some(caller.name.clone()),
        None,
        format!("CSV import: {created} created, {updated} updated, {skipped} skipped"),
    )
    .await;

    Ok(super::users_credentials::private_no_store(Json(
        ImportResult {
            total: req.rows.len(),
            created,
            updated,
            skipped,
            users: out,
        },
    )))
}

/// `GET /api/admin/users` — paginated listing with optional substring search.
pub async fn users(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Query(q): Query<ListQuery>,
) -> AppResult<ArrayResponse<UserInfoModel>> {
    let count = q.count.clamp(0, 500);
    let mut base = user::Entity::find();
    if let Some(search) = q.search.as_deref().filter(|s| !s.is_empty()) {
        base = base.filter(
            Condition::any()
                .add(user::Column::UserName.contains(search))
                .add(user::Column::Email.contains(search)),
        );
    }

    let total = base.clone().count(&st.db).await? as i64;
    let rows = base
        .order_by_asc(user::Column::Id)
        .offset(q.skip)
        .limit(count)
        .all(&st.db)
        .await?;

    let data = rows.into_iter().map(UserInfoModel::from).collect();
    Ok(ArrayResponse::new(data, total))
}

/// `POST /api/admin/users` — batch user creation. Mirrors RSCTF `AddUsers`:
/// each row becomes a user (password Argon2-hashed with the same helper
/// `register` uses); a `teamName` joins an existing team of that name or creates
/// a fresh one with the user as captain.
///
/// A row duplicating an earlier row in THIS batch (same username/email) is
/// skipped. A row whose username or email already exists in the DB UPDATES that
/// existing user — re-credentialing them with the row's password, overwriting the
/// provided profile fields, and re-adding them to their team — instead of failing
/// the batch, mirroring RSCTF `AddUsers` (`CreateAsync` duplicate → `FindByName` /
/// `FindByEmail` + `UpdateUserInfo` + `ResetPassword`). Genuine validation failures
/// (short username, missing password, malformed email) still fail the batch.
pub async fn add_users(
    State(st): State<SharedState>,
    AdminUser(caller): AdminUser,
    Json(models): Json<Vec<UserCreateModel>>,
) -> AppResult<MessageResponse> {
    // ── Validate every row before inserting anything ──────────────────────
    let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut seen_emails: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut prepared: Vec<(UserCreateModel, String, String, String, String)> =
        Vec::with_capacity(models.len());

    for m in models {
        let user_name = m.user_name.trim().to_string();
        if user_name.len() < 3 {
            return Err(AppError::bad_request(
                "Username must be at least 3 characters",
            ));
        }
        if m.password.is_empty() {
            return Err(AppError::bad_request("Password is required"));
        }
        let email = m.email.trim().to_lowercase();
        if !email.contains('@') {
            return Err(AppError::bad_request("Invalid email address"));
        }
        let norm_name = user_name.to_uppercase();
        let norm_email = email.to_uppercase();

        // A row duplicating an earlier accepted row in THIS batch is skipped —
        // "updating the user you created two rows ago" is meaningless. Only accepted
        // rows consume the seen-sets, so a skipped row never masks a later one.
        if seen_names.contains(&norm_name) || seen_emails.contains(&norm_email) {
            continue;
        }
        seen_names.insert(norm_name.clone());
        seen_emails.insert(norm_email.clone());
        prepared.push((m, user_name, email, norm_name, norm_email));
    }

    // ── Insert users, then wire up team membership ────────────────────────
    let now = Utc::now();
    // Track teams created/joined during this import so two rows naming the same
    // (new) team join one team instead of creating duplicates.
    let mut team_by_name: BTreeMap<String, i32> = BTreeMap::new();

    // RSCTF logs `users.Count` — the number of rows successfully upserted (created
    // + updated). Capture it before the consuming loop below.
    let created_count = prepared.len();

    for (m, user_name, email, norm_name, norm_email) in prepared {
        let password_hash = hash_password_async(m.password.clone()).await?;
        let team_name = m
            .team_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let cached_team_id = team_name.and_then(|name| team_by_name.get(name).copied());
        let update_real_name = m
            .real_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let update_std_number = m
            .std_number
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let update_phone = m
            .phone
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let provision = provision_explicit_user(
            st.pg(),
            ExplicitUserWrite {
                user_name: &user_name,
                normalized_user_name: &norm_name,
                email: &email,
                normalized_email: &norm_email,
                password_hash: &password_hash,
                phone: m.phone.as_deref(),
                create_real_name: m.real_name.as_deref().unwrap_or_default(),
                create_std_number: m.std_number.as_deref().unwrap_or_default(),
                update_real_name,
                update_std_number,
                update_phone,
                now,
            },
            team_name,
            cached_team_id,
        )
        .await?;
        if let (Some(name), Some(team_id)) = (team_name, provision.team_id) {
            team_by_name.insert(name.to_string(), team_id);
        }
    }

    // RSCTF `AdminController` audit event (`Admin_UserBatchAdded`).
    crate::services::audit::info(
        &st,
        "AdminController",
        Some(caller.name.clone()),
        None,
        format!("Successfully added {created_count} users"),
    )
    .await;

    Ok(MessageResponse::ok(""))
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_import_step_errors_are_logged_but_sanitized() {
        let reason = import_row_step::<()>(
            Err(AppError::internal(
                "postgres://operator:secret@example.test/private",
            )),
            7,
            "password_hash",
        )
        .unwrap_err();
        assert_eq!(reason, "row could not be imported");
        assert!(!reason.contains("secret"));
        assert!(!reason.contains("postgres"));
    }

    #[test]
    fn expected_import_step_errors_keep_only_safe_reasons() {
        assert_eq!(
            import_row_step::<()>(Err(AppError::bad_request("Team is full")), 2, "provision",)
                .unwrap_err(),
            "Team is full"
        );
        assert_eq!(
            import_row_step::<()>(
                Err(AppError::unavailable("redis endpoint details")),
                3,
                "provision",
            )
            .unwrap_err(),
            "service temporarily unavailable"
        );
    }
}
