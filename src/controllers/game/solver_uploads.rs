//! Optional solver uploads on solved Jeopardy challenges. When an event enables
//! it, a team may upload the script or notes it used so organizers can verify
//! the solve. Every upload is an immutable version kept for review; the server
//! stores the bytes in PostgreSQL and never unpacks, parses, or executes them.

use sha2::{Digest, Sha256};

use super::ai_chats::{editable_until, jeopardy_challenge, private_json, solved};
use super::*;

pub(super) const DISABLED_MESSAGE: &str = "Solver uploads are not enabled for this event";
const NOT_JEOPARDY: &str = "Solver uploads apply only to Jeopardy challenges";
pub(super) const MAX_FILE_BYTES: usize = crate::utils::upload::SOLVER_FILE_BYTES;
pub(super) const MAX_VERSIONS: i32 = 10;
/// Upper bound on everything one team stores for one event.
pub(super) const MAX_TEAM_BYTES: i64 = 16 * 1024 * 1024;
const MAX_FILE_NAME_CHARS: usize = 128;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SolverUploadVersion {
    pub id: i64,
    pub version: i32,
    pub file_name: String,
    pub size_bytes: i32,
    pub sha256: String,
    pub uploaded_by: Option<String>,
    pub seconds_since_solve: Option<i64>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub uploaded_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SolverUploadState {
    pub editable: bool,
    pub solved: bool,
    #[serde(with = "crate::utils::datetime::millis")]
    pub editable_until: DateTime<Utc>,
    pub max_file_bytes: usize,
    pub max_versions: i32,
    pub team_bytes_used: i64,
    pub team_bytes_limit: i64,
    /// Newest first.
    pub versions: Vec<SolverUploadVersion>,
}

#[derive(sqlx::FromRow)]
pub(super) struct VersionRow {
    pub(super) id: i64,
    pub(super) version: i32,
    pub(super) file_name: String,
    pub(super) size_bytes: i32,
    pub(super) sha256: Vec<u8>,
    pub(super) uploaded_by: Option<String>,
    pub(super) seconds_since_solve: Option<i64>,
    pub(super) uploaded_at: DateTime<Utc>,
}

impl From<VersionRow> for SolverUploadVersion {
    fn from(row: VersionRow) -> Self {
        Self {
            id: row.id,
            version: row.version,
            file_name: row.file_name,
            size_bytes: row.size_bytes,
            sha256: hex::encode(row.sha256),
            uploaded_by: row.uploaded_by,
            seconds_since_solve: row.seconds_since_solve,
            uploaded_at: row.uploaded_at,
        }
    }
}

/// Keep only a display-safe basename: no directories, no control characters,
/// at most 128 characters. The name is metadata only and never touches disk.
pub(super) fn clean_file_name(raw: Option<&str>) -> String {
    let base = raw
        .unwrap_or_default()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default();
    let cleaned: String = base
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_FILE_NAME_CHARS)
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "solver".to_owned()
    } else {
        cleaned.to_owned()
    }
}

async fn team_bytes_used<'e, E>(executor: E, participation_id: i32) -> AppResult<i64>
where
    E: sqlx::PgExecutor<'e>,
{
    sqlx::query_scalar(
        r#"SELECT COALESCE(SUM(size_bytes), 0)::bigint FROM "SolverUploads"
            WHERE participation_id = $1"#,
    )
    .bind(participation_id)
    .fetch_one(executor)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

async fn load_state(
    st: &SharedState,
    game: &game::Model,
    participation_id: i32,
    challenge_id: i32,
) -> AppResult<SolverUploadState> {
    let solved = solved(st.pg(), participation_id, challenge_id).await?;
    let versions = sqlx::query_as::<_, VersionRow>(
        r#"SELECT upload.id, upload.version, upload.file_name, upload.size_bytes,
                  upload.sha256, account.user_name AS uploaded_by,
                  upload.seconds_since_solve, upload.uploaded_at
             FROM "SolverUploads" upload
             LEFT JOIN "AspNetUsers" account ON account.id = upload.uploaded_by
            WHERE upload.game_id = $1 AND upload.participation_id = $2
              AND upload.challenge_id = $3
            ORDER BY upload.version DESC"#,
    )
    .bind(game.id)
    .bind(participation_id)
    .bind(challenge_id)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let until = editable_until(game);
    let team_bytes_used = team_bytes_used(st.pg(), participation_id).await?;
    Ok(SolverUploadState {
        editable: solved
            && Utc::now() <= until
            && (versions.len() as i32) < MAX_VERSIONS
            && team_bytes_used < MAX_TEAM_BYTES,
        solved,
        editable_until: until,
        max_file_bytes: MAX_FILE_BYTES,
        max_versions: MAX_VERSIONS,
        team_bytes_used,
        team_bytes_limit: MAX_TEAM_BYTES,
        versions: versions.into_iter().map(Into::into).collect(),
    })
}

/// `GET /api/game/{id}/challenges/{challengeId}/solver-uploads` — the caller's
/// own team's solver versions for one challenge (metadata only).
pub async fn get_solver_uploads(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, challenge_id)): Path<(i32, i32)>,
) -> AppResult<Response> {
    let ctx = context_info(&st, &user, id, false).await?;
    if !ctx.game.solver_uploads_enabled {
        return Err(AppError::not_found(DISABLED_MESSAGE));
    }
    jeopardy_challenge(st.pg(), id, challenge_id, NOT_JEOPARDY).await?;
    Ok(private_json(
        load_state(&st, &ctx.game, ctx.participation.id, challenge_id).await?,
    ))
}

/// One validated upload, ready for the roster-fenced write.
pub(super) struct NewSolverUpload<'a> {
    pub(super) game_id: i32,
    pub(super) team_id: i32,
    pub(super) participation_id: i32,
    pub(super) challenge_id: i32,
    pub(super) user_id: Uuid,
    pub(super) security_stamp: &'a str,
    pub(super) operation_id: Uuid,
    pub(super) file_name: String,
    pub(super) content: &'a [u8],
    pub(super) remote_ip_hash: Option<Vec<u8>>,
}

/// The database supplies `uploaded_at` and the solve-relative delay; the client
/// never supplies a time.
const INSERT_SQL: &str = r#"
INSERT INTO "SolverUploads"
       (game_id, participation_id, challenge_id, version, file_name, size_bytes,
        sha256, content, uploaded_by, operation_id, solved_at, seconds_since_solve,
        remote_ip_hash)
SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
       solve.submit_time_utc,
       CASE WHEN solve.submit_time_utc IS NULL THEN NULL
            ELSE floor(EXTRACT(EPOCH FROM clock_timestamp() - solve.submit_time_utc))::bigint
       END,
       $11
  FROM (SELECT (SELECT submission.submit_time_utc
                  FROM "FirstSolves" first_solve
                  JOIN "Submissions" submission ON submission.id = first_solve.submission_id
                 WHERE first_solve.participation_id = $2 AND first_solve.challenge_id = $3
                   AND submission.game_id = $1) AS submit_time_utc) solve
ON CONFLICT (participation_id, operation_id) DO NOTHING
RETURNING id, version
"#;

/// Append one solver version and return its `(id, version)`. A retried
/// operation id returns the version it already created instead of storing a
/// second copy.
pub(super) async fn store_solver_upload(
    pool: &sqlx::PgPool,
    upload: NewSolverUpload<'_>,
) -> AppResult<(i64, i32)> {
    if upload.content.is_empty() {
        return Err(AppError::bad_request("File is empty"));
    }
    if upload.content.len() > MAX_FILE_BYTES {
        return Err(AppError::bad_request("File is too large"));
    }
    let size = upload.content.len() as i32;
    let sha256 = Sha256::digest(upload.content).to_vec();

    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    crate::utils::single_flight::acquire_transaction_advisory_lock_shared(
        &mut transaction,
        &crate::services::live_roster::lock_key(upload.team_id),
    )
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !crate::services::live_roster::participation_caller_is_live_on(
        &mut *transaction,
        upload.user_id,
        upload.security_stamp,
        upload.game_id,
        upload.team_id,
        upload.participation_id,
        true,
    )
    .await?
    {
        return Err(AppError::Forbidden);
    }
    jeopardy_challenge(
        &mut *transaction,
        upload.game_id,
        upload.challenge_id,
        NOT_JEOPARDY,
    )
    .await?;
    if !solved(
        &mut *transaction,
        upload.participation_id,
        upload.challenge_id,
    )
    .await?
    {
        return Err(AppError::bad_request(
            "Solve the challenge before uploading a solver",
        ));
    }
    // Teammates may upload concurrently; serialize this team's version and
    // quota accounting. The unique indexes stay the backstop.
    crate::utils::single_flight::acquire_transaction_advisory_lock(
        &mut transaction,
        &format!("solver-uploads:{}", upload.participation_id),
    )
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let replay: Option<(i64, i32, i32)> = sqlx::query_as(
        r#"SELECT id, challenge_id, version FROM "SolverUploads"
            WHERE participation_id = $1 AND operation_id = $2"#,
    )
    .bind(upload.participation_id)
    .bind(upload.operation_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if let Some((id, challenge_id, version)) = replay {
        if challenge_id != upload.challenge_id {
            return Err(AppError::conflict(
                "This operation id was already used for another challenge",
            ));
        }
        return Ok((id, version));
    }
    let latest: i32 = sqlx::query_scalar(
        r#"SELECT COALESCE(MAX(version), 0) FROM "SolverUploads"
            WHERE participation_id = $1 AND challenge_id = $2"#,
    )
    .bind(upload.participation_id)
    .bind(upload.challenge_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if latest >= MAX_VERSIONS {
        return Err(AppError::bad_request(format!(
            "At most {MAX_VERSIONS} solver versions can be uploaded per challenge"
        )));
    }
    if team_bytes_used(&mut *transaction, upload.participation_id).await? + i64::from(size)
        > MAX_TEAM_BYTES
    {
        return Err(AppError::bad_request(
            "Your team has reached its solver storage limit for this event",
        ));
    }
    let stored = sqlx::query_as::<_, (i64, i32)>(INSERT_SQL)
        .bind(upload.game_id)
        .bind(upload.participation_id)
        .bind(upload.challenge_id)
        .bind(latest + 1)
        .bind(&upload.file_name)
        .bind(size)
        .bind(&sha256)
        .bind(upload.content)
        .bind(upload.user_id)
        .bind(upload.operation_id)
        .bind(upload.remote_ip_hash.as_deref())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .ok_or_else(|| AppError::conflict("Solver upload changed; refresh and try again"))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(stored)
}

/// `POST /api/game/{id}/challenges/{challengeId}/solver-uploads` — append a
/// solver version (multipart field `file`, at most 1 MiB).
pub async fn submit_solver_upload(
    State(st): State<SharedState>,
    user: CurrentUser,
    headers: HeaderMap,
    Path((id, challenge_id)): Path<(i32, i32)>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    mut multipart: Multipart,
) -> AppResult<Response> {
    let operation_id = crate::utils::upload::required_operation_id(&headers)?;
    // Resolve the play context and policy before buffering the body.
    let ctx = context_info(&st, &user, id, false).await?;
    if !ctx.game.solver_uploads_enabled {
        return Err(AppError::not_found(DISABLED_MESSAGE));
    }
    if Utc::now() > editable_until(&ctx.game) {
        return Err(AppError::bad_request(
            "Solver uploads are closed for this event",
        ));
    }
    jeopardy_challenge(st.pg(), id, challenge_id, NOT_JEOPARDY).await?;
    let _upload_reservation =
        crate::utils::upload::reserve_buffered(crate::utils::upload::SOLVER_BODY_BYTES)?;

    let mut upload: Option<(String, axum::body::Bytes)> = None;
    let mut field_count = 0usize;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::bad_request(format!("multipart error: {e}")))?
    {
        field_count += 1;
        if field_count > crate::utils::upload::SINGLE_FILE_FIELD_COUNT {
            return Err(AppError::bad_request("Too many multipart fields"));
        }
        if field.name() == Some("file") {
            let file_name = clean_file_name(field.file_name());
            let bytes = field
                .bytes()
                .await
                .map_err(|e| AppError::bad_request(format!("could not read file: {e}")))?;
            upload = Some((file_name, bytes));
            break;
        }
    }
    let (file_name, bytes) = upload.ok_or_else(|| AppError::bad_request("No file provided"))?;
    let remote_ip_hash = crate::services::anti_cheat::client_ip(&headers, Some(peer.ip()))
        .and_then(|ip| {
            crate::services::anti_cheat::hash_ip_identity(st.config.as_ref(), &ip)
                .map(|identity| identity.exact)
        });
    let (upload_id, _) = store_solver_upload(
        st.pg(),
        NewSolverUpload {
            game_id: id,
            team_id: ctx.participation.team_id,
            participation_id: ctx.participation.id,
            challenge_id,
            user_id: user.id,
            security_stamp: &user.security_stamp,
            operation_id,
            file_name,
            content: &bytes,
            remote_ip_hash,
        },
    )
    .await?;
    // Scan the committed file for agent artifacts without delaying the reply.
    crate::services::agent_artifacts::spawn_solver_scan(st.clone(), upload_id);
    Ok(private_json(
        load_state(&st, &ctx.game, ctx.participation.id, challenge_id).await?,
    ))
}

#[cfg(test)]
#[path = "solver_uploads_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "solver_uploads_artifact_tests.rs"]
mod artifact_tests;
