//! edit: flag CRUD (see edit/mod.rs for the router + shared DTOs/helpers).
use super::*;
use sha2::{Digest, Sha256};

const MAX_FLAGS_PER_IMPORT: usize = 100;
const MAX_FLAGS_PER_CHALLENGE: i64 = 512;
const MAX_FLAG_BYTES: usize = 127;
const MAX_FLAG_REMOTE_URL_BYTES: usize = 2_048;
const MAX_FLAG_FILE_HASH_BYTES: usize = 256;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlagImportResult {
    pub inserted: i32,
    pub duplicates: i32,
}

fn validate_flag_import(flags: &[FlagCreateModel]) -> AppResult<()> {
    if flags.is_empty() || flags.len() > MAX_FLAGS_PER_IMPORT {
        return Err(AppError::payload_too_large(format!(
            "A flag import must contain 1 to {MAX_FLAGS_PER_IMPORT} rows"
        )));
    }
    for model in flags {
        if model.flag.is_empty() || model.flag.len() > MAX_FLAG_BYTES {
            return Err(AppError::bad_request(format!(
                "Every flag must contain 1 to {MAX_FLAG_BYTES} UTF-8 bytes"
            )));
        }
        if model
            .remote_url
            .as_ref()
            .is_some_and(|value| value.len() > MAX_FLAG_REMOTE_URL_BYTES)
            || model
                .file_hash
                .as_ref()
                .is_some_and(|value| value.len() > MAX_FLAG_FILE_HASH_BYTES)
        {
            return Err(AppError::payload_too_large(
                "A flag attachment URL or hash exceeds its byte limit",
            ));
        }
    }
    Ok(())
}

async fn abandon_flag_import(pool: &sqlx::PgPool, challenge_id: i32, operation_id: Uuid) {
    if let Err(error) = sqlx::query(
        r#"DELETE FROM "FlagImportOperations"
            WHERE challenge_id = $1 AND operation_id = $2 AND state = 0"#,
    )
    .bind(challenge_id)
    .bind(operation_id)
    .execute(pool)
    .await
    {
        tracing::warn!(%error, challenge_id, %operation_id, "failed to abandon flag import reservation");
    }
}

async fn reserve_flag_import(
    pool: &sqlx::PgPool,
    challenge_id: i32,
    actor_user_id: Uuid,
    operation_id: Uuid,
    request_digest: &[u8],
) -> AppResult<Option<FlagImportResult>> {
    let inserted = sqlx::query_scalar::<_, bool>(
        r#"INSERT INTO "FlagImportOperations"
             (challenge_id, operation_id, actor_user_id, request_digest)
           VALUES ($1, $2, $3, $4)
           ON CONFLICT (challenge_id, operation_id) DO NOTHING
           RETURNING TRUE"#,
    )
    .bind(challenge_id)
    .bind(operation_id)
    .bind(actor_user_id)
    .bind(request_digest)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if inserted.is_some() {
        return Ok(None);
    }
    let stored = sqlx::query_as::<_, (Uuid, Vec<u8>, i16, Option<i32>, Option<i32>, bool)>(
        r#"SELECT actor_user_id, request_digest, state, inserted_count,
                  duplicate_count, lease_expires_at_utc <= clock_timestamp()
             FROM "FlagImportOperations"
            WHERE challenge_id = $1 AND operation_id = $2"#,
    )
    .bind(challenge_id)
    .bind(operation_id)
    .fetch_one(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if stored.0 != actor_user_id || stored.1 != request_digest {
        return Err(AppError::conflict(
            "The operation ID is already bound to another flag import",
        ));
    }
    if stored.2 == 1 {
        return Ok(Some(FlagImportResult {
            inserted: stored.3.unwrap_or_default(),
            duplicates: stored.4.unwrap_or_default(),
        }));
    }
    if !stored.5 {
        return Err(AppError::conflict(
            "This flag import is still running; retry its operation ID later",
        ));
    }
    let reclaimed = sqlx::query(
        r#"UPDATE "FlagImportOperations"
              SET lease_expires_at_utc = clock_timestamp() + INTERVAL '5 minutes'
            WHERE challenge_id = $1 AND operation_id = $2 AND state = 0
              AND lease_expires_at_utc <= clock_timestamp()"#,
    )
    .bind(challenge_id)
    .bind(operation_id)
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if reclaimed.rows_affected() != 1 {
        return Err(AppError::conflict(
            "This flag import was reclaimed by another request",
        ));
    }
    Ok(None)
}

struct PreparedFlag {
    flag: String,
    file_type: FileType,
    remote_url: Option<String>,
    upload_stage: Option<crate::services::blob_refs::StagedBlob>,
}

async fn prepare_flag(
    st: &SharedState,
    user_id: Uuid,
    model: FlagCreateModel,
) -> AppResult<PreparedFlag> {
    let file_type = model.attachment_type.unwrap_or(FileType::None);
    let remote_url = match file_type {
        FileType::Remote => Some(challenges::validate_remote_attachment_url(
            model.remote_url.as_deref().unwrap_or_default(),
        )?),
        _ => None,
    };
    let upload_stage = match (file_type, model.upload_id) {
        (FileType::Local, Some(upload_id)) => {
            let hash = model
                .file_hash
                .as_deref()
                .ok_or_else(|| AppError::bad_request("A local attachment requires fileHash"))?;
            Some(
                crate::services::blob_refs::load_ready_upload_stage(
                    st.pg(),
                    upload_id,
                    user_id,
                    hash,
                )
                .await?,
            )
        }
        (FileType::Local, None) => {
            return Err(AppError::bad_request(
                "A local flag attachment requires its uploadId",
            ));
        }
        (_, Some(_)) => {
            return Err(AppError::bad_request(
                "An uploadId requires a local attachment",
            ));
        }
        (_, None) => None,
    };
    Ok(PreparedFlag {
        flag: model.flag,
        file_type,
        remote_url,
        upload_stage,
    })
}

async fn insert_flag_attachment_locked(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    prepared: &PreparedFlag,
) -> AppResult<Option<i32>> {
    if prepared.file_type == FileType::None {
        return Ok(None);
    }
    let id = sqlx::query_scalar::<_, i32>(
        r#"INSERT INTO "Attachments" ("Type", remote_url, local_file_id)
           VALUES ($1, $2, NULL) RETURNING id"#,
    )
    .bind(prepared.file_type as i16)
    .bind(prepared.remote_url.as_deref())
    .fetch_one(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if prepared.file_type == FileType::Local {
        let stage = prepared.upload_stage.as_ref().ok_or_else(|| {
            AppError::bad_request("A local flag attachment requires its uploadId")
        })?;
        let local_file_id = crate::services::blob_refs::publish_staged_blob_for_owner(
            transaction,
            stage,
            &format!("attachment:{id}"),
        )
        .await?;
        sqlx::query(r#"UPDATE "Attachments" SET local_file_id = $2 WHERE id = $1"#)
            .bind(id)
            .bind(local_file_id)
            .execute(&mut **transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    }
    Ok(Some(id))
}

fn validate_authored_flags(models: &[FlagCreateModel]) -> AppResult<()> {
    for model in models {
        crate::utils::flag_policy::validate_normal(&model.flag)
            .map_err(|error| AppError::bad_request(error.to_string()))?;
    }
    Ok(())
}

/// The accepted static flags and a dynamic flag template are scoring policy,
/// not ordinary challenge content. Production callers own the game-control
/// lock and the challenge-scoped JFLG lock, making this decision linearizable
/// with both engine startup and a first accepted submit.
async fn ensure_flag_policy_mutable_locked(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<()> {
    let challenge_exists = sqlx::query_scalar::<_, bool>(
        r#"SELECT TRUE FROM "GameChallenges"
            WHERE game_id = $1 AND id = $2"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Challenge not found"))?;
    debug_assert!(challenge_exists);
    let scoring_started = competition_scoring_started_locked(connection, game_id).await?;
    if scoring_started {
        return Err(AppError::bad_request(
            "Challenge flags are locked after competition scoring has started.",
        ));
    }
    Ok(())
}

/// Take the challenge-definition fence on the transaction that already owns
/// the broader game-control lock. Flag policy is part of the challenge
/// definition, but checking out a second pooled connection here can deadlock a
/// small pool when organizers edit several different games concurrently.
async fn acquire_flag_definition_lock(
    control: &mut crate::services::ad_engine::GameControlLock,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<()> {
    crate::utils::single_flight::acquire_transaction_advisory_lock(
        control.transaction_mut(),
        &crate::services::challenge_workloads::definition_lock_key(game_id, challenge_id),
    )
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

/// `POST /api/edit/games/{id}/challenges/{cId}/flags` — void.
pub async fn add_flags(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, c_id)): Path<(i32, i32)>,
    Json(request): Json<FlagImportRequest>,
) -> AppResult<RequestResponse<FlagImportResult>> {
    manager_or_admin(&st, &user, id).await?;
    challenges::reject_pending_mutation(st.pg(), id, c_id).await?;
    load_challenge(&st, id, c_id).await?;
    if request.operation_id.is_nil() {
        return Err(AppError::bad_request(
            "Flag import operation ID is required",
        ));
    }
    validate_flag_import(&request.flags)?;
    let request_digest = Sha256::digest(
        serde_json::to_vec(&request.flags)
            .map_err(|error| AppError::internal(error.to_string()))?,
    )
    .to_vec();
    if let Some(result) = reserve_flag_import(
        st.pg(),
        c_id,
        user.id,
        request.operation_id,
        &request_digest,
    )
    .await?
    {
        return Ok(RequestResponse::ok(result));
    }

    let mut seen = std::collections::HashSet::with_capacity(request.flags.len());
    let mut body_duplicates = 0_i32;
    let models = request
        .flags
        .into_iter()
        .filter(|model| {
            let unique = seen.insert(model.flag.clone());
            body_duplicates += i32::from(!unique);
            unique
        })
        .collect::<Vec<_>>();

    // Reject impossible answers before attachment staging or lock acquisition.
    // Player submissions trim the answer and accept at most 127 UTF-8 bytes, so
    // every authored static value must already be in that exact canonical form.
    validate_authored_flags(&models)?;

    // Storage already completed under durable upload stages. Resolve immutable
    // metadata without a domain lock; publication remains atomic with the flag.
    let mut flags = Vec::with_capacity(models.len());
    for model in models {
        match prepare_flag(&st, user.id, model).await {
            Ok(prepared) => flags.push(prepared),
            Err(error) => {
                abandon_flag_import(st.pg(), c_id, request.operation_id).await;
                return Err(error);
            }
        }
    }

    // Global order is game-control -> challenge definition -> JFLG. The game
    // lock prevents an A&D/KotH first round from crossing the policy check;
    // JFLG provides the corresponding first-Jeopardy-solve fence.
    let mut game_control = match crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await
    {
        Ok(lock) => lock,
        Err(error) => {
            abandon_flag_import(st.pg(), c_id, request.operation_id).await;
            return Err(error);
        }
    };
    if let Err(error) = acquire_flag_definition_lock(&mut game_control, id, c_id).await {
        drop(game_control);
        abandon_flag_import(st.pg(), c_id, request.operation_id).await;
        return Err(error);
    }
    let mutation: AppResult<(i32, std::collections::HashSet<String>)> = async {
        challenges::reject_pending_mutation(&mut **game_control.transaction_mut(), id, c_id)
            .await?;
        ensure_flag_policy_mutable_locked(game_control.transaction_mut(), id, c_id).await?;
        crate::utils::scoring::lock_jeopardy_flags_exclusive(game_control.transaction_mut(), c_id)
            .await?;

        let current_count = sqlx::query_scalar::<_, i64>(
            r#"SELECT COUNT(*)::bigint FROM "FlagContexts" WHERE challenge_id = $1"#,
        )
        .bind(c_id)
        .fetch_one(&mut **game_control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if current_count > MAX_FLAGS_PER_CHALLENGE {
            return Err(AppError::payload_too_large(
                "This challenge already exceeds the editable flag limit",
            ));
        }

        let values = flags
            .iter()
            .map(|prepared| prepared.flag.clone())
            .collect::<Vec<_>>();
        let existing = sqlx::query_scalar::<_, String>(
            r#"SELECT flag FROM "FlagContexts"
                WHERE challenge_id = $1 AND flag = ANY($2)"#,
        )
        .bind(c_id)
        .bind(&values)
        .fetch_all(&mut **game_control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
        let new_count = flags.len() as i64 - existing.len() as i64;
        if current_count + new_count > MAX_FLAGS_PER_CHALLENGE {
            return Err(AppError::payload_too_large(format!(
                "A challenge may contain at most {MAX_FLAGS_PER_CHALLENGE} flags"
            )));
        }

        let mut inserted_set = std::collections::HashSet::with_capacity(new_count as usize);
        for prepared in &flags {
            if existing.contains(&prepared.flag) {
                continue;
            }
            let attachment_id =
                insert_flag_attachment_locked(game_control.transaction_mut(), prepared).await?;
            sqlx::query(
                r#"INSERT INTO "FlagContexts"
                     (flag, is_occupied, challenge_id, attachment_id)
                   VALUES ($1, FALSE, $2, $3)"#,
            )
            .bind(&prepared.flag)
            .bind(c_id)
            .bind(attachment_id)
            .execute(&mut **game_control.transaction_mut())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
            inserted_set.insert(prepared.flag.clone());
        }

        let inserted_count = inserted_set.len() as i32;
        let duplicate_count = body_duplicates + flags.len() as i32 - inserted_count;
        sqlx::query(
            r#"UPDATE "FlagImportOperations"
                  SET state = 1, inserted_count = $3, duplicate_count = $4,
                      completed_at_utc = clock_timestamp()
                WHERE challenge_id = $1 AND operation_id = $2 AND state = 0"#,
        )
        .bind(c_id)
        .bind(request.operation_id)
        .bind(inserted_count)
        .bind(duplicate_count)
        .execute(&mut **game_control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        sqlx::query(
            r#"WITH expired AS (
                   SELECT challenge_id, operation_id
                     FROM "FlagImportOperations"
                    WHERE state = 1
                      AND completed_at_utc < clock_timestamp() - INTERVAL '30 days'
                    ORDER BY completed_at_utc, challenge_id, operation_id
                    LIMIT 128
               )
               DELETE FROM "FlagImportOperations" operation
                USING expired
                WHERE operation.challenge_id = expired.challenge_id
                  AND operation.operation_id = expired.operation_id"#,
        )
        .execute(&mut **game_control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        Ok((duplicate_count, inserted_set))
    }
    .await;

    let (duplicates, inserted_set) = match mutation {
        Ok(result) => result,
        Err(error) => {
            drop(game_control);
            abandon_flag_import(st.pg(), c_id, request.operation_id).await;
            return Err(error);
        }
    };
    if let Err(error) = game_control.release().await {
        // Commit acknowledgement is ambiguous. Leave the durable operation and
        // ready upload stages intact so the exact replay can recover safely.
        return Err(AppError::internal(error.to_string()));
    }
    Ok(RequestResponse::ok(FlagImportResult {
        inserted: inserted_set.len() as i32,
        duplicates,
    }))
}

pub(crate) async fn load_flags(st: &SharedState, c_id: i32) -> AppResult<Vec<FlagInfoModel>> {
    #[derive(sqlx::FromRow)]
    struct FlagRow {
        id: i32,
        flag: String,
        attachment_id: Option<i32>,
        file_type: Option<i16>,
        remote_url: Option<String>,
        file_hash: Option<String>,
        file_name: Option<String>,
        file_size: Option<i64>,
    }

    let rows = sqlx::query_as::<_, FlagRow>(
        r#"SELECT context.id, context.flag,
                  attachment.id AS attachment_id,
                  attachment."Type" AS file_type,
                  attachment.remote_url,
                  file.hash AS file_hash,
                  file.name AS file_name,
                  file.file_size
             FROM "FlagContexts" context
             LEFT JOIN "Attachments" attachment ON attachment.id = context.attachment_id
             LEFT JOIN "Files" file ON file.id = attachment.local_file_id
            WHERE context.challenge_id = $1
            ORDER BY context.id
            LIMIT 513"#,
    )
    .bind(c_id)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if rows.len() > 512 {
        return Err(AppError::payload_too_large(
            "This challenge exceeds the editable flag limit; remove legacy flags first",
        ));
    }

    rows.into_iter()
        .map(|row| {
            let attachment = match (row.attachment_id, row.file_type) {
                (Some(id), Some(raw_type)) => {
                    let file_type = match raw_type {
                        value if value == FileType::None as i16 => FileType::None,
                        value if value == FileType::Local as i16 => FileType::Local,
                        value if value == FileType::Remote as i16 => FileType::Remote,
                        _ => return Err(AppError::internal("Invalid attachment type")),
                    };
                    let (url, file_size) = match file_type {
                        FileType::None => (None, None),
                        FileType::Remote => (row.remote_url, None),
                        FileType::Local => (
                            row.file_hash
                                .zip(row.file_name)
                                .map(|(hash, name)| format!("/assets/{hash}/{name}")),
                            row.file_size,
                        ),
                    };
                    Some(AttachmentInfoModel {
                        id,
                        file_type,
                        url,
                        file_size,
                    })
                }
                _ => None,
            };
            Ok(FlagInfoModel {
                id: row.id,
                flag: row.flag,
                attachment,
            })
        })
        .collect()
}

#[cfg(test)]
mod policy_tests {
    use super::*;

    fn model(flag: String) -> FlagCreateModel {
        FlagCreateModel {
            flag,
            attachment_type: None,
            file_hash: None,
            upload_id: None,
            remote_url: None,
        }
    }

    #[test]
    fn direct_editor_rejects_an_unsubmittable_flag_before_attachment_work() {
        assert!(validate_authored_flags(&[model("x".repeat(127))]).is_ok());
        assert!(validate_authored_flags(&[model("x".repeat(128))]).is_err());
        assert!(validate_authored_flags(&[model(format!("{}x", "界".repeat(42)))]).is_ok());
        assert!(validate_authored_flags(&[model(format!("{}xx", "界".repeat(42)))]).is_err());
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn flag_definition_fence_reuses_the_single_game_control_connection() {
        use sea_orm::SqlxPostgresConnector;
        use sqlx::postgres::PgPoolOptions;

        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("connect single-connection test pool");
        let database = SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone());
        let identity = Uuid::new_v4().as_u128();
        let game_id = ((identity & 0x3fff_ffff) as i32).max(1);
        let challenge_id = (((identity >> 32) & 0x3fff_ffff) as i32).max(1);

        let mut control = crate::services::ad_engine::acquire_ad_game_lock(&database, game_id)
            .await
            .expect("acquire game control");
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            acquire_flag_definition_lock(&mut control, game_id, challenge_id),
        )
        .await
        .expect("definition fence tried to check out a second pool connection")
        .expect("acquire definition fence on retained transaction");
        let value = sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(&mut **control.transaction_mut())
            .await
            .expect("retained connection remains usable");
        assert_eq!(value, 1);
        control.release().await.expect("release game control");
        pool.close().await;
    }
}

/// `DELETE /api/edit/games/{id}/challenges/{cId}/flags/{fId}` — returns a
/// `TaskStatus`. RSCTF serializes this enum as a **string**, so we emit the
/// string literal directly (the port's `TaskStatus` enum is int-repr).
pub async fn remove_flag(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, c_id, f_id)): Path<(i32, i32, i32)>,
) -> AppResult<RequestResponse<String>> {
    manager_or_admin(&st, &user, id).await?;
    let mut game_control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await?;
    acquire_flag_definition_lock(&mut game_control, id, c_id).await?;
    let removal = match remove_flag_locked(game_control.transaction_mut(), id, c_id, f_id).await {
        Ok(removal) => removal,
        Err(error) => {
            drop(game_control);
            return Err(error);
        }
    };
    game_control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let Some(deleted_hash) = removal else {
        return Ok(RequestResponse::ok("NotFound".to_string()));
    };
    if let Some(hash) = deleted_hash {
        if let Err(error) =
            crate::services::blob_refs::purge_if_unreferenced(st.pg(), st.storage.as_ref(), &hash)
                .await
        {
            tracing::warn!(%error, %hash, f_id, "removed flag attachment blob purge deferred");
        }
    }
    Ok(RequestResponse::ok("Success".to_string()))
}

/// Delete the flag and consume its now-orphaned attachment reference in the
/// retained definition transaction. `None` means the flag did not exist;
/// `Some(None)` means it existed without a local blob requiring purge.
pub(super) async fn remove_flag_locked(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    game_id: i32,
    challenge_id: i32,
    flag_id: i32,
) -> AppResult<Option<Option<String>>> {
    challenges::reject_pending_mutation(&mut **transaction, game_id, challenge_id).await?;
    ensure_flag_policy_mutable_locked(transaction, game_id, challenge_id).await?;
    crate::utils::scoring::lock_jeopardy_flags_exclusive(transaction, challenge_id).await?;

    // Capture the hand-out attachment in the same statement that removes the
    // flag. The exclusive advisory lock makes this deletion linearizable with
    // every authoritative submit-side grade, including static flag inserts.
    let attachment_id: Option<Option<i32>> = sqlx::query_scalar(
        r#"DELETE FROM "FlagContexts"
            WHERE id = $1 AND challenge_id = $2
            RETURNING attachment_id"#,
    )
    .bind(flag_id)
    .bind(challenge_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some(attachment_id) = attachment_id else {
        return Ok(None);
    };
    let deleted_hash = match attachment_id {
        Some(attachment_id) => {
            crate::services::blob_refs::delete_attachment_locked(transaction, attachment_id).await?
        }
        None => None,
    };
    Ok(Some(deleted_hash))
}

// ============================================================================
//  Notices
// ============================================================================
