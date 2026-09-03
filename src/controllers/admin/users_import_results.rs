//! Validation and encrypted replay material for bounded CSV user imports.

use super::users::{ImportRequest, ImportResult, ImportUserResult};
use super::*;
use aes_gcm::aead::consts::U12;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use sha2::{Digest, Sha256};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportJobStatus {
    pub operation_id: Uuid,
    pub status: String,
    pub total: usize,
    pub completed: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<ImportResult>,
}

const MAX_IMPORT_ROWS: usize = 200;
const MAX_IMPORT_TEAMS: usize = 100;
const MAX_IMPORT_EVENTS: usize = 10;
const MAX_IMPORT_TEAM_EVENT_ASSIGNMENTS: usize = 200;
const MAX_IMPORT_FIELD_BYTES: usize = 512;
const MAX_IMPORT_SOURCE_NAME_BYTES: usize = 255;

pub(super) enum ImportRowClaim {
    Owned(Uuid),
    Completed(ImportUserResult),
}

pub(super) fn validate_import_request(request: &ImportRequest) -> AppResult<()> {
    if request.operation_id.is_nil() {
        return Err(AppError::bad_request(
            "A valid import operation ID is required",
        ));
    }
    if request.rows.is_empty() || request.rows.len() > MAX_IMPORT_ROWS {
        return Err(AppError::payload_too_large(
            "Imports must contain between 1 and 200 rows",
        ));
    }
    if !matches!(request.team_mode.as_str(), "fromrow" | "single" | "none") {
        return Err(AppError::bad_request("Invalid import team mode"));
    }
    if request.event_assignments.len() > MAX_IMPORT_EVENTS {
        return Err(AppError::payload_too_large(
            "An import may assign teams to at most 10 events",
        ));
    }
    if !request.event_assignments.is_empty() && request.team_mode == "none" {
        return Err(AppError::bad_request(
            "Event enrollment requires team assignment",
        ));
    }
    let mut event_ids = std::collections::HashSet::new();
    for assignment in &request.event_assignments {
        if assignment.game_id <= 0 || assignment.division_id.is_some_and(|id| id <= 0) {
            return Err(AppError::bad_request("Invalid import event assignment"));
        }
        if !event_ids.insert(assignment.game_id) {
            return Err(AppError::bad_request(
                "An event can be selected only once per import",
            ));
        }
    }
    if request.source_name.as_deref().is_some_and(|value| {
        value.is_empty()
            || value.len() > MAX_IMPORT_SOURCE_NAME_BYTES
            || value.contains('/')
            || value.contains('\\')
    }) {
        return Err(AppError::bad_request("Invalid import file name"));
    }
    if request
        .single_team_name
        .as_deref()
        .is_some_and(|value| value.len() > MAX_IMPORT_FIELD_BYTES)
    {
        return Err(AppError::payload_too_large("Import field is too large"));
    }
    if request.team_mode == "single"
        && request
            .single_team_name
            .as_deref()
            .map(str::trim)
            .is_none_or(str::is_empty)
    {
        return Err(AppError::bad_request(
            "A team name is required for single-team import",
        ));
    }
    let mut teams = std::collections::HashSet::new();
    let mut emails = std::collections::HashSet::new();
    for row in &request.rows {
        for value in [
            Some(row.email.as_str()),
            Some(row.real_name.as_str()),
            row.user_name_override.as_deref(),
            row.team_name.as_deref(),
            row.std_number.as_deref(),
            row.phone.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if value.len() > MAX_IMPORT_FIELD_BYTES {
                return Err(AppError::payload_too_large("Import field is too large"));
            }
        }
        let email = row.email.trim().to_uppercase();
        if !emails.insert(email) {
            return Err(AppError::bad_request(
                "Import contains duplicate email addresses",
            ));
        }
        if let Some(team) = row
            .team_name
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            teams.insert(team.to_lowercase());
        }
    }
    if let Some(team) = request
        .single_team_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        teams.insert(team.to_lowercase());
    }
    if teams.len() > MAX_IMPORT_TEAMS {
        return Err(AppError::payload_too_large(
            "Import contains more than 100 distinct teams",
        ));
    }
    if !request.event_assignments.is_empty()
        && request.team_mode == "fromrow"
        && request.rows.iter().any(|row| {
            row.team_name
                .as_deref()
                .map(str::trim)
                .is_none_or(str::is_empty)
        })
    {
        return Err(AppError::bad_request(
            "Every imported row needs a team name when events are selected",
        ));
    }
    let effective_team_count = match request.team_mode.as_str() {
        "single" => usize::from(
            request
                .single_team_name
                .as_deref()
                .map(str::trim)
                .is_some_and(|name| !name.is_empty()),
        ),
        "fromrow" => request
            .rows
            .iter()
            .filter_map(|row| row.team_name.as_deref().map(str::trim))
            .filter(|name| !name.is_empty())
            .map(str::to_lowercase)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        _ => 0,
    };
    if effective_team_count.saturating_mul(request.event_assignments.len())
        > MAX_IMPORT_TEAM_EVENT_ASSIGNMENTS
    {
        return Err(AppError::payload_too_large(
            "An import may contain at most 200 team-event assignments",
        ));
    }
    Ok(())
}

pub(super) fn import_request_digest(request: &ImportRequest) -> AppResult<Vec<u8>> {
    let mut event_assignments = request.event_assignments.clone();
    event_assignments.sort_by_key(|assignment| (assignment.game_id, assignment.division_id));
    let canonical = serde_json::to_vec(&(
        &request.source_name,
        &request.rows,
        &request.team_mode,
        &request.single_team_name,
        &event_assignments,
        request.email_confirmed,
    ))
    .map_err(|error| AppError::internal(format!("serialize import request: {error}")))?;
    Ok(Sha256::digest(canonical).to_vec())
}

fn import_result_key(secret: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"rsctf:admin-import-result:v1\0");
    digest.update(secret.as_bytes());
    digest.finalize().into()
}

fn import_row_aad(operation_id: Uuid, row_index: usize) -> Vec<u8> {
    format!("v1:{operation_id}:{row_index}").into_bytes()
}

fn encrypt_import_result(
    secret: &str,
    operation_id: Uuid,
    row_index: usize,
    result: &ImportUserResult,
) -> AppResult<(Vec<u8>, [u8; 12])> {
    let plaintext = serde_json::to_vec(result)
        .map_err(|error| AppError::internal(format!("serialize import result: {error}")))?;
    let cipher = Aes256Gcm::new_from_slice(&import_result_key(secret))
        .map_err(|_| AppError::internal("initialize import result encryption"))?;
    let nonce: [u8; 12] = rand::random();
    let nonce_value: Nonce<U12> = nonce.into();
    let ciphertext = cipher
        .encrypt(
            &nonce_value,
            Payload {
                msg: &plaintext,
                aad: &import_row_aad(operation_id, row_index),
            },
        )
        .map_err(|_| AppError::internal("encrypt import result"))?;
    Ok((ciphertext, nonce))
}

pub(super) fn decrypt_import_result(
    secret: &str,
    operation_id: Uuid,
    row_index: usize,
    ciphertext: &[u8],
    nonce: &[u8],
) -> AppResult<ImportUserResult> {
    let cipher = Aes256Gcm::new_from_slice(&import_result_key(secret))
        .map_err(|_| AppError::internal("initialize import result encryption"))?;
    let nonce = Nonce::<U12>::try_from(nonce)
        .map_err(|_| AppError::internal("invalid import result nonce"))?;
    let plaintext = cipher
        .decrypt(
            &nonce,
            Payload {
                msg: ciphertext,
                aad: &import_row_aad(operation_id, row_index),
            },
        )
        .map_err(|_| AppError::unavailable("Import credential result cannot be recovered"))?;
    serde_json::from_slice(&plaintext)
        .map_err(|_| AppError::internal("invalid encrypted import result"))
}

pub(super) async fn store_import_result_in_transaction(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    st: &SharedState,
    operation_id: Uuid,
    row_index: usize,
    lease_token: Uuid,
    result: &ImportUserResult,
) -> AppResult<()> {
    let (ciphertext, nonce) =
        encrypt_import_result(&st.config.jwt_secret, operation_id, row_index, result)?;
    let updated = sqlx::query(
        r#"UPDATE "AdminCredentialJobRows"
              SET status = 1, result_ciphertext = $4, result_nonce = $5,
                  completed_at_utc = clock_timestamp()
            WHERE operation_id = $1 AND row_index = $2
              AND lease_token = $3 AND status = 0"#,
    )
    .bind(operation_id)
    .bind(i32::try_from(row_index).expect("validated import row index"))
    .bind(lease_token)
    .bind(ciphertext)
    .bind(nonce.as_slice())
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    if updated != 1 {
        return Err(AppError::conflict(
            "Import row operation lost its durable lease",
        ));
    }
    upsert_import_history_row(transaction, operation_id, row_index, result).await?;
    Ok(())
}

async fn upsert_import_history_row(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    operation_id: Uuid,
    row_index: usize,
    result: &ImportUserResult,
) -> AppResult<()> {
    let bounded_error = result
        .error
        .as_deref()
        .map(|error| truncate_utf8(error, 1_024));
    sqlx::query(
        r#"INSERT INTO "AdminUserImportHistoryRows"
               (operation_id, row_index, user_id, email, real_name,
                user_name, team_name, outcome, error)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
           ON CONFLICT (operation_id, row_index) DO UPDATE
             SET user_id = EXCLUDED.user_id,
                 email = EXCLUDED.email,
                 real_name = EXCLUDED.real_name,
                 user_name = EXCLUDED.user_name,
                 team_name = EXCLUDED.team_name,
                 outcome = EXCLUDED.outcome,
                 error = EXCLUDED.error"#,
    )
    .bind(operation_id)
    .bind(i32::try_from(row_index).expect("validated import row index"))
    .bind(result.user_id)
    .bind(&result.email)
    .bind(&result.real_name)
    .bind(&result.user_name)
    .bind(&result.team_name)
    .bind(&result.status)
    .bind(bounded_error)
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    value[..end].to_string()
}

async fn store_import_result(
    st: &SharedState,
    operation_id: Uuid,
    row_index: usize,
    lease_token: Uuid,
    result: &ImportUserResult,
) -> AppResult<()> {
    let mut transaction = st
        .pg()
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    store_import_result_in_transaction(
        &mut transaction,
        st,
        operation_id,
        row_index,
        lease_token,
        result,
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

pub(super) async fn persist_and_push_import_result(
    st: &SharedState,
    operation_id: Uuid,
    row_index: usize,
    lease_token: Uuid,
    out: &mut Vec<ImportUserResult>,
    result: ImportUserResult,
) -> AppResult<()> {
    let result = match store_import_result(st, operation_id, row_index, lease_token, &result).await
    {
        Ok(()) => result,
        Err(AppError::Conflict(_)) => load_completed_import_result(st, operation_id, row_index)
            .await?
            .ok_or_else(|| AppError::conflict("Import row is owned by another request"))?,
        Err(error) => return Err(error),
    };
    out.push(result);
    Ok(())
}

async fn load_completed_import_result(
    st: &SharedState,
    operation_id: Uuid,
    row_index: usize,
) -> AppResult<Option<ImportUserResult>> {
    let row: Option<(Vec<u8>, Vec<u8>)> = sqlx::query_as(
        r#"SELECT result_ciphertext, result_nonce
             FROM "AdminCredentialJobRows"
            WHERE operation_id = $1 AND row_index = $2 AND status = 1"#,
    )
    .bind(operation_id)
    .bind(i32::try_from(row_index).expect("validated import row index"))
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    row.map(|(ciphertext, nonce)| {
        decrypt_import_result(
            &st.config.jwt_secret,
            operation_id,
            row_index,
            &ciphertext,
            &nonce,
        )
    })
    .transpose()
}

/// Claim one uncommitted row before generating a password. A reclaimed row is
/// fenced by its lease token; an exact retry of a completed row receives the
/// authoritative encrypted credential instead of changing the password again.
pub(super) async fn claim_import_row(
    st: &SharedState,
    operation_id: Uuid,
    row_index: usize,
) -> AppResult<ImportRowClaim> {
    let lease_token = Uuid::new_v4();
    let row_index_i32 = i32::try_from(row_index).expect("validated import row index");
    let claimed: Option<Uuid> = sqlx::query_scalar(
        r#"INSERT INTO "AdminCredentialJobRows"
               (operation_id, row_index, lease_token, lease_expires_at_utc)
           VALUES ($1, $2, $3, clock_timestamp() + INTERVAL '2 minutes')
           ON CONFLICT (operation_id, row_index) DO UPDATE
             SET lease_token = EXCLUDED.lease_token,
                 lease_expires_at_utc = EXCLUDED.lease_expires_at_utc
           WHERE "AdminCredentialJobRows".status = 0
             AND "AdminCredentialJobRows".lease_expires_at_utc <= clock_timestamp()
        RETURNING lease_token"#,
    )
    .bind(operation_id)
    .bind(row_index_i32)
    .bind(lease_token)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if claimed == Some(lease_token) {
        return Ok(ImportRowClaim::Owned(lease_token));
    }
    if let Some(result) = load_completed_import_result(st, operation_id, row_index).await? {
        return Ok(ImportRowClaim::Completed(result));
    }
    Err(AppError::too_many_requests(1))
}

pub(super) fn summarize_import(users: Vec<ImportUserResult>, total: usize) -> ImportResult {
    ImportResult {
        total,
        created: users.iter().filter(|row| row.status == "created").count(),
        updated: users.iter().filter(|row| row.status == "updated").count(),
        skipped: users.iter().filter(|row| row.status == "skipped").count(),
        users,
    }
}

/// Recover one admin-owned import after a response loss or browser reload.
pub async fn recover_import_job(
    State(st): State<SharedState>,
    AdminUser(caller): AdminUser,
    Path(operation_id): Path<Uuid>,
) -> AppResult<Response> {
    if operation_id.is_nil() {
        return Err(AppError::bad_request(
            "A valid import operation ID is required",
        ));
    }

    let mut transaction = st
        .pg()
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let job = sqlx::query_as::<_, (i32, i16, bool)>(
        r#"SELECT row_count, status,
                  status = 0 OR COALESCE(result_expires_at_utc > clock_timestamp(), FALSE)
             FROM "AdminCredentialJobs"
            WHERE operation_id = $1 AND requested_by = $2
            FOR SHARE"#,
    )
    .bind(operation_id)
    .bind(caller.id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Import operation not found"))?;
    if !job.2 {
        return Err(AppError::not_found("Import credential result has expired"));
    }

    let total = usize::try_from(job.0)
        .map_err(|_| AppError::internal("Invalid persisted import row count"))?;
    let (completed, encrypted_rows) = if job.1 == 1 {
        let rows: Vec<(i32, Vec<u8>, Vec<u8>)> = sqlx::query_as(
            r#"SELECT row_index, result_ciphertext, result_nonce
                 FROM "AdminCredentialJobRows"
                WHERE operation_id = $1 AND status = 1
                ORDER BY row_index"#,
        )
        .bind(operation_id)
        .fetch_all(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        (rows.len(), Some(rows))
    } else {
        let count: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*) FROM "AdminCredentialJobRows"
                WHERE operation_id = $1 AND status = 1"#,
        )
        .bind(operation_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        (
            usize::try_from(count)
                .map_err(|_| AppError::internal("Invalid completed import row count"))?,
            None,
        )
    };
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    let result = if job.1 == 1 {
        if completed != total {
            return Err(AppError::internal(
                "Completed import is missing credential result rows",
            ));
        }
        let mut rows = Vec::with_capacity(completed);
        for (row_index, ciphertext, nonce) in encrypted_rows.unwrap_or_default() {
            let row_index = usize::try_from(row_index)
                .map_err(|_| AppError::internal("Invalid persisted import row index"))?;
            let result = decrypt_import_result(
                &st.config.jwt_secret,
                operation_id,
                row_index,
                &ciphertext,
                &nonce,
            )?;
            rows.push((row_index, result));
        }
        let mut history = st
            .pg()
            .begin()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        for (row_index, row) in &rows {
            upsert_import_history_row(&mut history, operation_id, *row_index, row).await?;
        }
        history
            .commit()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        Some(summarize_import(
            rows.into_iter().map(|(_, row)| row).collect(),
            total,
        ))
    } else {
        None
    };

    Ok(super::users_credentials::private_no_store(Json(
        ImportJobStatus {
            operation_id,
            status: if job.1 == 1 { "Completed" } else { "Running" }.to_string(),
            total,
            completed,
            result,
        },
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(operation_id: Uuid, emails: &[&str]) -> ImportRequest {
        ImportRequest {
            operation_id,
            source_name: Some("players.csv".to_string()),
            rows: emails
                .iter()
                .map(|email| super::super::users::ImportRow {
                    email: (*email).to_string(),
                    real_name: "Player".to_string(),
                    user_name_override: None,
                    team_name: None,
                    std_number: None,
                    phone: None,
                })
                .collect(),
            team_mode: "none".to_string(),
            single_team_name: None,
            event_assignments: Vec::new(),
            email_confirmed: true,
        }
    }

    #[test]
    fn nil_import_operation_is_rejected_before_work_admission() {
        assert!(matches!(
            validate_import_request(&request(Uuid::nil(), &["player@example.test"])),
            Err(AppError::BadRequest(_))
        ));
    }

    #[test]
    fn normalized_duplicate_email_is_rejected_before_hashing() {
        assert!(matches!(
            validate_import_request(&request(
                Uuid::new_v4(),
                &["player@example.test", " PLAYER@example.test "]
            )),
            Err(AppError::BadRequest(_))
        ));
    }

    #[test]
    fn oversized_single_team_name_is_rejected_before_database_work() {
        let mut request = request(Uuid::new_v4(), &["player@example.test"]);
        request.team_mode = "single".to_string();
        request.single_team_name = Some("x".repeat(MAX_IMPORT_FIELD_BYTES + 1));
        assert!(matches!(
            validate_import_request(&request),
            Err(AppError::PayloadTooLarge(_))
        ));
    }

    #[test]
    fn single_team_mode_requires_a_team_name() {
        let mut request = request(Uuid::new_v4(), &["player@example.test"]);
        request.team_mode = "single".to_string();
        assert!(matches!(
            validate_import_request(&request),
            Err(AppError::BadRequest(_))
        ));
    }

    #[test]
    fn event_assignment_requires_a_team_for_every_row() {
        let mut request = request(Uuid::new_v4(), &["player@example.test"]);
        request.team_mode = "fromrow".to_string();
        request.event_assignments = vec![super::super::users::ImportEventAssignment {
            game_id: 7,
            division_id: None,
        }];
        assert!(matches!(
            validate_import_request(&request),
            Err(AppError::BadRequest(_))
        ));
        request.rows[0].team_name = Some("Team Seven".to_string());
        assert!(validate_import_request(&request).is_ok());
    }

    #[test]
    fn duplicate_or_invalid_event_assignment_is_rejected() {
        let mut request = request(Uuid::new_v4(), &["player@example.test"]);
        request.team_mode = "single".to_string();
        request.single_team_name = Some("One Team".to_string());
        request.event_assignments = vec![
            super::super::users::ImportEventAssignment {
                game_id: 7,
                division_id: None,
            },
            super::super::users::ImportEventAssignment {
                game_id: 7,
                division_id: Some(2),
            },
        ];
        assert!(matches!(
            validate_import_request(&request),
            Err(AppError::BadRequest(_))
        ));
        request.event_assignments = vec![super::super::users::ImportEventAssignment {
            game_id: 0,
            division_id: None,
        }];
        assert!(matches!(
            validate_import_request(&request),
            Err(AppError::BadRequest(_))
        ));
    }

    #[test]
    fn event_assignments_are_part_of_the_idempotency_digest() {
        let operation_id = Uuid::new_v4();
        let mut request = request(operation_id, &["player@example.test"]);
        let baseline = import_request_digest(&request).unwrap();
        request.team_mode = "single".to_string();
        request.single_team_name = Some("One Team".to_string());
        request.event_assignments = vec![super::super::users::ImportEventAssignment {
            game_id: 7,
            division_id: None,
        }];
        let assigned = import_request_digest(&request).unwrap();
        assert_ne!(baseline, assigned);

        request
            .event_assignments
            .push(super::super::users::ImportEventAssignment {
                game_id: 3,
                division_id: Some(4),
            });
        let first_order = import_request_digest(&request).unwrap();
        request.event_assignments.reverse();
        assert_eq!(first_order, import_request_digest(&request).unwrap());
    }
}
