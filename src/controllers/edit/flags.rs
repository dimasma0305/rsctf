//! edit: flag CRUD (see edit/mod.rs for the router + shared DTOs/helpers).
use super::*;
use sha2::{Digest, Sha256};

const MAX_FLAGS_PER_IMPORT: usize = 100;
const MAX_FLAGS_PER_CHALLENGE: i64 = 512;
const MAX_FLAG_BYTES: usize = crate::utils::flag_policy::NORMAL_FLAG_MAX_BYTES;
const MAX_FLAG_REMOTE_URL_BYTES: usize = 2_048;
const MAX_FLAG_FILE_HASH_BYTES: usize = 256;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlagImportResult {
    pub inserted: i32,
    pub duplicates: i32,
}

enum FlagImportMutation {
    Applied {
        duplicates: i32,
        inserted: std::collections::HashSet<String>,
    },
    Replayed(FlagImportResult),
}

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
        r#"DELETE FROM "FlagImportOperations"
            WHERE challenge_id = $1 AND operation_id = $2 AND state = 0
              AND lease_token = $3"#,
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
    let inserted = sqlx::query_scalar::<_, Uuid>(
        r#"INSERT INTO "FlagImportOperations"
             (challenge_id, operation_id, actor_user_id, request_digest, lease_token)
           VALUES ($1, $2, $3, $4, $5)
           ON CONFLICT (challenge_id, operation_id) DO NOTHING
           RETURNING lease_token"#,
    )
    .bind(challenge_id)
    .bind(operation_id)
    .bind(actor_user_id)
    .bind(request_digest)
    .bind(lease_token)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if inserted.is_some() {
        transaction
            .commit()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(FlagImportReservation::Acquired(lease_token));
    }
    let stored = sqlx::query_as::<_, (Uuid, Vec<u8>, i16, Option<i32>, Option<i32>, bool)>(
        r#"SELECT actor_user_id, request_digest, state, inserted_count,
                  duplicate_count, lease_expires_at_utc <= clock_timestamp()
             FROM "FlagImportOperations"
            WHERE challenge_id = $1 AND operation_id = $2"#,
    )
    .bind(challenge_id)
    .bind(operation_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
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
    if !stored.5 {
        return Err(AppError::conflict(
            "This flag import is still running; retry its operation ID later",
        ));
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
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(FlagImportReservation::Acquired(lease_token))
}

async fn cleanup_staged_flag_attachments(st: &SharedState, flags: &[(String, Option<i32>)]) {
    for attachment_id in flags.iter().filter_map(|(_, attachment_id)| *attachment_id) {
        if let Err(error) = delete_attachment(st, attachment_id).await {
            tracing::warn!(
                %error,
                attachment_id,
                "failed to clean an unpublished flag attachment"
            );
        }
    }
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
                 WHERE challenge_id = $1),
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

    // Attachment creation does not alter grading policy. Materialize it before
    // taking the flag-policy lock so submissions are not held up by blob lookup.
    let mut flags = Vec::with_capacity(models.len());
    for m in models {
        // Each flag can carry its own hand-out attachment (RSCTF AddFlags).
        let attachment_id =
            match build_attachment(&st, m.attachment_type, m.file_hash, m.remote_url).await {
                Ok(attachment_id) => attachment_id,
                Err(error) => {
                    cleanup_staged_flag_attachments(&st, &flags).await;
                    abandon_flag_import(st.pg(), c_id, request.operation_id, lease_token).await;
                    return Err(error);
                }
            };
        flags.push((m.flag, attachment_id));
    }

    // Global order is game-control -> challenge definition -> JFLG. The game
    // lock prevents an A&D/KotH first round from crossing the policy check;
    // JFLG provides the corresponding first-Jeopardy-solve fence.
    let game_control = match crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await {
        Ok(lock) => lock,
        Err(error) => {
            cleanup_staged_flag_attachments(&st, &flags).await;
            abandon_flag_import(st.pg(), c_id, request.operation_id, lease_token).await;
            return Err(error);
        }
    };
    let mut definition_lock = match crate::services::challenge_workloads::acquire_definition_lock(
        st.pg(),
        id,
        c_id,
    )
    .await
    {
        Ok(lock) => lock,
        Err(error) => {
            drop(game_control);
            cleanup_staged_flag_attachments(&st, &flags).await;
            abandon_flag_import(st.pg(), c_id, request.operation_id, lease_token).await;
            return Err(AppError::internal(error.to_string()));
        }
    };
    let mutation: AppResult<FlagImportMutation> = async {
        // Deletion may have won after the intentionally lock-free attachment
        // staging. Recheck both durable fences in this retained transaction so
        // their key-share row locks survive until every flag insert commits.
        challenges::reject_pending_mutation(&mut **definition_lock.transaction_mut(), id, c_id)
            .await?;
        ensure_flag_policy_mutable_locked(definition_lock.transaction_mut(), id, c_id).await?;
        crate::utils::scoring::lock_jeopardy_flags_exclusive(
            definition_lock.transaction_mut(),
            c_id,
        )
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
        .fetch_one(&mut **definition_lock.transaction_mut())
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
            r#"SELECT COUNT(*)::bigint FROM "FlagContexts" WHERE challenge_id = $1"#,
        )
        .bind(c_id)
        .fetch_one(&mut **definition_lock.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if current_count > MAX_FLAGS_PER_CHALLENGE {
            return Err(AppError::payload_too_large(
                "This challenge already exceeds the editable flag limit",
            ));
        }
        let values = flags
            .iter()
            .map(|(flag, _)| flag.clone())
            .collect::<Vec<_>>();
        let attachment_ids = flags
            .iter()
            .map(|(_, attachment_id)| *attachment_id)
            .collect::<Vec<_>>();
        let inserted = sqlx::query_scalar::<_, String>(
            r#"WITH desired AS MATERIALIZED (
                   SELECT input.flag, input.attachment_id
                     FROM UNNEST($2::text[], $3::integer[])
                          AS input(flag, attachment_id)
               ), inserted AS (
                   INSERT INTO "FlagContexts"
                       (flag, is_occupied, challenge_id, attachment_id)
                   SELECT desired.flag, FALSE, $1, desired.attachment_id
                     FROM desired
                   WHERE NOT EXISTS (
                        SELECT 1 FROM "FlagContexts" existing
                         WHERE existing.challenge_id = $1
                           AND existing.flag = desired.flag
                    )
                   ON CONFLICT (challenge_id, flag)
                   WHERE challenge_id IS NOT NULL AND canonical_identity_enforced
                   DO NOTHING
                   RETURNING flag
               ) SELECT flag FROM inserted"#,
        )
        .bind(c_id)
        .bind(&values)
        .bind(&attachment_ids)
        .fetch_all(&mut **definition_lock.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        let inserted_set = inserted
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        if current_count + inserted_set.len() as i64 > MAX_FLAGS_PER_CHALLENGE {
            return Err(AppError::payload_too_large(format!(
                "A challenge may contain at most {MAX_FLAGS_PER_CHALLENGE} flags"
            )));
        }
        let inserted_count = inserted_set.len() as i32;
        let duplicate_count = body_duplicates + flags.len() as i32 - inserted_count;
        let completion = sqlx::query(
            r#"UPDATE "FlagImportOperations"
                  SET state = 1, inserted_count = $3, duplicate_count = $4,
                      completed_at_utc = clock_timestamp()
                WHERE challenge_id = $1 AND operation_id = $2 AND state = 0
                  AND lease_token = $5"#,
        )
        .bind(c_id)
        .bind(request.operation_id)
        .bind(inserted_count)
        .bind(duplicate_count)
        .bind(lease_token)
        .execute(&mut **definition_lock.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if completion.rows_affected() != 1 {
            return Err(AppError::conflict(
                "The flag import lease changed while it was being committed",
            ));
        }
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
        .execute(&mut **definition_lock.transaction_mut())
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
            drop(definition_lock);
            drop(game_control);
            cleanup_staged_flag_attachments(&st, &flags).await;
            abandon_flag_import(st.pg(), c_id, request.operation_id, lease_token).await;
            return Err(error);
        }
    };
    if let Err(error) = definition_lock.release().await {
        drop(game_control);
        // Commit acknowledgement is ambiguous. Keep staged rows intact so a
        // committed flag never loses its hand-out; the durable operation and
        // ordinary orphan reconciler make a retry/recovery safe.
        return Err(AppError::internal(error.to_string()));
    }
    if let Err(error) = game_control.release().await {
        tracing::warn!(%error, id, c_id, "flag-policy game lock release failed after commit");
    }
    let (duplicates, inserted_set) = match mutation {
        FlagImportMutation::Replayed(result) => {
            cleanup_staged_flag_attachments(&st, &flags).await;
            return Ok(RequestResponse::ok(result));
        }
        FlagImportMutation::Applied {
            duplicates,
            inserted,
        } => (duplicates, inserted),
    };
    let duplicate_attachments = flags
        .iter()
        .filter(|(flag, _)| !inserted_set.contains(flag))
        .cloned()
        .collect::<Vec<_>>();
    cleanup_staged_flag_attachments(&st, &duplicate_attachments).await;
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
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use std::str::FromStr;

    fn model(flag: String) -> FlagCreateModel {
        FlagCreateModel {
            flag,
            attachment_type: None,
            file_hash: None,
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
                 id SERIAL PRIMARY KEY, challenge_id INTEGER NOT NULL, flag TEXT NOT NULL)"#,
        )
        .execute(&pool)
        .await
        .unwrap();
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

/// `DELETE /api/edit/games/{id}/challenges/{cId}/flags/{fId}` — returns a
/// `TaskStatus`. RSCTF serializes this enum as a **string**, so we emit the
/// string literal directly (the port's `TaskStatus` enum is int-repr).
pub async fn remove_flag(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, c_id, f_id)): Path<(i32, i32, i32)>,
) -> AppResult<RequestResponse<String>> {
    manager_or_admin(&st, &user, id).await?;
    let game_control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await?;
    let mut definition_lock =
        crate::services::challenge_workloads::acquire_definition_lock(st.pg(), id, c_id).await?;
    let removal = match remove_flag_locked(definition_lock.transaction_mut(), id, c_id, f_id).await
    {
        Ok(removal) => removal,
        Err(error) => {
            if let Err(rollback_error) = definition_lock.rollback().await {
                tracing::warn!(%rollback_error, f_id, "flag removal rollback failed");
            }
            drop(game_control);
            return Err(error);
        }
    };
    definition_lock
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if let Err(error) = game_control.release().await {
        tracing::warn!(%error, id, c_id, f_id, "flag-policy game lock release failed after commit");
    }
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
