//! Validation and encrypted replay material for bounded CSV user imports.

use super::users::{ImportRequest, ImportResult, ImportUserResult};
use super::*;
use aes_gcm::aead::consts::U12;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use sha2::{Digest, Sha256};

const MAX_IMPORT_ROWS: usize = 200;
const MAX_IMPORT_TEAMS: usize = 100;
const MAX_IMPORT_FIELD_BYTES: usize = 512;

pub(super) fn validate_import_request(request: &ImportRequest) -> AppResult<()> {
    if request.rows.is_empty() || request.rows.len() > MAX_IMPORT_ROWS {
        return Err(AppError::payload_too_large(
            "Imports must contain between 1 and 200 rows",
        ));
    }
    if !matches!(request.team_mode.as_str(), "fromrow" | "single" | "none") {
        return Err(AppError::bad_request("Invalid import team mode"));
    }
    let mut teams = std::collections::HashSet::new();
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
    result: &ImportUserResult,
) -> AppResult<()> {
    let (ciphertext, nonce) =
        encrypt_import_result(&st.config.jwt_secret, operation_id, row_index, result)?;
    sqlx::query(
        r#"INSERT INTO "AdminCredentialJobRows"
               (operation_id, row_index, result_ciphertext, result_nonce)
           VALUES ($1, $2, $3, $4)
           ON CONFLICT (operation_id, row_index) DO NOTHING"#,
    )
    .bind(operation_id)
    .bind(i32::try_from(row_index).expect("validated import row index"))
    .bind(ciphertext)
    .bind(nonce.as_slice())
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

async fn store_import_result(
    st: &SharedState,
    operation_id: Uuid,
    row_index: usize,
    result: &ImportUserResult,
) -> AppResult<()> {
    let mut transaction = st
        .pg()
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    store_import_result_in_transaction(&mut transaction, st, operation_id, row_index, result)
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
    out: &mut Vec<ImportUserResult>,
    result: ImportUserResult,
) -> AppResult<()> {
    store_import_result(st, operation_id, row_index, &result).await?;
    out.push(result);
    Ok(())
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
