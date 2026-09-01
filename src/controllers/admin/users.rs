//! User listing / search / CRUD / batch creation.

use super::users_bulk_identity::{
    provision_explicit_user, provision_import_user_durable, DurableImportResult, ExplicitUserWrite,
    ImportCredentialWrite, ImportProvision, ImportUserWrite,
};
use super::users_credential_admission::admin_credential_scopes;
use super::users_import_results::{
    claim_import_row, decrypt_import_result, import_request_digest, persist_and_push_import_result,
    summarize_import, validate_import_request, ImportRowClaim,
};
use super::*;
use axum::extract::ConnectInfo;
use sea_orm::sea_query::{Alias, Expr, Func};
use std::net::SocketAddr;

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
#[derive(Clone, Debug, Deserialize, Serialize)]
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
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportRequest {
    pub operation_id: Uuid,
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
#[derive(Debug, Deserialize, Serialize)]
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

pub(super) fn terminal_import_row_reason(error: &AppError) -> Option<String> {
    match error {
        AppError::BadRequest(reason)
        | AppError::PayloadTooLarge(reason)
        | AppError::Validation(reason) => Some(reason.clone()),
        _ => None,
    }
}

/// Log a failing row step without converting infrastructure failures into a
/// terminal skipped result. The caller persists only deterministic validation
/// failures; retryable failures leave the durable row uncommitted for recovery.
pub(super) fn import_row_step<T>(
    result: AppResult<T>,
    row_number: usize,
    stage: &'static str,
) -> AppResult<T> {
    result.map_err(|error| {
        if terminal_import_row_reason(&error).is_some() {
            tracing::warn!(row_number, stage, error = %error, "CSV import row rejected");
        } else {
            tracing::error!(row_number, stage, error = %error, "CSV import row interrupted");
        }
        error
    })
}

async fn begin_import_job(
    st: &SharedState,
    requested_by: Uuid,
    request: &ImportRequest,
    request_digest: &[u8],
) -> AppResult<(bool, Vec<Option<ImportUserResult>>)> {
    let mut transaction = st
        .pg()
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let existing: Option<(Uuid, Vec<u8>, i32, i16, bool)> = sqlx::query_as(
        r#"SELECT requested_by, request_digest, row_count, status,
                  status = 0 OR COALESCE(result_expires_at_utc > clock_timestamp(), FALSE)
             FROM "AdminCredentialJobs"
            WHERE operation_id = $1 FOR UPDATE"#,
    )
    .bind(request.operation_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let completed = if let Some((owner, digest, row_count, status, live)) = existing {
        if owner != requested_by
            || digest != request_digest
            || row_count != i32::try_from(request.rows.len()).expect("validated row count")
        {
            return Err(AppError::conflict(
                "Import operation ID is bound to different input",
            ));
        }
        if !live {
            return Err(AppError::not_found("Import credential result has expired"));
        }
        status == 1
    } else {
        sqlx::query(
            r#"INSERT INTO "AdminCredentialJobs"
                   (operation_id, requested_by, request_digest, row_count)
               VALUES ($1, $2, $3, $4)"#,
        )
        .bind(request.operation_id)
        .bind(requested_by)
        .bind(request_digest)
        .bind(i32::try_from(request.rows.len()).expect("validated row count"))
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        false
    };

    let mut target_emails = request
        .rows
        .iter()
        .map(|row| row.email.trim().to_uppercase())
        .filter(|email| {
            email.contains('@')
                && (3..=crate::controllers::account::MAX_EMAIL_BYTES).contains(&email.len())
        })
        .collect::<Vec<_>>();
    target_emails.sort_unstable();
    target_emails.dedup();
    if !completed && !target_emails.is_empty() {
        let claimed = sqlx::query_scalar::<_, String>(
            r#"INSERT INTO "AdminCredentialTargetLeases"
                   (normalized_email, operation_id, expires_at_utc)
               SELECT email, $1, clock_timestamp() + INTERVAL '1 hour'
                 FROM UNNEST($2::text[]) AS email
               ON CONFLICT (normalized_email) DO UPDATE
                 SET operation_id = EXCLUDED.operation_id,
                     expires_at_utc = EXCLUDED.expires_at_utc
               WHERE "AdminCredentialTargetLeases".operation_id = EXCLUDED.operation_id
                  OR "AdminCredentialTargetLeases".expires_at_utc <= clock_timestamp()
            RETURNING normalized_email"#,
        )
        .bind(request.operation_id)
        .bind(&target_emails)
        .fetch_all(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if claimed.len() != target_emails.len() {
            return Err(AppError::conflict(
                "Another credential operation is active for an imported email",
            ));
        }
    }
    let encrypted_rows: Vec<(i32, Vec<u8>, Vec<u8>)> = sqlx::query_as(
        r#"SELECT row_index, result_ciphertext, result_nonce
             FROM "AdminCredentialJobRows"
            WHERE operation_id = $1 AND status = 1 ORDER BY row_index"#,
    )
    .bind(request.operation_id)
    .fetch_all(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    let mut rows = std::iter::repeat_with(|| None)
        .take(request.rows.len())
        .collect::<Vec<_>>();
    for (row_index, ciphertext, nonce) in encrypted_rows {
        let index = usize::try_from(row_index)
            .map_err(|_| AppError::internal("invalid persisted import row index"))?;
        let slot = rows
            .get_mut(index)
            .ok_or_else(|| AppError::internal("persisted import row is out of range"))?;
        *slot = Some(decrypt_import_result(
            &st.config.jwt_secret,
            request.operation_id,
            index,
            &ciphertext,
            &nonce,
        )?);
    }
    if completed && rows.iter().any(Option::is_none) {
        return Err(AppError::internal(
            "completed import is missing credential result rows",
        ));
    }
    Ok((completed, rows))
}

async fn complete_import_job(st: &SharedState, operation_id: Uuid) -> AppResult<()> {
    let mut transaction = st
        .pg()
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let completed = sqlx::query(
        r#"UPDATE "AdminCredentialJobs"
              SET status = 1, completed_at_utc = clock_timestamp(),
                  result_expires_at_utc = clock_timestamp() + INTERVAL '1 hour'
            WHERE operation_id = $1 AND status = 0
              AND row_count = (SELECT COUNT(*) FROM "AdminCredentialJobRows"
                                WHERE operation_id = $1 AND status = 1)"#,
    )
    .bind(operation_id)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    if completed != 1 {
        return Err(AppError::conflict(
            "Import operation is missing a completed durable row",
        ));
    }
    sqlx::query(r#"DELETE FROM "AdminCredentialTargetLeases" WHERE operation_id = $1"#)
        .bind(operation_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

/// `POST /api/admin/users/import` — CSV bulk import (client-parsed → JSON rows).
///
/// For each row: generate a username (override or derived from real name, made
/// unique) and a random password, create the user, and — per `teamMode` — join
/// or create a team. Duplicate EMAIL rows in the same request are rejected before
/// work begins, but a duplicate already in the DB UPDATES the existing user —
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
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<ImportRequest>,
) -> AppResult<Response> {
    validate_import_request(&req)?;
    let request_digest = import_request_digest(&req)?;
    let normalized_emails = req
        .rows
        .iter()
        .map(|row| row.email.trim().to_uppercase())
        .collect::<Vec<_>>();
    let source = crate::services::anti_cheat::client_ip(&headers, Some(peer.ip()))
        .unwrap_or_else(|| peer.ip().to_string());
    let scopes = admin_credential_scopes(st.pg(), &normalized_emails, &[], &source).await?;
    let scope_refs = scopes.iter().map(String::as_str).collect::<Vec<_>>();
    let mut credential_work = crate::services::credential_admission::try_acquire_scopes(
        st.pg(),
        crate::services::credential_admission::CredentialWorkClass::AdminBulk,
        &scope_refs,
    )
    .await?;
    let (completed, mut durable_rows) =
        begin_import_job(&st, caller.id, &req, &request_digest).await?;
    if completed {
        let users = durable_rows.into_iter().flatten().collect::<Vec<_>>();
        return Ok(super::users_credentials::private_no_store(Json(
            summarize_import(users, req.rows.len()),
        )));
    }
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

    for (row_index, row) in req.rows.iter().enumerate() {
        credential_work.renew_if_needed().await?;
        if let Some(result) = durable_rows[row_index].take() {
            seen_emails.insert(row.email.trim().to_uppercase());
            out.push(result);
            continue;
        }
        let row_lease = match claim_import_row(&st, req.operation_id, row_index).await? {
            ImportRowClaim::Owned(lease_token) => lease_token,
            ImportRowClaim::Completed(result) => {
                seen_emails.insert(row.email.trim().to_uppercase());
                out.push(result);
                continue;
            }
        };
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

        let field_validation = crate::controllers::account::validate_profile_fields(
            None,
            row.phone.as_deref(),
            Some(&real_name),
            row.std_number.as_deref(),
        )
        .and_then(|()| crate::controllers::team::validate_team_profile(team_name.as_deref(), None));
        if let Err(error) = field_validation {
            let result = skipped_row(
                &email,
                &real_name,
                &preview_name,
                team_name,
                &error.to_string(),
            );
            persist_and_push_import_result(
                &st,
                req.operation_id,
                row_index,
                row_lease,
                &mut out,
                result,
            )
            .await?;
            continue;
        }

        if !email.contains('@') || email.len() > crate::controllers::account::MAX_EMAIL_BYTES {
            let result = skipped_row(
                &email,
                &real_name,
                &preview_name,
                team_name,
                "invalid email address",
            );
            persist_and_push_import_result(
                &st,
                req.operation_id,
                row_index,
                row_lease,
                &mut out,
                result,
            )
            .await?;
            continue;
        }
        let norm_email = email.to_uppercase();
        if seen_emails.contains(&norm_email) {
            let result = skipped_row(
                &email,
                &real_name,
                &preview_name,
                team_name,
                "duplicate email in this import",
            );
            persist_and_push_import_result(
                &st,
                req.operation_id,
                row_index,
                row_lease,
                &mut out,
                result,
            )
            .await?;
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
            Err(error) => {
                let Some(reason) = terminal_import_row_reason(&error) else {
                    return Err(error);
                };
                let result = skipped_row(&email, &real_name, &preview_name, team_name, &reason);
                persist_and_push_import_result(
                    &st,
                    req.operation_id,
                    row_index,
                    row_lease,
                    &mut out,
                    result,
                )
                .await?;
                continue;
            }
        };
        credential_work.ensure_owned().await?;
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
            provision_import_user_durable(
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
                DurableImportResult {
                    state: &st,
                    operation_id: req.operation_id,
                    row_index,
                    lease_token: row_lease,
                    email: &email,
                    real_name: &real_name,
                    password: &password,
                    team_name: team_name.as_deref(),
                },
            )
            .await,
            row_number,
            "provision",
        ) {
            Ok(provision) => provision,
            Err(error) => {
                let Some(reason) = terminal_import_row_reason(&error) else {
                    return Err(error);
                };
                let result = skipped_row(&email, &real_name, &preview_name, team_name, &reason);
                persist_and_push_import_result(
                    &st,
                    req.operation_id,
                    row_index,
                    row_lease,
                    &mut out,
                    result,
                )
                .await?;
                continue;
            }
        };
        let (provision, durable_result) = provision;
        let provision = match provision {
            ImportProvision::Provisioned(provision) => provision,
            ImportProvision::Skipped(reason) => {
                let result = skipped_row(&email, &real_name, &preview_name, team_name, reason);
                persist_and_push_import_result(
                    &st,
                    req.operation_id,
                    row_index,
                    row_lease,
                    &mut out,
                    result,
                )
                .await?;
                continue;
            }
        };
        if let (Some(name), Some(team_id)) = (team_name.as_ref(), provision.team_id) {
            team_by_name.insert(name.clone(), team_id);
        }
        let result = durable_result.ok_or_else(|| {
            AppError::internal("provisioned import row is missing its durable credential result")
        })?;
        out.push(result);
    }

    complete_import_job(&st, req.operation_id).await?;
    let result = summarize_import(out, req.rows.len());

    crate::services::audit::info(
        &st,
        "AdminController",
        Some(caller.name.clone()),
        None,
        format!(
            "CSV import: {} created, {} updated, {} skipped",
            result.created, result.updated, result.skipped
        ),
    )
    .await;

    Ok(super::users_credentials::private_no_store(Json(result)))
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
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(models): Json<Vec<UserCreateModel>>,
) -> AppResult<MessageResponse> {
    if models.is_empty() || models.len() > 100 {
        return Err(AppError::payload_too_large(
            "Batch user creation accepts between 1 and 100 users",
        ));
    }
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
        if user_name.len() > crate::controllers::account::MAX_USER_NAME_BYTES {
            return Err(AppError::bad_request("Username is too long"));
        }
        if m.password.len() > crate::controllers::account::MAX_PASSWORD_BYTES {
            return Err(AppError::bad_request("Password is too long"));
        }
        crate::controllers::account::validate_profile_fields(
            None,
            m.phone.as_deref(),
            m.real_name.as_deref(),
            m.std_number.as_deref(),
        )?;
        crate::controllers::team::validate_team_profile(
            m.team_name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty()),
            None,
        )?;
        let email = m.email.trim().to_lowercase();
        if !email.contains('@') || email.len() > crate::controllers::account::MAX_EMAIL_BYTES {
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

    let normalized_names = prepared.iter().map(|row| row.3.clone()).collect::<Vec<_>>();
    let normalized_emails = prepared.iter().map(|row| row.4.clone()).collect::<Vec<_>>();
    let source = crate::services::anti_cheat::client_ip(&headers, Some(peer.ip()))
        .unwrap_or_else(|| peer.ip().to_string());
    let scopes =
        admin_credential_scopes(st.pg(), &normalized_emails, &normalized_names, &source).await?;
    let scope_refs = scopes.iter().map(String::as_str).collect::<Vec<_>>();
    let mut credential_work = crate::services::credential_admission::try_acquire_scopes(
        st.pg(),
        crate::services::credential_admission::CredentialWorkClass::AdminBulk,
        &scope_refs,
    )
    .await?;

    // ── Insert users, then wire up team membership ────────────────────────
    let now = Utc::now();
    // Track teams created/joined during this import so two rows naming the same
    // (new) team join one team instead of creating duplicates.
    let mut team_by_name: BTreeMap<String, i32> = BTreeMap::new();

    // RSCTF logs `users.Count` — the number of rows successfully upserted (created
    // + updated). Capture it before the consuming loop below.
    let created_count = prepared.len();
    let mut renamed_user_ids = Vec::new();

    // Each row commits independently. Always invalidate renames from earlier
    // rows even if a later hash/provisioning step fails and the endpoint returns
    // a partial-result error.
    let provision_result: AppResult<()> = async {
        for (m, user_name, email, norm_name, norm_email) in prepared {
            credential_work.renew_if_needed().await?;
            let password_hash = hash_password_async(m.password.clone()).await?;
            credential_work.ensure_owned().await?;
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
            if provision.user_name_changed {
                renamed_user_ids.push(provision.id);
            }
        }
        Ok(())
    }
    .await;
    if let Err(error) =
        crate::controllers::team::flush_scoreboards_for_users(&st, &renamed_user_ids).await
    {
        tracing::warn!(%error, renamed_users = renamed_user_ids.len(), "post-bulk-rename scoreboard invalidation deferred");
    }
    provision_result?;

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

#[path = "users_read.rs"]
mod read;
pub use read::{search_users, user_info};

#[cfg(test)]
#[path = "users_tests.rs"]
mod tests;
