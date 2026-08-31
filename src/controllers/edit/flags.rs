//! edit: flag CRUD (see edit/mod.rs for the router + shared DTOs/helpers).
use super::*;
use sha2::{Digest, Sha256};

const MAX_FLAGS_PER_IMPORT: usize = 100;
const MAX_FLAGS_PER_CHALLENGE: i64 = 512;
const MAX_PENDING_FLAG_IMPORTS: i64 = 64;
const MAX_FLAG_BYTES: usize = crate::utils::flag_policy::NORMAL_FLAG_MAX_BYTES;
const MAX_FLAG_REMOTE_URL_BYTES: usize = 2_048;
const MAX_FLAG_FILE_HASH_BYTES: usize = 256;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlagImportResult {
    pub inserted: i32,
    pub duplicates: i32,
}

#[derive(Debug)]
enum FlagImportMutation {
    Applied {
        duplicates: i32,
        inserted: std::collections::HashSet<String>,
    },
    Replayed(FlagImportResult),
}

#[derive(Debug)]
enum FlagImportReservation {
    Acquired(Uuid),
    Replayed(FlagImportResult),
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

async fn abandon_flag_import(
    pool: &sqlx::PgPool,
    challenge_id: i32,
    operation_id: Uuid,
    lease_token: Uuid,
) {
    if let Err(error) = sqlx::query(
        r#"WITH removed AS (
               DELETE FROM "FlagImportOperations"
                WHERE challenge_id = $1 AND operation_id = $2 AND state = 0
                  AND lease_token = $3
              RETURNING 1
           )
           UPDATE "FlagImportSlots"
              SET lease_token = NULL, expires_at_utc = NULL
            WHERE lease_token = $3 AND EXISTS (SELECT 1 FROM removed)"#,
    )
    .bind(challenge_id)
    .bind(operation_id)
    .bind(lease_token)
    .execute(pool)
    .await
    {
        tracing::warn!(%error, challenge_id, %operation_id, "failed to abandon flag import reservation");
    }
}

async fn claim_flag_import_slot(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    lease_token: Uuid,
) -> AppResult<bool> {
    let claimed = sqlx::query_scalar::<_, i16>(
        r#"WITH candidate AS (
               SELECT slot_id FROM "FlagImportSlots"
                WHERE lease_token IS NULL OR expires_at_utc <= clock_timestamp()
                ORDER BY slot_id FOR UPDATE SKIP LOCKED LIMIT 1
           )
           UPDATE "FlagImportSlots" slot
              SET lease_token = $1,
                  expires_at_utc = clock_timestamp() + INTERVAL '5 minutes'
             FROM candidate
            WHERE slot.slot_id = candidate.slot_id
           RETURNING slot.slot_id"#,
    )
    .bind(lease_token)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(claimed.is_some())
}

async fn reserve_flag_import(
    pool: &sqlx::PgPool,
    challenge_id: i32,
    actor_user_id: Uuid,
    operation_id: Uuid,
    request_digest: &[u8],
) -> AppResult<FlagImportReservation> {
    let lease_token = Uuid::new_v4();
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let admission_owner: bool = sqlx::query_scalar(
        "SELECT pg_try_advisory_xact_lock(hashtextextended('rsctf:flag-import-admission', 0))",
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !admission_owner {
        return Err(AppError::too_many_requests(1));
    }

    // A client that disappears cannot retain one staging slot or one pending
    // identity forever. Keep the expired identity as a bounded tombstone so a
    // late exact replay is rejected instead of silently starting new work.
    sqlx::query(
        r#"WITH candidates AS (
               SELECT challenge_id, operation_id
                 FROM "FlagImportOperations"
                WHERE state = 0
                  AND lease_expires_at_utc <= clock_timestamp()
                  AND created_at_utc < clock_timestamp() - INTERVAL '1 hour'
                ORDER BY created_at_utc, challenge_id, operation_id
                LIMIT 128 FOR UPDATE SKIP LOCKED
           ), expired AS (
               UPDATE "FlagImportOperations" operation
                  SET state = 2, completed_at_utc = clock_timestamp()
                 FROM candidates
                WHERE operation.challenge_id = candidates.challenge_id
                  AND operation.operation_id = candidates.operation_id
              RETURNING operation.lease_token
           )
           UPDATE "FlagImportSlots"
              SET lease_token = NULL, expires_at_utc = NULL
            WHERE lease_token IN (SELECT lease_token FROM expired)"#,
    )
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let stored = sqlx::query_as::<_, (Uuid, Vec<u8>, i16, Option<i32>, Option<i32>, bool)>(
        r#"SELECT actor_user_id, request_digest, state, inserted_count,
                  duplicate_count, lease_expires_at_utc <= clock_timestamp()
             FROM "FlagImportOperations"
            WHERE challenge_id = $1 AND operation_id = $2"#,
    )
    .bind(challenge_id)
    .bind(operation_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if let Some(stored) = stored {
        if stored.0 != actor_user_id || stored.1 != request_digest {
            return Err(AppError::conflict(
                "The operation ID is already bound to another flag import",
            ));
        }
        if stored.2 == 1 {
            transaction
                .commit()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            return Ok(FlagImportReservation::Replayed(FlagImportResult {
                inserted: stored.3.unwrap_or_default(),
                duplicates: stored.4.unwrap_or_default(),
            }));
        }
        if stored.2 == 2 {
            return Err(AppError::conflict(
                "This expired flag import can no longer be resumed",
            ));
        }
        if !stored.5 {
            return Err(AppError::conflict(
                "This flag import is still running; retry its operation ID later",
            ));
        }
        if !claim_flag_import_slot(&mut transaction, lease_token).await? {
            return Err(AppError::too_many_requests(1));
        }
        let reclaimed = sqlx::query(
            r#"UPDATE "FlagImportOperations"
                  SET lease_token = $3,
                      lease_expires_at_utc = clock_timestamp() + INTERVAL '5 minutes'
                WHERE challenge_id = $1 AND operation_id = $2 AND state = 0
                  AND lease_expires_at_utc <= clock_timestamp()"#,
        )
        .bind(challenge_id)
        .bind(operation_id)
        .bind(lease_token)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if reclaimed.rows_affected() != 1 {
            return Err(AppError::conflict(
                "This flag import was reclaimed by another request",
            ));
        }
    } else {
        let pending = sqlx::query_scalar::<_, i64>(
            r#"SELECT COUNT(*)::bigint FROM "FlagImportOperations" WHERE state = 0"#,
        )
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if pending >= MAX_PENDING_FLAG_IMPORTS {
            return Err(AppError::too_many_requests(1));
        }
        if !claim_flag_import_slot(&mut transaction, lease_token).await? {
            return Err(AppError::too_many_requests(1));
        }
        sqlx::query(
            r#"INSERT INTO "FlagImportOperations"
                 (challenge_id, operation_id, actor_user_id, request_digest, lease_token)
               VALUES ($1, $2, $3, $4, $5)"#,
        )
        .bind(challenge_id)
        .bind(operation_id)
        .bind(actor_user_id)
        .bind(request_digest)
        .bind(lease_token)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(FlagImportReservation::Acquired(lease_token))
}

async fn renew_flag_import(
    pool: &sqlx::PgPool,
    challenge_id: i32,
    operation_id: Uuid,
    lease_token: Uuid,
) -> AppResult<bool> {
    let renewed = sqlx::query_scalar::<_, i64>(
        r#"WITH slot AS (
               UPDATE "FlagImportSlots"
                  SET expires_at_utc = clock_timestamp() + INTERVAL '5 minutes'
                WHERE lease_token = $3
              RETURNING 1
           ), operation AS (
               UPDATE "FlagImportOperations"
                  SET lease_expires_at_utc = clock_timestamp() + INTERVAL '5 minutes'
                WHERE challenge_id = $1 AND operation_id = $2 AND state = 0
                  AND lease_token = $3 AND EXISTS (SELECT 1 FROM slot)
              RETURNING 1
           ) SELECT COUNT(*)::bigint FROM operation"#,
    )
    .bind(challenge_id)
    .bind(operation_id)
    .bind(lease_token)
    .fetch_one(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(renewed == 1)
}

struct PreparedFlag {
    flag: String,
    file_type: FileType,
    remote_url: Option<String>,
    upload_stage: Option<crate::services::blob_refs::StagedBlob>,
}

pub(super) struct FlagRemoval {
    pub(super) revoked_hash: Option<String>,
    pub(super) deleted_hash: Option<String>,
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

async fn ensure_flag_import_capacity(
    pool: &sqlx::PgPool,
    challenge_id: i32,
    requested_values: &[String],
) -> AppResult<()> {
    let (current_count, missing_count) = sqlx::query_as::<_, (i64, i64)>(
        r#"WITH desired(flag) AS (
               SELECT UNNEST($2::text[])
           )
           SELECT
               (SELECT COUNT(*)::bigint FROM "FlagContexts"
                 WHERE challenge_id = $1 AND is_occupied = FALSE),
               (SELECT COUNT(*)::bigint FROM desired
                 WHERE NOT EXISTS (
                     SELECT 1 FROM "FlagContexts" existing
                      WHERE existing.challenge_id = $1
                        AND existing.flag = desired.flag
                 ))"#,
    )
    .bind(challenge_id)
    .bind(requested_values)
    .fetch_one(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if current_count > MAX_FLAGS_PER_CHALLENGE
        || current_count.saturating_add(missing_count) > MAX_FLAGS_PER_CHALLENGE
    {
        return Err(AppError::payload_too_large(format!(
            "A challenge may contain at most {MAX_FLAGS_PER_CHALLENGE} flags"
        )));
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

/// Reject a false -> true transition when legacy authored rows cannot be
/// submitted through the canonical player envelope. Runtime-owned rows never
/// satisfy static authoring policy and must not make an empty challenge appear
/// playable.
pub(super) async fn ensure_static_flag_can_enable_locked(
    connection: &mut sqlx::PgConnection,
    challenge_id: i32,
) -> AppResult<()> {
    let (valid_count, invalid_count) = sqlx::query_as::<_, (i64, i64)>(
        r#"SELECT
               COUNT(*) FILTER (
                   WHERE OCTET_LENGTH(flag) BETWEEN 1 AND $2
                     AND NOT rsctf_flag_has_boundary_whitespace(flag)
               )::BIGINT,
               COUNT(*) FILTER (
                   WHERE NOT (
                       OCTET_LENGTH(flag) BETWEEN 1 AND $2
                       AND NOT rsctf_flag_has_boundary_whitespace(flag)
                   )
               )::BIGINT
             FROM "FlagContexts"
            WHERE challenge_id = $1 AND is_occupied = FALSE"#,
    )
    .bind(challenge_id)
    .bind(
        i32::try_from(crate::utils::flag_policy::NORMAL_FLAG_MAX_BYTES)
            .expect("normal flag bound fits i32"),
    )
    .fetch_one(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if invalid_count != 0 {
        return Err(AppError::bad_request(
            "Cannot enable a challenge with a non-canonical flag",
        ));
    }
    if valid_count == 0 {
        return Err(AppError::bad_request(
            "Cannot enable a challenge that has no flag",
        ));
    }
    Ok(())
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
    // Canonical validation is side-effect free, so do it before reserving the
    // durable operation. Invalid input must not leave a lease that blocks a
    // corrected retry using the same client-owned operation identity.
    validate_authored_flags(&request.flags)?;
    let request_digest = Sha256::digest(
        serde_json::to_vec(&request.flags)
            .map_err(|error| AppError::internal(error.to_string()))?,
    )
    .to_vec();
    let lease_token = match reserve_flag_import(
        st.pg(),
        c_id,
        user.id,
        request.operation_id,
        &request_digest,
    )
    .await?
    {
        FlagImportReservation::Acquired(lease_token) => lease_token,
        FlagImportReservation::Replayed(result) => return Ok(RequestResponse::ok(result)),
    };

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

    // Reject an already-full or necessarily-overflowing import before any
    // attachment/blob work. The final count under the definition lock below
    // remains authoritative for races with another authoring request.
    let requested_values = models
        .iter()
        .map(|model| model.flag.clone())
        .collect::<Vec<_>>();
    if let Err(error) = ensure_flag_import_capacity(st.pg(), c_id, &requested_values).await {
        abandon_flag_import(st.pg(), c_id, request.operation_id, lease_token).await;
        return Err(error);
    }

    // Storage already completed under durable upload stages. Resolve their
    // immutable metadata before taking policy locks; the logical references,
    // attachment rows, and flags publish together below.
    let mut flags = Vec::with_capacity(models.len());
    for m in models {
        match renew_flag_import(st.pg(), c_id, request.operation_id, lease_token).await {
            Ok(true) => {}
            Ok(false) => {
                return Err(AppError::conflict(
                    "This flag import was reclaimed by another request",
                ));
            }
            Err(error) => return Err(error),
        }
        match prepare_flag(&st, user.id, m).await {
            Ok(prepared) => flags.push(prepared),
            Err(error) => {
                abandon_flag_import(st.pg(), c_id, request.operation_id, lease_token).await;
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
            abandon_flag_import(st.pg(), c_id, request.operation_id, lease_token).await;
            return Err(error);
        }
    };
    if let Err(error) = crate::utils::single_flight::acquire_transaction_advisory_lock(
        game_control.transaction_mut(),
        &crate::services::challenge_workloads::definition_lock_key(id, c_id),
    )
    .await
    {
        drop(game_control);
        abandon_flag_import(st.pg(), c_id, request.operation_id, lease_token).await;
        return Err(AppError::internal(error.to_string()));
    }
    let mutation: AppResult<FlagImportMutation> = async {
        // Deletion may have won after the intentionally lock-free attachment
        // staging. Recheck both durable fences in this retained transaction so
        // their key-share row locks survive until every flag insert commits.
        challenges::reject_pending_mutation(&mut **game_control.transaction_mut(), id, c_id)
            .await?;
        ensure_flag_policy_mutable_locked(game_control.transaction_mut(), id, c_id).await?;
        crate::utils::scoring::lock_jeopardy_flags_exclusive(game_control.transaction_mut(), c_id)
            .await?;

        // Attachment staging can outlive the five-minute recovery lease. Two
        // reclaimers may therefore arrive at this lock with the same identity;
        // serialize on the durable row and recover the winner's exact result
        // before attempting any grading-row mutation.
        let operation = sqlx::query_as::<_, (i16, Option<i32>, Option<i32>, Uuid)>(
            r#"SELECT state, inserted_count, duplicate_count, lease_token
                 FROM "FlagImportOperations"
                WHERE challenge_id = $1 AND operation_id = $2
                FOR UPDATE"#,
        )
        .bind(c_id)
        .bind(request.operation_id)
        .fetch_one(&mut **game_control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if operation.0 == 1 {
            return Ok(FlagImportMutation::Replayed(FlagImportResult {
                inserted: operation.1.unwrap_or_default(),
                duplicates: operation.2.unwrap_or_default(),
            }));
        }
        if operation.3 != lease_token {
            return Err(AppError::conflict(
                "This flag import was reclaimed by another request",
            ));
        }

        let current_count = sqlx::query_scalar::<_, i64>(
            r#"SELECT COUNT(*)::bigint FROM "FlagContexts"
                WHERE challenge_id = $1 AND is_occupied = FALSE"#,
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
        let missing_count = sqlx::query_scalar::<_, i64>(
            r#"SELECT COUNT(*)::bigint
                 FROM UNNEST($2::text[]) AS desired(flag)
                WHERE NOT EXISTS (
                    SELECT 1 FROM "FlagContexts" existing
                     WHERE existing.challenge_id = $1
                       AND existing.flag = desired.flag
                )"#,
        )
        .bind(c_id)
        .bind(&values)
        .fetch_one(&mut **game_control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if current_count.saturating_add(missing_count) > MAX_FLAGS_PER_CHALLENGE {
            return Err(AppError::payload_too_large(format!(
                "A challenge may contain at most {MAX_FLAGS_PER_CHALLENGE} flags"
            )));
        }

        let mut inserted_set = std::collections::HashSet::with_capacity(flags.len());
        for prepared in &flags {
            // Reserve the canonical flag identity before publishing its staged
            // attachment. A duplicate therefore cannot create an unreferenced
            // attachment row or consume the caller's durable upload stage.
            let flag_id = sqlx::query_scalar::<_, i32>(
                r#"INSERT INTO "FlagContexts"
                       (flag, is_occupied, challenge_id, attachment_id)
                   SELECT $1, FALSE, $2, NULL
                    WHERE NOT EXISTS (
                        SELECT 1 FROM "FlagContexts" existing
                         WHERE existing.challenge_id = $2
                           AND existing.flag = $1
                    )
                   ON CONFLICT (challenge_id, flag)
                   WHERE challenge_id IS NOT NULL AND canonical_identity_enforced
                   DO NOTHING
                   RETURNING id"#,
            )
            .bind(&prepared.flag)
            .bind(c_id)
            .fetch_optional(&mut **game_control.transaction_mut())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
            let Some(flag_id) = flag_id else {
                continue;
            };
            let attachment_id =
                insert_flag_attachment_locked(game_control.transaction_mut(), prepared).await?;
            if let Some(attachment_id) = attachment_id {
                sqlx::query(r#"UPDATE "FlagContexts" SET attachment_id = $2 WHERE id = $1"#)
                    .bind(flag_id)
                    .bind(attachment_id)
                    .execute(&mut **game_control.transaction_mut())
                    .await
                    .map_err(|error| AppError::internal(error.to_string()))?;
            }
            inserted_set.insert(prepared.flag.clone());
        }
        let inserted_count = inserted_set.len() as i32;
        let duplicate_count = body_duplicates + flags.len() as i32 - inserted_count;
        let completion = sqlx::query_scalar::<_, i64>(
            r#"WITH completed AS (
                   UPDATE "FlagImportOperations" operation
                      SET state = 1, inserted_count = $3, duplicate_count = $4,
                          completed_at_utc = clock_timestamp()
                    WHERE challenge_id = $1 AND operation_id = $2 AND state = 0
                      AND lease_token = $5
                      AND EXISTS (
                          SELECT 1 FROM "FlagImportSlots" slot
                           WHERE slot.lease_token = $5
                      )
                  RETURNING 1
               ), released AS (
                   UPDATE "FlagImportSlots"
                      SET lease_token = NULL, expires_at_utc = NULL
                    WHERE lease_token = $5 AND EXISTS (SELECT 1 FROM completed)
                  RETURNING 1
               ) SELECT COUNT(*)::bigint FROM completed"#,
        )
        .bind(c_id)
        .bind(request.operation_id)
        .bind(inserted_count)
        .bind(duplicate_count)
        .bind(lease_token)
        .fetch_one(&mut **game_control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if completion != 1 {
            return Err(AppError::conflict(
                "The flag import lease changed while it was being committed",
            ));
        }
        sqlx::query(
            r#"WITH expired AS (
                   SELECT challenge_id, operation_id
                     FROM "FlagImportOperations"
                    WHERE state IN (1, 2)
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
        Ok(FlagImportMutation::Applied {
            duplicates: duplicate_count,
            inserted: inserted_set,
        })
    }
    .await;

    let mutation = match mutation {
        Ok(result) => result,
        Err(error) => {
            drop(game_control);
            abandon_flag_import(st.pg(), c_id, request.operation_id, lease_token).await;
            return Err(error);
        }
    };
    if let Err(error) = game_control.release().await {
        // Commit acknowledgement is ambiguous. Keep staged rows intact so a
        // committed flag never loses its hand-out; the durable operation and
        // ordinary orphan reconciler make a retry/recovery safe.
        return Err(AppError::internal(error.to_string()));
    }
    for hash in flags.iter().filter_map(|prepared| {
        inserted_set_contains(&mutation, &prepared.flag)
            .then_some(prepared.upload_stage.as_ref())
            .flatten()
            .map(|stage| stage.blob.hash.as_str())
    }) {
        crate::controllers::assets::invalidate_asset_gate(&st, hash).await;
    }
    let (duplicates, inserted_set) = match mutation {
        FlagImportMutation::Replayed(result) => {
            return Ok(RequestResponse::ok(result));
        }
        FlagImportMutation::Applied {
            duplicates,
            inserted,
        } => (duplicates, inserted),
    };
    Ok(RequestResponse::ok(FlagImportResult {
        inserted: inserted_set.len() as i32,
        duplicates,
    }))
}

fn inserted_set_contains(mutation: &FlagImportMutation, flag: &str) -> bool {
    matches!(
        mutation,
        FlagImportMutation::Applied { inserted, .. } if inserted.contains(flag)
    )
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
            WHERE context.challenge_id = $1 AND context.is_occupied = FALSE
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
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use std::str::FromStr;

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
    #[ignore = "requires disposable PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn capacity_preflight_allows_duplicates_but_rejects_new_attachment_work() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("flag_capacity_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            r#"CREATE TABLE "FlagContexts" (
                 id SERIAL PRIMARY KEY, challenge_id INTEGER NOT NULL,
                 flag TEXT NOT NULL, is_occupied BOOLEAN NOT NULL DEFAULT FALSE)"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let runtime_values = (0..600)
            .map(|index| format!("runtime{{{index}}}"))
            .collect::<Vec<_>>();
        sqlx::query(
            r#"INSERT INTO "FlagContexts" (challenge_id, flag, is_occupied)
               SELECT 1, input.value, TRUE
                 FROM UNNEST($1::text[]) AS input(value)"#,
        )
        .bind(&runtime_values)
        .execute(&pool)
        .await
        .unwrap();
        assert!(
            ensure_flag_import_capacity(&pool, 1, &["flag{first}".to_string()])
                .await
                .is_ok()
        );
        let values = (0..MAX_FLAGS_PER_CHALLENGE)
            .map(|index| format!("flag{{{index}}}"))
            .collect::<Vec<_>>();
        sqlx::query(
            r#"INSERT INTO "FlagContexts" (challenge_id, flag)
               SELECT 1, input.value FROM UNNEST($1::text[]) AS input(value)"#,
        )
        .bind(&values)
        .execute(&pool)
        .await
        .unwrap();

        assert!(ensure_flag_import_capacity(&pool, 1, &[values[0].clone()])
            .await
            .is_ok());
        let rejected = ensure_flag_import_capacity(&pool, 1, &["flag{new}".to_string()])
            .await
            .unwrap_err();
        assert_eq!(rejected.status(), axum::http::StatusCode::PAYLOAD_TOO_LARGE);

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}

#[cfg(test)]
#[path = "flags_import_tests.rs"]
mod import_tests;

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
    crate::utils::single_flight::acquire_transaction_advisory_lock(
        game_control.transaction_mut(),
        &crate::services::challenge_workloads::definition_lock_key(id, c_id),
    )
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let removal = match remove_flag_locked(game_control.transaction_mut(), id, c_id, f_id).await {
        Ok(removal) => removal,
        Err(error) => {
            drop(game_control);
            return Err(error);
        }
    };
    if let Err(error) = game_control.release().await {
        return Err(AppError::internal(error.to_string()));
    }
    let Some(removal) = removal else {
        return Ok(RequestResponse::ok("NotFound".to_string()));
    };
    if let Some(hash) = removal.revoked_hash.as_deref() {
        crate::controllers::assets::invalidate_asset_gate(&st, hash).await;
    }
    if let Some(hash) = removal.deleted_hash {
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
) -> AppResult<Option<FlagRemoval>> {
    challenges::reject_pending_mutation(&mut **transaction, game_id, challenge_id).await?;
    ensure_flag_policy_mutable_locked(transaction, game_id, challenge_id).await?;
    crate::utils::scoring::lock_jeopardy_flags_exclusive(transaction, challenge_id).await?;

    // Capture the hand-out attachment in the same statement that removes the
    // flag. The exclusive advisory lock makes this deletion linearizable with
    // every authoritative submit-side grade, including static flag inserts.
    let attachment_id: Option<Option<i32>> = sqlx::query_scalar(
        r#"DELETE FROM "FlagContexts"
            WHERE id = $1 AND challenge_id = $2 AND is_occupied = FALSE
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
    let (revoked_hash, deleted_hash) = match attachment_id {
        Some(attachment_id) => {
            let revoked_hash = sqlx::query_scalar::<_, String>(
                r#"SELECT file.hash
                     FROM "Attachments" attachment
                     JOIN "Files" file ON file.id = attachment.local_file_id
                    WHERE attachment.id = $1"#,
            )
            .bind(attachment_id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
            let deleted_hash =
                crate::services::blob_refs::delete_attachment_locked(transaction, attachment_id)
                    .await?;
            (revoked_hash, deleted_hash)
        }
        None => (None, None),
    };
    Ok(Some(FlagRemoval {
        revoked_hash,
        deleted_hash,
    }))
}

// ============================================================================
//  Notices
// ============================================================================
