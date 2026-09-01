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
const MAX_IMPORT_FIELD_BYTES: usize = 512;

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
    if request
        .single_team_name
        .as_deref()
        .is_some_and(|value| value.len() > MAX_IMPORT_FIELD_BYTES)
    {
        return Err(AppError::payload_too_large("Import field is too large"));
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
    Ok(())
}

pub(super) fn import_request_digest(request: &ImportRequest) -> AppResult<Vec<u8>> {
    let canonical = serde_json::to_vec(&(
        &request.rows,
        &request.team_mode,
        &request.single_team_name,
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
    Ok(())
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
            rows.push(decrypt_import_result(
                &st.config.jwt_secret,
                operation_id,
                row_index,
                &ciphertext,
                &nonce,
            )?);
        }
        Some(summarize_import(rows, total))
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
}
