//! Durable admission, supervision, and result recovery for challenge imports.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use aes_gcm::aead::consts::U12;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use axum::http::{header, HeaderValue};
use axum::response::IntoResponse;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{Postgres, Transaction};
use tokio::sync::Semaphore;
use uuid::Uuid;

use super::{
    import_from_dir, resolve_subpath, ChallengeImportResult, MessageResponse, RequestResponse,
};
use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::CurrentUser;
use crate::services::git_sync::ImportPolicy;
use crate::utils::codec::sha256_hex;
use crate::utils::error::{AppError, AppResult};

mod source;
use source::{encrypt_token, normalized_github_url, token_aad, token_key};
mod retention;
use retention::release_terminal_sources;
mod result;
use result::{bounded_error, bounded_result};
mod zip;
pub(super) use zip::enqueue_zip;

const SOURCE_ZIP: i16 = 0;
const SOURCE_GIT: i16 = 1;
const POLICY_TRUSTED: i16 = 0;
const POLICY_PENDING: i16 = 1;
const STATUS_QUEUED: i16 = 0;
const STATUS_RUNNING: i16 = 1;
const STATUS_SUCCEEDED: i16 = 2;
const STATUS_FAILED: i16 = 3;
const GLOBAL_ACTIVE_JOBS: i64 = 2;
const EVENT_ACTIVE_JOBS: i64 = 1;
const WORKSPACE_MIB: u32 = 64;
const LOCAL_WORKSPACE_MIB: usize = 128;
const TOTAL_JOB_DEADLINE: Duration = Duration::from_secs(15 * 60);
const WORKER_POLL_INTERVAL: Duration = Duration::from_millis(500);
const CLEANUP_INTERVAL: Duration = Duration::from_secs(60);
const ADMISSION_RETRY_SECONDS: u64 = 5;
const ADMISSION_LOCK: i64 = 0x5253_4354_4649_4d50;

fn local_workspace_budget() -> &'static Arc<Semaphore> {
    static BUDGET: LazyLock<Arc<Semaphore>> =
        LazyLock::new(|| Arc::new(Semaphore::new(LOCAL_WORKSPACE_MIB)));
    &BUDGET
}

#[derive(Debug, Clone, Copy, Serialize)]
pub enum ChallengeImportJobStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
}

impl TryFrom<i16> for ChallengeImportJobStatus {
    type Error = AppError;

    fn try_from(value: i16) -> Result<Self, Self::Error> {
        match value {
            STATUS_QUEUED => Ok(Self::Queued),
            STATUS_RUNNING => Ok(Self::Running),
            STATUS_SUCCEEDED => Ok(Self::Succeeded),
            STATUS_FAILED => Ok(Self::Failed),
            _ => Err(AppError::internal("invalid challenge import job status")),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeImportJobModel {
    pub job_id: Uuid,
    pub status: ChallengeImportJobStatus,
    pub result: Option<ChallengeImportResult>,
    pub error: Option<String>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub created_at: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub updated_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct JobProjection {
    job_id: Uuid,
    status: i16,
    result: Option<serde_json::Value>,
    error: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

pub(super) struct GitImportSource {
    pub repo_url: String,
    pub git_ref: Option<String>,
    pub subpath: Option<PathBuf>,
    pub token: String,
}

impl TryFrom<JobProjection> for ChallengeImportJobModel {
    type Error = AppError;

    fn try_from(row: JobProjection) -> Result<Self, Self::Error> {
        Ok(Self {
            job_id: row.job_id,
            status: row.status.try_into()?,
            result: row
                .result
                .map(serde_json::from_value)
                .transpose()
                .map_err(|error| AppError::internal(format!("decode import result: {error}")))?,
            error: row.error,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

async fn load_job_model(st: &SharedState, job_id: Uuid) -> AppResult<ChallengeImportJobModel> {
    let row = sqlx::query_as::<_, JobProjection>(
        r#"SELECT requested.id AS job_id,
                  COALESCE(owner.status, requested.status) AS status,
                  COALESCE(owner.result, requested.result) AS result,
                  COALESCE(owner.error, requested.error) AS error,
                  requested.created_at,
                  GREATEST(requested.updated_at, COALESCE(owner.updated_at, requested.updated_at)) AS updated_at
             FROM "ChallengeImportJobs" requested
             LEFT JOIN "ChallengeImportJobs" owner
               ON owner.id = requested.coalesced_job_id
            WHERE requested.id = $1"#,
    )
    .bind(job_id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Challenge import job not found"))?;
    row.try_into()
}

fn accepted(model: ChallengeImportJobModel) -> axum::response::Response {
    RequestResponse::with_status(model, 202).into_response()
}

pub(super) fn busy() -> axum::response::Response {
    let mut response =
        MessageResponse::new("Challenge import capacity is busy; retry shortly", 503)
            .into_response();
    response.headers_mut().insert(
        header::RETRY_AFTER,
        HeaderValue::from_str(&ADMISSION_RETRY_SECONDS.to_string())
            .expect("bounded retry seconds are a valid header"),
    );
    response
}

#[derive(sqlx::FromRow)]
struct ExistingOperation {
    id: Uuid,
    source_key: String,
}

async fn begin_admitted<'a>(
    st: &'a SharedState,
    game_id: i32,
    actor_user_id: Uuid,
    operation_id: Uuid,
    source_key: &str,
) -> AppResult<Result<Transaction<'a, Postgres>, Uuid>> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if let Some(existing) = sqlx::query_as::<_, ExistingOperation>(
        r#"SELECT id, source_key
             FROM "ChallengeImportJobs"
            WHERE game_id = $1 AND actor_user_id = $2 AND operation_id = $3"#,
    )
    .bind(game_id)
    .bind(actor_user_id)
    .bind(operation_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    {
        if existing.source_key != source_key {
            return Err(AppError::conflict(
                "operationId was already used for a different import source",
            ));
        }
        transaction
            .commit()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(Err(existing.id));
    }

    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
        .bind(ADMISSION_LOCK)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if !locked {
        return Err(AppError::unavailable(
            "challenge import admission is contended",
        ));
    }
    let (global, event): (i64, i64) = sqlx::query_as(
        r#"SELECT COUNT(*) FILTER (WHERE coalesced_job_id IS NULL)::bigint,
                  COUNT(*) FILTER (WHERE coalesced_job_id IS NULL AND game_id = $1)::bigint
             FROM "ChallengeImportJobs"
            WHERE status IN (0, 1)"#,
    )
    .bind(game_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if global >= GLOBAL_ACTIVE_JOBS || event >= EVENT_ACTIVE_JOBS {
        transaction
            .rollback()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Err(AppError::unavailable("challenge import capacity is busy"));
    }
    Ok(Ok(transaction))
}

fn pending_submitter(policy: ImportPolicy) -> Option<Uuid> {
    match policy {
        ImportPolicy::PendingReview {
            submitted_by_user_id,
        } => Some(submitted_by_user_id),
        ImportPolicy::Trusted => None,
    }
}

fn policy_code(policy: ImportPolicy) -> i16 {
    if matches!(policy, ImportPolicy::Trusted) {
        POLICY_TRUSTED
    } else {
        POLICY_PENDING
    }
}

async fn coalesce_revision(
    transaction: &mut Transaction<'_, Postgres>,
    job_id: Uuid,
    game_id: i32,
    source_kind: i16,
    revision_key: &str,
) -> AppResult<bool> {
    let inserted = sqlx::query(
        r#"INSERT INTO "ChallengeImportRevisions"
              (game_id, source_kind, revision_key, owner_job_id)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (game_id, source_kind, revision_key) DO NOTHING"#,
    )
    .bind(game_id)
    .bind(source_kind)
    .bind(revision_key)
    .bind(job_id)
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    if inserted == 1 {
        return Ok(true);
    }

    let (owner_job_id, owner_status): (Uuid, i16) = sqlx::query_as(
        r#"SELECT revision.owner_job_id, owner.status
             FROM "ChallengeImportRevisions" revision
             JOIN "ChallengeImportJobs" owner ON owner.id = revision.owner_job_id
            WHERE revision.game_id = $1
              AND revision.source_kind = $2
              AND revision.revision_key = $3
            FOR UPDATE OF revision"#,
    )
    .bind(game_id)
    .bind(source_kind)
    .bind(revision_key)
    .fetch_one(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if owner_job_id == job_id {
        return Ok(true);
    }
    if owner_status == STATUS_FAILED {
        sqlx::query(
            r#"UPDATE "ChallengeImportRevisions"
                  SET owner_job_id = $4, created_at = clock_timestamp()
                WHERE game_id = $1 AND source_kind = $2 AND revision_key = $3"#,
        )
        .bind(game_id)
        .bind(source_kind)
        .bind(revision_key)
        .bind(job_id)
        .execute(&mut **transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(true);
    }
    sqlx::query(
        r#"UPDATE "ChallengeImportJobs" requested
              SET coalesced_job_id = owner.id,
                  status = owner.status,
                  result = owner.result,
                  error = owner.error,
                  lease_owner = NULL,
                  lease_expires_at = NULL,
                  token_ciphertext = NULL,
                  token_nonce = NULL,
                  updated_at = clock_timestamp()
             FROM "ChallengeImportJobs" owner
            WHERE requested.id = $1 AND owner.id = $2"#,
    )
    .bind(job_id)
    .bind(owner_job_id)
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(false)
}

pub(super) async fn enqueue_git(
    st: &SharedState,
    game_id: i32,
    actor_user_id: Uuid,
    operation_id: Uuid,
    source: GitImportSource,
) -> AppResult<axum::response::Response> {
    let GitImportSource {
        repo_url,
        git_ref,
        subpath,
        token,
    } = source;
    let repo_url = normalized_github_url(&repo_url)?;
    let subpath = subpath.map(|path| path.to_string_lossy().replace('\\', "/"));
    if subpath
        .as_ref()
        .is_some_and(|subpath| subpath.len() > 1_024)
    {
        return Err(AppError::bad_request("subpath may be at most 1024 bytes"));
    }
    let source_key = sha256_hex(
        format!(
            "git\0{repo_url}\0{}\0{}",
            git_ref.as_deref().unwrap_or("HEAD"),
            subpath.as_deref().unwrap_or("")
        )
        .as_bytes(),
    );
    let admitted = begin_admitted(st, game_id, actor_user_id, operation_id, &source_key).await;
    let mut transaction = match admitted {
        Ok(Ok(transaction)) => transaction,
        Ok(Err(job_id)) => return Ok(accepted(load_job_model(st, job_id).await?)),
        Err(AppError::ServiceUnavailable(_)) => return Ok(busy()),
        Err(error) => return Err(error),
    };
    let job_id = Uuid::new_v4();
    let (token_ciphertext, token_nonce) = encrypt_token(
        &st.config.jwt_secret,
        job_id,
        game_id,
        actor_user_id,
        token.trim(),
    )?;
    sqlx::query(
        r#"INSERT INTO "ChallengeImportJobs"
              (id, game_id, actor_user_id, operation_id, source_kind, import_policy,
               source_key, repo_url, git_ref, subpath, token_ciphertext, token_nonce)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)"#,
    )
    .bind(job_id)
    .bind(game_id)
    .bind(actor_user_id)
    .bind(operation_id)
    .bind(SOURCE_GIT)
    .bind(POLICY_TRUSTED)
    .bind(source_key)
    .bind(repo_url)
    .bind(git_ref)
    .bind(subpath)
    .bind(token_ciphertext)
    .bind(token_nonce)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(accepted(load_job_model(st, job_id).await?))
}

pub async fn get_job(
    axum::extract::State(st): axum::extract::State<SharedState>,
    user: CurrentUser,
    axum::extract::Path((game_id, job_id)): axum::extract::Path<(i32, Uuid)>,
) -> AppResult<axum::response::Response> {
    let actor = sqlx::query_scalar::<_, Uuid>(
        r#"SELECT actor_user_id FROM "ChallengeImportJobs"
            WHERE id = $1 AND game_id = $2"#,
    )
    .bind(job_id)
    .bind(game_id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Challenge import job not found"))?;
    if actor != user.id {
        super::super::manager_or_admin(&st, &user, game_id).await?;
    }
    let mut response = RequestResponse::ok(load_job_model(&st, job_id).await?).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

#[derive(sqlx::FromRow)]
struct ClaimedJob {
    id: Uuid,
    game_id: i32,
    actor_user_id: Uuid,
    source_kind: i16,
    import_policy: i16,
    source_key: String,
    source_hash: Option<String>,
    repo_url: Option<String>,
    git_ref: Option<String>,
    subpath: Option<String>,
    token_ciphertext: Option<Vec<u8>>,
    token_nonce: Option<Vec<u8>>,
}

async fn claim_job(st: &SharedState, worker_id: Uuid) -> AppResult<Option<ClaimedJob>> {
    let row = sqlx::query_as::<_, ClaimedJob>(
        r#"WITH candidate AS (
                SELECT job.id
                  FROM "ChallengeImportJobs" job
                 WHERE job.coalesced_job_id IS NULL
                   AND job.attempts < 8
                   AND (job.source_kind = 1 OR job.source_staged = TRUE)
                   AND (job.status = 0 OR (job.status = 1 AND job.lease_expires_at <= clock_timestamp()))
                 ORDER BY job.created_at, job.id
                 FOR UPDATE SKIP LOCKED
                 LIMIT 1
            ), claimed AS (
                UPDATE "ChallengeImportJobs" job
                   SET status = 1,
                       lease_owner = $1,
                       lease_expires_at = clock_timestamp() + INTERVAL '16 minutes',
                       attempts = attempts + 1,
                       updated_at = clock_timestamp()
                  FROM candidate
                 WHERE job.id = candidate.id
             RETURNING job.*
            )
            SELECT claimed.id, claimed.game_id, claimed.actor_user_id,
                   claimed.source_kind, claimed.import_policy, claimed.source_key,
                   file.hash AS source_hash,
                   claimed.repo_url, claimed.git_ref, claimed.subpath,
                   claimed.token_ciphertext, claimed.token_nonce
              FROM claimed
              LEFT JOIN "Files" file ON file.id = claimed.source_file_id"#,
    )
    .bind(worker_id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(row)
}

fn decrypt_token(st: &SharedState, job: &ClaimedJob) -> AppResult<String> {
    let Some(ciphertext) = job.token_ciphertext.as_deref() else {
        return Ok(String::new());
    };
    let nonce = job
        .token_nonce
        .as_deref()
        .filter(|nonce| nonce.len() == 12)
        .ok_or_else(|| AppError::internal("invalid import token nonce"))?;
    let cipher = Aes256Gcm::new_from_slice(&token_key(&st.config.jwt_secret))
        .map_err(|_| AppError::internal("initialize import token encryption"))?;
    let nonce = Nonce::<U12>::try_from(nonce)
        .map_err(|_| AppError::internal("invalid import token nonce"))?;
    let plaintext = cipher
        .decrypt(
            &nonce,
            Payload {
                msg: ciphertext,
                aad: &token_aad(job.id, job.game_id, job.actor_user_id),
            },
        )
        .map_err(|_| AppError::unavailable("import token cannot be decrypted"))?;
    String::from_utf8(plaintext).map_err(|_| AppError::internal("import token is not UTF-8"))
}

struct Workspace {
    path: Option<PathBuf>,
}

impl Workspace {
    fn create(job_id: Uuid) -> AppResult<Self> {
        let path = std::env::temp_dir().join(format!("rsctf-import-{job_id}"));
        if path.exists() {
            std::fs::remove_dir_all(&path).map_err(|error| {
                AppError::internal(format!("remove stale import workspace: {error}"))
            })?;
        }
        std::fs::create_dir(&path)
            .map_err(|error| AppError::internal(format!("create import workspace: {error}")))?;
        Ok(Self { path: Some(path) })
    }

    fn path(&self) -> &Path {
        self.path.as_deref().expect("workspace path remains owned")
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let Some(path) = self.path.take() else {
            return;
        };
        let cleanup_path = path.clone();
        if std::thread::Builder::new()
            .name("rsctf-import-cleanup".to_string())
            .spawn(move || {
                let _ = std::fs::remove_dir_all(cleanup_path);
            })
            .is_err()
        {
            if let Err(error) = std::fs::remove_dir_all(&path) {
                tracing::warn!(%error, path = %path.display(), "import workspace cleanup failed");
            }
        }
    }
}

fn import_policy(job: &ClaimedJob) -> AppResult<ImportPolicy> {
    match job.import_policy {
        POLICY_TRUSTED => Ok(ImportPolicy::Trusted),
        POLICY_PENDING => Ok(ImportPolicy::PendingReview {
            submitted_by_user_id: job.actor_user_id,
        }),
        _ => Err(AppError::internal("invalid challenge import policy")),
    }
}

async fn claim_git_revision(
    st: &SharedState,
    job: &ClaimedJob,
    revision_key: &str,
) -> AppResult<bool> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"UPDATE "ChallengeImportJobs"
              SET resolved_revision = $2, updated_at = clock_timestamp()
            WHERE id = $1"#,
    )
    .bind(job.id)
    .bind(revision_key)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let owned = coalesce_revision(
        &mut transaction,
        job.id,
        job.game_id,
        SOURCE_GIT,
        revision_key,
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(owned)
}

async fn execute_job(
    st: &SharedState,
    job: &ClaimedJob,
) -> AppResult<Option<ChallengeImportResult>> {
    let _budget = local_workspace_budget()
        .clone()
        .try_acquire_many_owned(WORKSPACE_MIB)
        .map_err(|_| AppError::unavailable("local import workspace budget is busy"))?;
    let policy = import_policy(job)?;
    match job.source_kind {
        SOURCE_ZIP => {
            let hash = job
                .source_hash
                .as_deref()
                .ok_or_else(|| AppError::internal("ZIP import source is unavailable"))?;
            let bytes = st
                .storage
                .load_bounded(hash, crate::utils::upload::ARCHIVE_FILE_BYTES)
                .await?;
            let job_id = job.id;
            let workspace = tokio::task::spawn_blocking(move || {
                let workspace = Workspace::create(job_id)?;
                super::archive::extract_zip(&bytes, workspace.path())?;
                Ok::<_, AppError>(workspace)
            })
            .await
            .map_err(|error| {
                AppError::internal(format!("ZIP extraction task failed: {error}"))
            })??;
            Ok(Some(
                import_from_dir(st, job.game_id, workspace.path(), policy, &job.source_key).await,
            ))
        }
        SOURCE_GIT => {
            let job_id = job.id;
            let workspace = tokio::task::spawn_blocking(move || Workspace::create(job_id))
                .await
                .map_err(|error| {
                    AppError::internal(format!("Git workspace task failed: {error}"))
                })??;
            let repo_url = job
                .repo_url
                .as_deref()
                .ok_or_else(|| AppError::internal("Git import URL is unavailable"))?;
            let auth_url = crate::services::git_sync::GitCredentials::new(decrypt_token(st, job)?)
                .apply(repo_url);
            crate::services::git_sync::sync_repo(
                &auth_url,
                job.git_ref.as_deref(),
                workspace.path(),
            )
            .await
            .map_err(|error| AppError::bad_request(format!("git clone failed: {error}")))?;
            let commit = crate::services::git_sync::head_sha(workspace.path()).await?;
            let revision_key = sha256_hex(
                format!(
                    "git\0{repo_url}\0{commit}\0{}",
                    job.subpath.as_deref().unwrap_or("")
                )
                .as_bytes(),
            );
            if !claim_git_revision(st, job, &revision_key).await? {
                return Ok(None);
            }
            let subpath = job.subpath.as_deref().map(Path::new);
            let root = resolve_subpath(workspace.path(), subpath)?;
            Ok(Some(
                import_from_dir(st, job.game_id, &root, policy, &revision_key).await,
            ))
        }
        _ => Err(AppError::internal("invalid challenge import source kind")),
    }
}

async fn finish_job(
    st: &SharedState,
    job_id: Uuid,
    outcome: AppResult<Option<ChallengeImportResult>>,
) -> AppResult<()> {
    let (status, result, error) = match outcome {
        Ok(Some(result)) => (
            STATUS_SUCCEEDED,
            Some(
                serde_json::to_value(bounded_result(result)).map_err(|error| {
                    AppError::internal(format!("encode import result: {error}"))
                })?,
            ),
            None,
        ),
        Ok(None) => return Ok(()),
        Err(error) => (STATUS_FAILED, None, Some(bounded_error(&error))),
    };
    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"UPDATE "ChallengeImportJobs"
              SET status = $2, result = $3, error = $4,
                  lease_owner = NULL, lease_expires_at = NULL,
                  token_ciphertext = NULL, token_nonce = NULL,
                  updated_at = clock_timestamp()
            WHERE id = $1"#,
    )
    .bind(job_id)
    .bind(status)
    .bind(&result)
    .bind(&error)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"UPDATE "ChallengeImportJobs"
              SET status = $2, result = $3, error = $4,
                  token_ciphertext = NULL, token_nonce = NULL,
                  updated_at = clock_timestamp()
            WHERE coalesced_job_id = $1"#,
    )
    .bind(job_id)
    .bind(status)
    .bind(result)
    .bind(error)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    // Terminal retry metadata does not need to retain the potentially large
    // ZIP payload. A periodic sweep repeats this after a crash in this seam.
    release_terminal_sources(st, Some(job_id)).await?;
    Ok(())
}

async fn worker_lane(st: SharedState, mut shutdown: tokio::sync::watch::Receiver<bool>) {
    let worker_id = Uuid::new_v4();
    loop {
        if *shutdown.borrow() {
            return;
        }
        match claim_job(&st, worker_id).await {
            Ok(Some(job)) => {
                let outcome = tokio::time::timeout(TOTAL_JOB_DEADLINE, execute_job(&st, &job))
                    .await
                    .unwrap_or_else(|_| Err(AppError::unavailable("challenge import timed out")));
                if let Err(error) = &outcome {
                    tracing::warn!(job_id = %job.id, %error, "challenge import execution failed");
                }
                if let Err(error) = finish_job(&st, job.id, outcome).await {
                    tracing::warn!(job_id = %job.id, %error, "challenge import completion failed");
                }
            }
            Ok(None) => {
                tokio::select! {
                    changed = shutdown.changed() => {
                        if changed.is_err() || *shutdown.borrow() {
                            return;
                        }
                    }
                    _ = tokio::time::sleep(WORKER_POLL_INTERVAL) => {}
                }
            }
            Err(error) => {
                tracing::warn!(%error, "challenge import claim failed");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

async fn sweep_expired_jobs(st: &SharedState) -> AppResult<usize> {
    sqlx::query(
        r#"UPDATE "ChallengeImportJobs"
              SET status = 3,
                  error = 'Challenge import source staging was interrupted.',
                  updated_at = clock_timestamp()
            WHERE source_kind = 0 AND source_staged = FALSE AND status = 0
              AND updated_at <= clock_timestamp() - INTERVAL '16 minutes'"#,
    )
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"UPDATE "ChallengeImportJobs"
              SET status = 3,
                  error = 'Challenge import expired before worker capacity became available.',
                  token_ciphertext = NULL,
                  token_nonce = NULL,
                  updated_at = clock_timestamp()
            WHERE status = 0 AND coalesced_job_id IS NULL
              AND expires_at <= clock_timestamp()"#,
    )
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"UPDATE "ChallengeImportJobs"
              SET status = 3,
                  error = 'Challenge import could not recover after repeated worker interruption.',
                  lease_owner = NULL,
                  lease_expires_at = NULL,
                  token_ciphertext = NULL,
                  token_nonce = NULL,
                  updated_at = clock_timestamp()
            WHERE status = 1 AND attempts >= 8
              AND lease_expires_at <= clock_timestamp()"#,
    )
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"UPDATE "ChallengeImportJobs" child
              SET status = owner.status,
                  result = owner.result,
                  error = owner.error,
                  token_ciphertext = NULL,
                  token_nonce = NULL,
                  updated_at = clock_timestamp()
             FROM "ChallengeImportJobs" owner
            WHERE child.coalesced_job_id = owner.id
              AND owner.status IN (2, 3)
              AND child.status IN (0, 1)"#,
    )
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    release_terminal_sources(st, None).await?;
    let rows = sqlx::query_as::<_, (Uuid, Option<String>)>(
        r#"SELECT job.id, file.hash
             FROM "ChallengeImportJobs" job
             LEFT JOIN "Files" file ON file.id = job.source_file_id
            WHERE job.status IN (2, 3) AND job.expires_at <= clock_timestamp()
            ORDER BY job.expires_at, job.id
            LIMIT 16"#,
    )
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    for (job_id, hash) in &rows {
        let deleted = sqlx::query(
            r#"DELETE FROM "ChallengeImportJobs"
                WHERE id = $1 AND status IN (2, 3) AND expires_at <= clock_timestamp()"#,
        )
        .bind(job_id)
        .execute(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .rows_affected();
        if deleted == 1 {
            if let Some(hash) = hash {
                crate::services::blob_refs::purge_if_unreferenced(
                    st.pg(),
                    st.storage.as_ref(),
                    hash,
                )
                .await?;
            }
        }
    }
    Ok(rows.len())
}

async fn sweep_abandoned_workspaces(st: &SharedState) -> AppResult<usize> {
    let mut entries = tokio::fs::read_dir(std::env::temp_dir())
        .await
        .map_err(|error| AppError::internal(format!("scan import workspaces: {error}")))?;
    let mut removed = 0usize;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| AppError::internal(format!("read import workspace: {error}")))?
    {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(raw_id) = name.strip_prefix("rsctf-import-") else {
            continue;
        };
        let Ok(job_id) = Uuid::parse_str(raw_id) else {
            continue;
        };
        let active: bool = sqlx::query_scalar(
            r#"SELECT EXISTS(
                    SELECT 1 FROM "ChallengeImportJobs"
                     WHERE id = $1 AND status = 1
                       AND lease_expires_at > clock_timestamp()
                )"#,
        )
        .bind(job_id)
        .fetch_one(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if !active {
            tokio::fs::remove_dir_all(entry.path())
                .await
                .map_err(|error| {
                    AppError::internal(format!("remove abandoned workspace: {error}"))
                })?;
            removed += 1;
        }
    }
    Ok(removed)
}

async fn cleanup_lane(st: SharedState, mut shutdown: tokio::sync::watch::Receiver<bool>) {
    let mut ticker = tokio::time::interval(CLEANUP_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
            _ = ticker.tick() => {
                if let Err(error) = sweep_abandoned_workspaces(&st).await {
                    tracing::warn!(%error, "challenge import workspace sweep failed");
                }
                if let Err(error) = sweep_expired_jobs(&st).await {
                    tracing::warn!(%error, "challenge import job expiry failed");
                }
            }
        }
    }
}

pub fn start_worker(
    st: SharedState,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Sweep before claiming so a restarted replica never creates new work
        // on top of abandoned extraction/clone output.
        if let Err(error) = sweep_abandoned_workspaces(&st).await {
            tracing::warn!(%error, "initial challenge import workspace sweep failed");
        }
        tokio::join!(
            worker_lane(st.clone(), shutdown.clone()),
            worker_lane(st.clone(), shutdown.clone()),
            cleanup_lane(st, shutdown),
        );
    })
}

#[cfg(test)]
mod tests;
