//! Durable admission and lease primitives for slow operator mutations.
//!
//! Routes enqueue before scanning data or touching a runtime. A partial unique
//! index coalesces concurrent intent for one scope across replicas, while an
//! opaque operation id makes lost-response recovery deterministic.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

use crate::app_state::{SharedState, SharedState as StateHandle};
use crate::utils::error::{AppError, AppResult};

const MAX_ACTIVE_JOBS: i64 = 256;
const MAX_ACTIVE_RESETS_PER_GAME: i64 = 32;
const MAX_ACTIVE_RESETS_PER_PARTICIPATION: i64 = 2;
const MAX_SCOPE_BYTES: usize = 256;
const MAX_INPUT_BYTES: usize = 16 * 1024;
const MAX_RESULT_BYTES: usize = 64 * 1024;
const MAX_ERROR_BYTES: usize = 4 * 1024;
const TERMINAL_RETENTION_DAYS: i32 = 7;
const MAX_PURGE_BATCH: i64 = 256;
const ADMISSION_LOCK_KEY: i64 = 0x5253_4354_464A_4F42;

const PURGE_TERMINAL_SQL: &str = r#"WITH expired AS (
    SELECT id FROM "ControlPlaneJobs"
     WHERE status IN (2, 3, 4)
       AND finished_at_utc < clock_timestamp() - make_interval(days => $1)
     ORDER BY finished_at_utc, id
     FOR UPDATE SKIP LOCKED LIMIT $2
)
DELETE FROM "ControlPlaneJobs" job
 USING expired WHERE job.id = expired.id"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlJobKind {
    ChallengeBuild,
    BuildBatch,
    VariantGeneration,
    SecurityDerivation,
    WorkloadRollout,
    AdReconcile,
    AdReset,
}

impl ControlJobKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ChallengeBuild => "ChallengeBuild",
            Self::BuildBatch => "BuildBatch",
            Self::VariantGeneration => "VariantGeneration",
            Self::SecurityDerivation => "SecurityDerivation",
            Self::WorkloadRollout => "WorkloadRollout",
            Self::AdReconcile => "AdReconcile",
            Self::AdReset => "AdReset",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum ControlJobStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl ControlJobStatus {
    fn from_i16(value: i16) -> AppResult<Self> {
        match value {
            0 => Ok(Self::Queued),
            1 => Ok(Self::Running),
            2 => Ok(Self::Succeeded),
            3 => Ok(Self::Failed),
            4 => Ok(Self::Cancelled),
            _ => Err(AppError::internal("invalid stored control-job status")),
        }
    }
}

#[derive(Clone, Debug, FromRow)]
struct JobRow {
    id: Uuid,
    kind: String,
    scope_key: String,
    game_id: i32,
    challenge_id: Option<i32>,
    operation_id: Uuid,
    fingerprint: String,
    input: Value,
    input_revision: i32,
    status: i16,
    progress_current: i32,
    progress_total: i32,
    result: Option<Value>,
    error: Option<String>,
    lease_token: Option<Uuid>,
    created_at_utc: DateTime<Utc>,
    updated_at_utc: DateTime<Utc>,
    finished_at_utc: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlJobModel {
    pub id: Uuid,
    pub kind: String,
    pub scope_key: String,
    pub game_id: i32,
    pub challenge_id: Option<i32>,
    pub operation_id: Uuid,
    pub fingerprint: String,
    pub status: ControlJobStatus,
    pub progress_current: i32,
    pub progress_total: i32,
    pub requested_generation: i32,
    pub result: Option<Value>,
    pub error: Option<String>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub created_at_utc: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub updated_at_utc: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub finished_at_utc: Option<DateTime<Utc>>,
}

impl TryFrom<JobRow> for ControlJobModel {
    type Error = AppError;

    fn try_from(row: JobRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            kind: row.kind,
            scope_key: row.scope_key,
            game_id: row.game_id,
            challenge_id: row.challenge_id,
            operation_id: row.operation_id,
            fingerprint: row.fingerprint,
            status: ControlJobStatus::from_i16(row.status)?,
            progress_current: row.progress_current,
            progress_total: row.progress_total,
            requested_generation: row.input_revision,
            result: row.result,
            error: row.error,
            created_at_utc: row.created_at_utc,
            updated_at_utc: row.updated_at_utc,
            finished_at_utc: row.finished_at_utc,
        })
    }
}

#[derive(Clone, Debug)]
pub struct ClaimedControlJob {
    pub model: ControlJobModel,
    pub input: Value,
    pub lease_token: Uuid,
    pub input_revision: i32,
}

const JOB_COLUMNS: &str = r#"id, kind, scope_key, game_id, challenge_id,
 operation_id, fingerprint, input, input_revision, status, progress_current, progress_total,
 result, error, lease_token, created_at_utc, updated_at_utc, finished_at_utc"#;
const JOB_COLUMNS_QUALIFIED: &str = r#"job.id, job.kind, job.scope_key, job.game_id,
 job.challenge_id, job.operation_id, job.fingerprint, job.input, job.input_revision,
 job.status, job.progress_current, job.progress_total, job.result, job.error,
 job.lease_token, job.created_at_utc, job.updated_at_utc, job.finished_at_utc"#;

fn validate_enqueue(scope_key: &str, fingerprint: &str, input: &Value) -> AppResult<()> {
    if scope_key.is_empty() || scope_key.len() > MAX_SCOPE_BYTES {
        return Err(AppError::bad_request(
            "control-job scope must contain 1 to 256 bytes",
        ));
    }
    if fingerprint.len() != 64 || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(AppError::bad_request(
            "control-job fingerprint must be a SHA-256 hex value",
        ));
    }
    let input_len = serde_json::to_vec(input)
        .map_err(|error| AppError::bad_request(error.to_string()))?
        .len();
    if input_len > MAX_INPUT_BYTES {
        return Err(AppError::bad_request("control-job input is too large"));
    }
    Ok(())
}

fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(|error| error.code())
        .is_some_and(|code| code == "23505")
}

fn can_coalesce_active(
    kind: ControlJobKind,
    active_fingerprint: &str,
    requested_fingerprint: &str,
) -> bool {
    active_fingerprint == requested_fingerprint
        || matches!(kind, ControlJobKind::AdReconcile | ControlJobKind::AdReset)
}

/// Atomically return an exact retry, coalesce with the active scope, or admit a
/// new job. The short transaction-wide admission lock makes the deployment-wide
/// active bound exact without retaining a connection during external work.
pub async fn enqueue(
    pool: &sqlx::PgPool,
    kind: ControlJobKind,
    scope_key: &str,
    game_id: i32,
    challenge_id: Option<i32>,
    operation_id: Uuid,
    fingerprint: &str,
    input: Value,
) -> AppResult<ControlJobModel> {
    validate_enqueue(scope_key, fingerprint, &input)?;
    let mut transaction = pool.begin().await.map_err(database_error)?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(ADMISSION_LOCK_KEY)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;

    let exact_sql = format!(
        r#"SELECT {JOB_COLUMNS_QUALIFIED} FROM "ControlPlaneJobs" job
            JOIN "ControlPlaneJobOperations" operation ON operation.job_id = job.id
            WHERE operation.operation_id = $1"#
    );
    if let Some(row) = sqlx::query_as::<_, JobRow>(&exact_sql)
        .bind(operation_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
    {
        if row.kind != kind.as_str() || row.scope_key != scope_key || row.fingerprint != fingerprint
        {
            transaction.rollback().await.map_err(database_error)?;
            return Err(AppError::conflict(
                "Idempotency-Key was already used for a different operation",
            ));
        }
        transaction.commit().await.map_err(database_error)?;
        return row.try_into();
    }

    let active_sql = format!(
        r#"SELECT {JOB_COLUMNS} FROM "ControlPlaneJobs"
            WHERE kind = $1 AND scope_key = $2 AND status IN (0, 1)
            ORDER BY created_at_utc, id LIMIT 1"#
    );
    if let Some(row) = sqlx::query_as::<_, JobRow>(&active_sql)
        .bind(kind.as_str())
        .bind(scope_key)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
    {
        if !can_coalesce_active(kind, &row.fingerprint, fingerprint) {
            transaction.rollback().await.map_err(database_error)?;
            return Err(AppError::conflict(
                "A different revision is already active for this control-plane resource",
            ));
        }
        sqlx::query(
            r#"INSERT INTO "ControlPlaneJobOperations" (operation_id, job_id)
                VALUES ($1, $2)"#,
        )
        .bind(operation_id)
        .bind(row.id)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        return row.try_into();
    }

    let active: i64 =
        sqlx::query_scalar(r#"SELECT COUNT(*) FROM "ControlPlaneJobs" WHERE status IN (0, 1)"#)
            .fetch_one(&mut *transaction)
            .await
            .map_err(database_error)?;
    if active >= MAX_ACTIVE_JOBS {
        transaction.rollback().await.map_err(database_error)?;
        return Err(AppError::unavailable(
            "Control-plane job capacity is full; retry later",
        ));
    }
    if kind == ControlJobKind::AdReset {
        let game_active: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*) FROM "ControlPlaneJobs"
                WHERE kind = 'AdReset' AND game_id = $1 AND status IN (0, 1)"#,
        )
        .bind(game_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
        if game_active >= MAX_ACTIVE_RESETS_PER_GAME {
            transaction.rollback().await.map_err(database_error)?;
            return Err(AppError::unavailable(
                "Event reset capacity is full; retry later",
            ));
        }
        if let Some(participation_id) = input
            .get("participationId")
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok())
        {
            let participation_active: i64 = sqlx::query_scalar(
                r#"SELECT COUNT(*) FROM "ControlPlaneJobs"
                    WHERE kind = 'AdReset' AND game_id = $1
                      AND status IN (0, 1)
                      AND (input->>'participationId')::integer = $2"#,
            )
            .bind(game_id)
            .bind(participation_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(database_error)?;
            if participation_active >= MAX_ACTIVE_RESETS_PER_PARTICIPATION {
                transaction.rollback().await.map_err(database_error)?;
                return Err(AppError::unavailable(
                    "Team reset capacity is full; retry later",
                ));
            }
        }
    }

    let id = Uuid::new_v4();
    let insert_sql = format!(
        r#"INSERT INTO "ControlPlaneJobs"
              (id, kind, scope_key, game_id, challenge_id, operation_id,
               fingerprint, input)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            RETURNING {JOB_COLUMNS}"#
    );
    let row = match sqlx::query_as::<_, JobRow>(&insert_sql)
        .bind(id)
        .bind(kind.as_str())
        .bind(scope_key)
        .bind(game_id)
        .bind(challenge_id)
        .bind(operation_id)
        .bind(fingerprint)
        .bind(input)
        .fetch_one(&mut *transaction)
        .await
    {
        Ok(row) => row,
        Err(error) if is_unique_violation(&error) => {
            transaction.rollback().await.map_err(database_error)?;
            return get_active(pool, kind, scope_key)
                .await?
                .ok_or_else(|| AppError::conflict("control-job admission raced; retry"));
        }
        Err(error) => return Err(database_error(error)),
    };
    sqlx::query(
        r#"INSERT INTO "ControlPlaneJobOperations" (operation_id, job_id)
            VALUES ($1, $2)"#,
    )
    .bind(operation_id)
    .bind(row.id)
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?;
    transaction.commit().await.map_err(database_error)?;
    row.try_into()
}

pub async fn get(pool: &sqlx::PgPool, id: Uuid) -> AppResult<Option<ControlJobModel>> {
    let sql = format!(r#"SELECT {JOB_COLUMNS} FROM "ControlPlaneJobs" WHERE id = $1"#);
    sqlx::query_as::<_, JobRow>(&sql)
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(database_error)?
        .map(TryInto::try_into)
        .transpose()
}

pub async fn get_by_operation(
    pool: &sqlx::PgPool,
    operation_id: Uuid,
) -> AppResult<Option<ControlJobModel>> {
    let sql = format!(
        r#"SELECT {JOB_COLUMNS_QUALIFIED} FROM "ControlPlaneJobs" job
            JOIN "ControlPlaneJobOperations" operation ON operation.job_id = job.id
            WHERE operation.operation_id = $1 LIMIT 1"#
    );
    sqlx::query_as::<_, JobRow>(&sql)
        .bind(operation_id)
        .fetch_optional(pool)
        .await
        .map_err(database_error)?
        .map(TryInto::try_into)
        .transpose()
}

pub async fn get_ad_reset_for_participation(
    pool: &sqlx::PgPool,
    game_id: i32,
    participation_id: i32,
    id: Option<Uuid>,
    operation_id: Option<Uuid>,
) -> AppResult<Option<ControlJobModel>> {
    let sql = format!(
        r#"SELECT {JOB_COLUMNS_QUALIFIED} FROM "ControlPlaneJobs" job
            WHERE job.kind = 'AdReset' AND job.game_id = $1
              AND (job.input->>'participationId')::integer = $2
              AND ($3::uuid IS NULL OR job.id = $3)
              AND ($4::uuid IS NULL OR EXISTS (
                  SELECT 1 FROM "ControlPlaneJobOperations" operation
                   WHERE operation.job_id = job.id
                     AND operation.operation_id = $4
              ))
            ORDER BY job.created_at_utc DESC, job.id DESC LIMIT 1"#
    );
    sqlx::query_as::<_, JobRow>(&sql)
        .bind(game_id)
        .bind(participation_id)
        .bind(id)
        .bind(operation_id)
        .fetch_optional(pool)
        .await
        .map_err(database_error)?
        .map(TryInto::try_into)
        .transpose()
}

pub async fn try_acquire_resource(
    pool: &sqlx::PgPool,
    resource_key: &str,
    owner_job_id: Uuid,
    lease: Duration,
) -> AppResult<bool> {
    if resource_key.is_empty() || resource_key.len() > MAX_SCOPE_BYTES {
        return Err(AppError::bad_request("control resource key is invalid"));
    }
    let seconds = lease.as_secs().clamp(1, 15 * 60) as i64;
    let owner = sqlx::query_scalar::<_, Uuid>(
        r#"INSERT INTO "ControlPlaneResourceLeases"
              (resource_key, owner_job_id, lease_expires_at_utc)
            VALUES ($1, $2, clock_timestamp() + make_interval(secs => $3))
            ON CONFLICT (resource_key) DO UPDATE
              SET owner_job_id = EXCLUDED.owner_job_id,
                  lease_expires_at_utc = EXCLUDED.lease_expires_at_utc
            WHERE "ControlPlaneResourceLeases".owner_job_id = EXCLUDED.owner_job_id
               OR "ControlPlaneResourceLeases".lease_expires_at_utc <= clock_timestamp()
            RETURNING owner_job_id"#,
    )
    .bind(resource_key)
    .bind(owner_job_id)
    .bind(seconds)
    .fetch_optional(pool)
    .await
    .map_err(database_error)?;
    Ok(owner == Some(owner_job_id))
}

pub async fn release_resource(
    pool: &sqlx::PgPool,
    resource_key: &str,
    owner_job_id: Uuid,
) -> AppResult<()> {
    sqlx::query(
        r#"DELETE FROM "ControlPlaneResourceLeases"
            WHERE resource_key = $1 AND owner_job_id = $2"#,
    )
    .bind(resource_key)
    .bind(owner_job_id)
    .execute(pool)
    .await
    .map_err(database_error)?;
    Ok(())
}

pub async fn checkpoint_input(
    pool: &sqlx::PgPool,
    id: Uuid,
    lease_token: Uuid,
    patch: Value,
) -> AppResult<()> {
    if !patch.is_object()
        || serde_json::to_vec(&patch)
            .map_err(|error| AppError::internal(error.to_string()))?
            .len()
            > MAX_INPUT_BYTES
    {
        return Err(AppError::internal("control-job checkpoint is invalid"));
    }
    let affected = sqlx::query(
        r#"UPDATE "ControlPlaneJobs"
              SET input = input || $3, updated_at_utc = clock_timestamp()
            WHERE id = $1 AND status = 1 AND lease_token = $2"#,
    )
    .bind(id)
    .bind(lease_token)
    .bind(patch)
    .execute(pool)
    .await
    .map_err(database_error)?
    .rows_affected();
    if affected != 1 {
        return Err(AppError::conflict("Control job lost its execution lease"));
    }
    Ok(())
}

/// Merge stronger requirements into one active generation. Exact/superset
/// retries do not advance the revision; a running owner observes the revision
/// fence at completion and performs one subsequent effective pass.
pub async fn merge_reconcile_input(pool: &sqlx::PgPool, id: Uuid, input: Value) -> AppResult<()> {
    if serde_json::to_vec(&input)
        .map_err(|error| AppError::bad_request(error.to_string()))?
        .len()
        > MAX_INPUT_BYTES
    {
        return Err(AppError::bad_request("control-job input is too large"));
    }
    sqlx::query(
        r#"UPDATE "ControlPlaneJobs"
              SET input = input || jsonb_build_object(
                      'ensureVpn',
                      COALESCE((input->>'ensureVpn')::boolean, false)
                        OR COALESCE(($2->>'ensureVpn')::boolean, false),
                      'ensureKoth',
                      COALESCE((input->>'ensureKoth')::boolean, false)
                        OR COALESCE(($2->>'ensureKoth')::boolean, false)
                  ),
                  input_revision = input_revision + 1,
                  updated_at_utc = clock_timestamp()
            WHERE id = $1 AND status IN (0, 1)
              AND (
                  (COALESCE(($2->>'ensureVpn')::boolean, false)
                    AND NOT COALESCE((input->>'ensureVpn')::boolean, false))
                  OR
                  (COALESCE(($2->>'ensureKoth')::boolean, false)
                    AND NOT COALESCE((input->>'ensureKoth')::boolean, false))
              )
              AND input_revision < 1000000"#,
    )
    .bind(id)
    .bind(input)
    .execute(pool)
    .await
    .map_err(database_error)?;
    Ok(())
}

pub async fn wait_for_terminal(
    pool: &sqlx::PgPool,
    id: Uuid,
    timeout: Duration,
) -> AppResult<ControlJobModel> {
    let deadline = tokio::time::Instant::now() + timeout.min(Duration::from_secs(15 * 60));
    let mut delay = Duration::from_millis(200);
    loop {
        let job = get(pool, id)
            .await?
            .ok_or_else(|| AppError::not_found("Control job disappeared"))?;
        if matches!(
            job.status,
            ControlJobStatus::Succeeded | ControlJobStatus::Failed | ControlJobStatus::Cancelled
        ) {
            return Ok(job);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(AppError::unavailable(
                "Control job is still running; retry its status later",
            ));
        }
        tokio::time::sleep(delay).await;
        delay = delay.saturating_mul(2).min(Duration::from_secs(2));
    }
}

/// Retain enough terminal history for ordinary lost-response recovery while
/// bounding jobs, operation aliases, completed variant claims, and stale
/// resource leases through their cascading ownership graph.
pub async fn purge_terminal(pool: &sqlx::PgPool, limit: i64) -> AppResult<u64> {
    let removed = sqlx::query(PURGE_TERMINAL_SQL)
        .bind(TERMINAL_RETENTION_DAYS)
        .bind(limit.clamp(1, MAX_PURGE_BATCH))
        .execute(pool)
        .await
        .map_err(database_error)?
        .rows_affected();
    Ok(removed)
}

pub fn result_count(job: &ControlJobModel, key: &str) -> AppResult<usize> {
    if job.status != ControlJobStatus::Succeeded {
        return Err(AppError::unavailable(job.error.clone().unwrap_or_else(
            || format!("Control job ended as {:?}", job.status),
        )));
    }
    job.result
        .as_ref()
        .and_then(|result| result.get(key))
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| AppError::internal("control-job result is missing a bounded count"))
}

pub async fn request_security_derivation(
    state: &SharedState,
    game_id: i32,
    operation_id: Uuid,
) -> AppResult<ControlJobModel> {
    let input = serde_json::json!({ "gameId": game_id });
    let fingerprint = crate::utils::codec::sha256_str(
        &serde_json::to_string(&input).map_err(|error| AppError::internal(error.to_string()))?,
    );
    let job = enqueue(
        state.pg(),
        ControlJobKind::SecurityDerivation,
        &format!("game:{game_id}"),
        game_id,
        None,
        operation_id,
        &fingerprint,
        input,
    )
    .await?;
    kick(state.clone());
    Ok(job)
}

async fn get_active(
    pool: &sqlx::PgPool,
    kind: ControlJobKind,
    scope_key: &str,
) -> AppResult<Option<ControlJobModel>> {
    let sql = format!(
        r#"SELECT {JOB_COLUMNS} FROM "ControlPlaneJobs"
            WHERE kind = $1 AND scope_key = $2 AND status IN (0, 1)
            ORDER BY created_at_utc, id LIMIT 1"#
    );
    sqlx::query_as::<_, JobRow>(&sql)
        .bind(kind.as_str())
        .bind(scope_key)
        .fetch_optional(pool)
        .await
        .map_err(database_error)?
        .map(TryInto::try_into)
        .transpose()
}

pub async fn claim_next(
    pool: &sqlx::PgPool,
    kind: ControlJobKind,
    lease: Duration,
) -> AppResult<Option<ClaimedControlJob>> {
    let lease_token = Uuid::new_v4();
    let lease_seconds = i64::try_from(lease.as_secs().clamp(5, 900)).unwrap_or(900);
    let sql = format!(
        r#"WITH candidate AS (
               SELECT id FROM "ControlPlaneJobs"
                WHERE kind = $1
                  AND (status = 0 OR (
                       status = 1 AND lease_expires_at_utc <= clock_timestamp()
                  ))
                ORDER BY created_at_utc, id
                FOR UPDATE SKIP LOCKED LIMIT 1
           )
           UPDATE "ControlPlaneJobs" job
              SET status = 1, lease_token = $2,
                  lease_expires_at_utc = clock_timestamp() + make_interval(secs => $3),
                  updated_at_utc = clock_timestamp()
             FROM candidate WHERE job.id = candidate.id
         RETURNING {JOB_COLUMNS}"#
    );
    let row = sqlx::query_as::<_, JobRow>(&sql)
        .bind(kind.as_str())
        .bind(lease_token)
        .bind(lease_seconds as f64)
        .fetch_optional(pool)
        .await
        .map_err(database_error)?;
    row.map(|row| {
        let input = row.input.clone();
        let input_revision = row.input_revision;
        let lease_token = row
            .lease_token
            .ok_or_else(|| AppError::internal("claimed control job has no durable lease token"))?;
        Ok(ClaimedControlJob {
            model: row.try_into()?,
            input,
            lease_token,
            input_revision,
        })
    })
    .transpose()
}

pub async fn complete(
    pool: &sqlx::PgPool,
    id: Uuid,
    lease_token: Uuid,
    input_revision: i32,
    result: Value,
) -> AppResult<bool> {
    if serde_json::to_vec(&result)
        .map_err(|error| AppError::internal(error.to_string()))?
        .len()
        > MAX_RESULT_BYTES
    {
        return Err(AppError::internal("control-job result exceeds its bound"));
    }
    let affected = sqlx::query(
        r#"UPDATE "ControlPlaneJobs"
              SET status = 2, result = $3, error = NULL,
                  progress_current = progress_total,
                  lease_token = NULL, lease_expires_at_utc = NULL,
                  updated_at_utc = clock_timestamp(),
                  finished_at_utc = clock_timestamp()
            WHERE id = $1 AND status = 1 AND lease_token = $2
              AND input_revision = $4"#,
    )
    .bind(id)
    .bind(lease_token)
    .bind(result)
    .bind(input_revision)
    .execute(pool)
    .await
    .map_err(database_error)?
    .rows_affected();
    if affected == 1 {
        return Ok(true);
    }
    // A stronger trigger merged while this pass ran. Relinquish the lease and
    // leave the same durable job queued for exactly one newer generation.
    sqlx::query(
        r#"UPDATE "ControlPlaneJobs"
              SET status = 0, lease_token = NULL, lease_expires_at_utc = NULL,
                  updated_at_utc = clock_timestamp()
            WHERE id = $1 AND status = 1 AND lease_token = $2
              AND input_revision <> $3"#,
    )
    .bind(id)
    .bind(lease_token)
    .bind(input_revision)
    .execute(pool)
    .await
    .map_err(database_error)?;
    Ok(false)
}

pub async fn set_progress(
    pool: &sqlx::PgPool,
    id: Uuid,
    lease_token: Uuid,
    current: i32,
    total: i32,
) -> AppResult<bool> {
    if current < 0 || total < 1 || current > total || total > 1_000_000 {
        return Err(AppError::internal("control-job progress is out of bounds"));
    }
    let affected = sqlx::query(
        r#"UPDATE "ControlPlaneJobs"
              SET progress_current = $3, progress_total = $4,
                  updated_at_utc = clock_timestamp()
            WHERE id = $1 AND status = 1 AND lease_token = $2"#,
    )
    .bind(id)
    .bind(lease_token)
    .bind(current)
    .bind(total)
    .execute(pool)
    .await
    .map_err(database_error)?
    .rows_affected();
    Ok(affected == 1)
}

pub async fn fail(
    pool: &sqlx::PgPool,
    id: Uuid,
    lease_token: Uuid,
    input_revision: i32,
    error: &str,
) -> AppResult<bool> {
    let mut error = error.to_owned();
    while error.len() > MAX_ERROR_BYTES {
        error.pop();
    }
    let affected = sqlx::query(
        r#"UPDATE "ControlPlaneJobs"
              SET status = 3, error = $3, result = NULL,
                  lease_token = NULL, lease_expires_at_utc = NULL,
                  updated_at_utc = clock_timestamp(),
                  finished_at_utc = clock_timestamp()
            WHERE id = $1 AND status = 1 AND lease_token = $2
              AND input_revision = $4"#,
    )
    .bind(id)
    .bind(lease_token)
    .bind(error)
    .bind(input_revision)
    .execute(pool)
    .await
    .map_err(database_error)?
    .rows_affected();
    if affected == 1 {
        return Ok(true);
    }
    sqlx::query(
        r#"UPDATE "ControlPlaneJobs"
              SET status = 0, error = NULL, lease_token = NULL,
                  lease_expires_at_utc = NULL, updated_at_utc = clock_timestamp()
            WHERE id = $1 AND status = 1 AND lease_token = $2
              AND input_revision <> $3"#,
    )
    .bind(id)
    .bind(lease_token)
    .bind(input_revision)
    .execute(pool)
    .await
    .map_err(database_error)?;
    Ok(false)
}

static CONTROL_JOB_WORKERS: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(2)));
static NEXT_KIND: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Best-effort wake-up. PostgreSQL owns queued work and lease recovery; this
/// process-local gate only bounds how many external operations one replica may
/// run at once. Maintenance invokes this too, so a crash or lost wake-up is not
/// terminal.
pub fn kick(state: SharedState) {
    let Ok(permit) = CONTROL_JOB_WORKERS.clone().try_acquire_owned() else {
        return;
    };
    tokio::spawn(async move {
        let _permit = permit;
        if let Err(error) = drain_bounded(&state, 8).await {
            tracing::warn!(%error, "control-plane job drain failed");
        }
    });
}

pub async fn drain_bounded(state: &StateHandle, limit: usize) -> AppResult<usize> {
    const KINDS: [ControlJobKind; 7] = [
        ControlJobKind::AdReconcile,
        ControlJobKind::AdReset,
        ControlJobKind::ChallengeBuild,
        ControlJobKind::BuildBatch,
        ControlJobKind::WorkloadRollout,
        ControlJobKind::VariantGeneration,
        ControlJobKind::SecurityDerivation,
    ];
    let mut processed = 0;
    while processed < limit {
        let mut claimed = None;
        let first = NEXT_KIND.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % KINDS.len();
        for offset in 0..KINDS.len() {
            let kind = KINDS[(first + offset) % KINDS.len()];
            if let Some(job) = claim_next(state.pg(), kind, Duration::from_secs(15 * 60)).await? {
                claimed = Some(job);
                break;
            }
        }
        let Some(job) = claimed else {
            break;
        };
        let id = job.model.id;
        let lease_token = job.lease_token;
        if !set_progress(state.pg(), id, lease_token, 0, 1).await? {
            tracing::warn!(%id, "control-plane job lost its lease before execution");
            processed += 1;
            continue;
        }
        let execution =
            tokio::time::timeout(Duration::from_secs(14 * 60), execute_claimed(state, &job)).await;
        match execution {
            Ok(Ok(result)) => {
                let _ = complete(state.pg(), id, lease_token, job.input_revision, result).await?;
            }
            Ok(Err(error)) => {
                if !fail(
                    state.pg(),
                    id,
                    lease_token,
                    job.input_revision,
                    &error.to_string(),
                )
                .await?
                {
                    tracing::warn!(%id, "control-plane job failure lost its lease fence");
                }
            }
            Err(_) => {
                let _ = fail(
                    state.pg(),
                    id,
                    lease_token,
                    job.input_revision,
                    "Control-plane job exceeded its total deadline",
                )
                .await?;
            }
        }
        processed += 1;
    }
    Ok(processed)
}

fn input_bool(input: &Value, key: &str, default: bool) -> bool {
    input.get(key).and_then(Value::as_bool).unwrap_or(default)
}

async fn execute_claimed(state: &StateHandle, job: &ClaimedControlJob) -> AppResult<Value> {
    match job.model.kind.as_str() {
        "VariantGeneration" => {
            let generated = crate::services::event_security::generate_event_variants_for_job(
                state,
                job.model.game_id,
                job.model.id,
            )
            .await?;
            Ok(serde_json::json!({ "generated": generated }))
        }
        "SecurityDerivation" => {
            let inserted =
                crate::services::event_security::derive_context_findings(state, job.model.game_id)
                    .await?;
            Ok(serde_json::json!({ "inserted": inserted }))
        }
        "AdReconcile" => {
            let (launched, failures) = crate::controllers::edit::run_ad_reconcile_job(
                state,
                job.model.game_id,
                input_bool(&job.input, "ensureVpn", false),
                input_bool(&job.input, "ensureKoth", false),
            )
            .await?;
            Ok(serde_json::json!({ "launched": launched, "failures": failures }))
        }
        "ChallengeBuild" => {
            crate::controllers::edit::execute_challenge_build_job(state, &job.model, &job.input)
                .await
        }
        "BuildBatch" => crate::controllers::edit::execute_build_batch_job(state, &job.model).await,
        "WorkloadRollout" => {
            let result =
                crate::controllers::edit::execute_workload_rollout_job(state, &job.model).await?;
            serde_json::to_value(result)
                .map_err(|error| AppError::internal(format!("rollout result failed: {error}")))
        }
        "AdReset" => crate::services::ad::reset::execute_job(state, job).await,
        _ => Err(AppError::internal("unsupported claimed control-job kind")),
    }
}

#[cfg(test)]
#[path = "control_jobs/tests.rs"]
mod tests;
