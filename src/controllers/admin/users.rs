//! User listing / search / CRUD / batch creation.

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

/// Cache-key prefix for a freshly-imported user's plaintext password, so
/// `credentials/send` can email it later WITHOUT resetting (and thus
/// invalidating) the password already shown in the import table.
pub(super) const CRED_CACHE_PREFIX: &str = "credimport:";

/// TTL for a cached import credential (7 days) — long enough to email after
/// review, short enough that stale plaintext doesn't linger.
const CRED_CACHE_TTL_SECS: u64 = 7 * 24 * 3600;

pub(super) fn may_bulk_recredential(role: Role) -> bool {
    role != Role::Admin
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

/// Pick a username unique (case-insensitively) across the DB AND this batch,
/// suffixing `.2`, `.3`, … on collision. Two distinct people who normalize to
/// the same base both get created (the client dedupes by EMAIL, not username),
/// and a collision never silently drops a legitimate row.
async fn unique_username(
    st: &SharedState,
    base: &str,
    used: &mut std::collections::HashSet<String>,
) -> AppResult<String> {
    let mut n = 1;
    loop {
        let candidate = if n == 1 {
            base.to_string()
        } else {
            format!("{base}.{n}")
        };
        let norm = candidate.to_uppercase();
        let free = !used.contains(&norm)
            && user::Entity::find()
                .filter(user::Column::NormalizedUserName.eq(norm.clone()))
                .one(&st.db)
                .await?
                .is_none();
        if free {
            used.insert(norm);
            return Ok(candidate);
        }
        n += 1;
    }
}

/// Join `user_id` to the team named `team_name`, reusing an existing team of that
/// name (first from this batch's `team_by_name` cache, then the DB) or creating a
/// fresh one with `user_id` as captain. Idempotent on the membership row. Used by
/// the upsert (re-add-to-team) paths of `import_users` / `add_users`.
async fn join_or_create_team(
    st: &SharedState,
    team_by_name: &mut BTreeMap<String, i32>,
    team_name: &str,
    user_id: Uuid,
) -> AppResult<()> {
    let team_id = if let Some(&tid) = team_by_name.get(team_name) {
        tid
    } else if let Some(existing) = team::Entity::find()
        .filter(team::Column::Name.eq(team_name))
        .one(&st.db)
        .await?
    {
        team_by_name.insert(team_name.to_string(), existing.id);
        existing.id
    } else {
        let team = team::ActiveModel {
            name: Set(team_name.to_string()),
            bio: Set(None),
            avatar_hash: Set(None),
            locked: Set(false),
            invite_token: Set(random_hex(16)),
            captain_id: Set(user_id),
            ..Default::default()
        }
        .insert(&st.db)
        .await?;
        team_by_name.insert(team_name.to_string(), team.id);
        team.id
    };
    let already = team_member::Entity::find()
        .filter(team_member::Column::TeamId.eq(team_id))
        .filter(team_member::Column::UserId.eq(user_id))
        .one(&st.db)
        .await?
        .is_some();
    if !already {
        team_member::ActiveModel {
            team_id: Set(team_id),
            user_id: Set(user_id),
            ..Default::default()
        }
        .insert(&st.db)
        .await?;
    }
    Ok(())
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
/// plaintext password is cached (keyed by email) so a later `credentials/send`
/// can email it without a destructive reset. Returns the RAW `CsvImportResult`
/// (no envelope — the client reads `result.total` directly).
pub async fn import_users(
    State(st): State<SharedState>,
    AdminUser(caller): AdminUser,
    Json(req): Json<ImportRequest>,
) -> AppResult<Json<ImportResult>> {
    let now = Utc::now();
    let single_team = req
        .single_team_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let mut used_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut seen_emails: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut team_by_name: BTreeMap<String, i32> = BTreeMap::new();

    let mut out: Vec<ImportUserResult> = Vec::with_capacity(req.rows.len());
    let (mut created, mut updated, mut skipped) = (0usize, 0usize, 0usize);

    for row in &req.rows {
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
        // Duplicate email already in the DB: UPDATE the existing user instead of
        // skipping — re-credential (fresh password), overwrite the provided profile
        // fields, and re-add to their team (RSCTF `ImportUsersFromCsv` upsert).
        if let Some(existing) = user::Entity::find()
            .filter(user::Column::NormalizedEmail.eq(norm_email.clone()))
            .one(&st.db)
            .await?
        {
            seen_emails.insert(norm_email.clone());
            if !may_bulk_recredential(existing.role) {
                skipped += 1;
                out.push(skipped_row(
                    &email,
                    &real_name,
                    &preview_name,
                    team_name,
                    "administrator accounts cannot be updated by import",
                ));
                continue;
            }
            let existing_id = existing.id;
            let existing_user_name = existing.user_name.clone().unwrap_or_default();
            let password = generate_password();
            let password_hash = hash_password(&password)?;

            let txn = crate::controllers::account::locked_registration_transaction(&st).await?;
            let existing = user::Entity::find_by_id(existing_id)
                .one(&txn)
                .await?
                .ok_or_else(|| AppError::not_found("User not found"))?;
            if !may_bulk_recredential(existing.role) {
                txn.rollback().await?;
                skipped += 1;
                out.push(skipped_row(
                    &email,
                    &real_name,
                    &preview_name,
                    team_name,
                    "administrator accounts cannot be updated by import",
                ));
                continue;
            }
            let mut am: user::ActiveModel = existing.into();
            am.password_hash = Set(Some(password_hash));
            // Rotate the security stamp so any live sessions are invalidated (as
            // RSCTF `ResetPasswordAsync` does).
            am.security_stamp = Set(Some(Uuid::new_v4().to_string()));
            // RSCTF `UpdateUserInfo(UserCreateModel)`: overwrite each provided field
            // (leave the existing value when the row omits it).
            if !real_name.is_empty() {
                am.real_name = Set(real_name.clone());
            }
            if let Some(std) = row
                .std_number
                .clone()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
            {
                am.std_number = Set(std);
            }
            if let Some(phone) = row
                .phone
                .clone()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
            {
                am.phone_number = Set(Some(phone));
            }
            am.update(&txn).await?;
            txn.commit().await?;

            if let Some(tname) = team_name.as_deref() {
                join_or_create_team(&st, &mut team_by_name, tname, existing_id).await?;
            }

            // Cache the fresh plaintext for a later `credentials/send` (no reset).
            st.cache
                .set(
                    &format!("{CRED_CACHE_PREFIX}{norm_email}"),
                    password.as_bytes(),
                    Some(std::time::Duration::from_secs(CRED_CACHE_TTL_SECS)),
                )
                .await;

            updated += 1;
            out.push(ImportUserResult {
                email,
                real_name,
                user_name: existing_user_name,
                password,
                team_name,
                status: "updated".into(),
                error: None,
            });
            continue;
        }
        seen_emails.insert(norm_email.clone());

        let user_name = unique_username(&st, &preview_name, &mut used_names).await?;
        let password = generate_password();
        let password_hash = hash_password(&password)?;
        let id = Uuid::now_v7();

        let am = user::ActiveModel {
            id: Set(id),
            user_name: Set(Some(user_name.clone())),
            normalized_user_name: Set(Some(user_name.to_uppercase())),
            email: Set(Some(email.clone())),
            normalized_email: Set(Some(norm_email.clone())),
            email_confirmed: Set(req.email_confirmed),
            password_hash: Set(Some(password_hash)),
            security_stamp: Set(Some(Uuid::new_v4().to_string())),
            concurrency_stamp: Set(Some(Uuid::new_v4().to_string())),
            phone_number: Set(row.phone.clone()),
            phone_number_confirmed: Set(false),
            two_factor_enabled: Set(false),
            lockout_end: Set(None),
            lockout_enabled: Set(false),
            access_failed_count: Set(0),
            role: Set(Role::User),
            ip: Set("0.0.0.0".to_string()),
            browser_fingerprint: Set(None),
            last_signed_in_utc: Set(now),
            last_visited_utc: Set(now),
            register_time_utc: Set(now),
            bio: Set(String::new()),
            real_name: Set(real_name.clone()),
            std_number: Set(row.std_number.clone().unwrap_or_default()),
            exercise_visible: Set(true),
            avatar_hash: Set(None),
        };
        am.insert(&st.db).await?;

        // Wire up team membership (reuse a team named `team_name`: this batch,
        // then the DB; else create it with this user as captain).
        if let Some(tname) = team_name.as_deref() {
            join_or_create_team(&st, &mut team_by_name, tname, id).await?;
        }

        // Cache the plaintext for a later `credentials/send` (no reset needed).
        st.cache
            .set(
                &format!("{CRED_CACHE_PREFIX}{norm_email}"),
                password.as_bytes(),
                Some(std::time::Duration::from_secs(CRED_CACHE_TTL_SECS)),
            )
            .await;

        created += 1;
        out.push(ImportUserResult {
            email,
            real_name,
            user_name,
            password,
            team_name,
            status: "created".into(),
            error: None,
        });
    }

    crate::services::audit::info(
        &st.db,
        "AdminController",
        Some(caller.name.clone()),
        None,
        format!("CSV import: {created} created, {updated} updated, {skipped} skipped"),
    )
    .await;

    Ok(Json(ImportResult {
        total: req.rows.len(),
        created,
        updated,
        skipped,
        users: out,
    }))
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
    // Each entry carries the row + resolved identity, plus `Some(id)` when it maps
    // to an existing DB user to UPDATE (upsert), or `None` to CREATE.
    let mut prepared: Vec<(
        UserCreateModel,
        String,
        String,
        String,
        String,
        Option<Uuid>,
    )> = Vec::with_capacity(models.len());

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
        // A row matching an existing DB user (by username first, then email —
        // mirroring RSCTF's DuplicateUserName-then-DuplicateEmail resolution) is an
        // UPDATE; otherwise a CREATE.
        let existing = if let Some(u) = user::Entity::find()
            .filter(user::Column::NormalizedUserName.eq(norm_name.clone()))
            .one(&st.db)
            .await?
        {
            Some(u)
        } else {
            user::Entity::find()
                .filter(user::Column::NormalizedEmail.eq(norm_email.clone()))
                .one(&st.db)
                .await?
        };
        if existing
            .as_ref()
            .is_some_and(|target| !may_bulk_recredential(target.role))
        {
            return Err(AppError::bad_request(
                "Administrator accounts cannot be updated by batch import",
            ));
        }
        let existing_id = existing.map(|target| target.id);

        seen_names.insert(norm_name.clone());
        seen_emails.insert(norm_email.clone());
        prepared.push((m, user_name, email, norm_name, norm_email, existing_id));
    }

    // ── Insert users, then wire up team membership ────────────────────────
    let now = Utc::now();
    // Track teams created/joined during this import so two rows naming the same
    // (new) team join one team instead of creating duplicates.
    let mut team_by_name: BTreeMap<String, i32> = BTreeMap::new();

    // RSCTF logs `users.Count` — the number of rows successfully upserted (created
    // + updated). Capture it before the consuming loop below.
    let created_count = prepared.len();

    for (m, user_name, email, norm_name, norm_email, existing_id) in prepared {
        // CREATE a new user, or UPDATE the matched existing one (upsert). Both arms
        // yield the user id the team block below wires membership for.
        let id = match existing_id {
            None => {
                let id = Uuid::now_v7();
                let password_hash = hash_password(&m.password)?;
                let am = user::ActiveModel {
                    id: Set(id),
                    user_name: Set(Some(user_name.clone())),
                    normalized_user_name: Set(Some(norm_name)),
                    email: Set(Some(email)),
                    normalized_email: Set(Some(norm_email)),
                    email_confirmed: Set(true),
                    password_hash: Set(Some(password_hash)),
                    security_stamp: Set(Some(Uuid::new_v4().to_string())),
                    concurrency_stamp: Set(Some(Uuid::new_v4().to_string())),
                    phone_number: Set(m.phone.clone()),
                    phone_number_confirmed: Set(false),
                    two_factor_enabled: Set(false),
                    lockout_end: Set(None),
                    lockout_enabled: Set(false),
                    access_failed_count: Set(0),
                    role: Set(Role::User),
                    ip: Set("0.0.0.0".to_string()),
                    browser_fingerprint: Set(None),
                    last_signed_in_utc: Set(now),
                    last_visited_utc: Set(now),
                    register_time_utc: Set(now),
                    bio: Set(String::new()),
                    real_name: Set(m.real_name.clone().unwrap_or_default()),
                    std_number: Set(m.std_number.clone().unwrap_or_default()),
                    exercise_visible: Set(true),
                    avatar_hash: Set(None),
                };
                am.insert(&st.db).await?;
                id
            }
            Some(eid) => {
                // RSCTF `UpdateUserInfo(UserCreateModel)` + `ResetPassword`: set the
                // username/email to the row's values (this row uniquely owns them —
                // the batch + DB dedup guarantees it), re-credential, rotate the
                // security stamp, and overwrite each provided profile field.
                let password_hash = hash_password(&m.password)?;
                let txn = crate::controllers::account::locked_registration_transaction(&st).await?;
                let existing = user::Entity::find_by_id(eid)
                    .one(&txn)
                    .await?
                    .ok_or_else(|| AppError::not_found("User not found"))?;
                if !may_bulk_recredential(existing.role) {
                    return Err(AppError::bad_request(
                        "Administrator accounts cannot be updated by batch import",
                    ));
                }
                let mut am: user::ActiveModel = existing.into();
                am.user_name = Set(Some(user_name.clone()));
                am.normalized_user_name = Set(Some(norm_name));
                am.email = Set(Some(email));
                am.normalized_email = Set(Some(norm_email));
                am.password_hash = Set(Some(password_hash));
                am.security_stamp = Set(Some(Uuid::new_v4().to_string()));
                if let Some(rn) = m
                    .real_name
                    .clone()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                {
                    am.real_name = Set(rn);
                }
                if let Some(std) = m
                    .std_number
                    .clone()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                {
                    am.std_number = Set(std);
                }
                if let Some(phone) = m
                    .phone
                    .clone()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                {
                    am.phone_number = Set(Some(phone));
                }
                am.update(&txn).await?;
                txn.commit().await?;
                eid
            }
        };

        let Some(team_name) = m
            .team_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        join_or_create_team(&st, &mut team_by_name, team_name, id).await?;
    }

    // RSCTF `AdminController` audit event (`Admin_UserBatchAdded`).
    crate::services::audit::info(
        &st.db,
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
    fn bulk_recredential_policy_excludes_only_administrators() {
        assert!(!may_bulk_recredential(Role::Admin));
        assert!(may_bulk_recredential(Role::Monitor));
        assert!(may_bulk_recredential(Role::User));
        assert!(may_bulk_recredential(Role::Banned));
    }
}
