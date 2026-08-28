//! Durable, idempotent hand-off from evidence ingestion to suspicion detectors.
//!
//! Producers enqueue in the same PostgreSQL transaction as the source row. A
//! singleton control worker leases jobs with `SKIP LOCKED`; a crash leaves the
//! lease recoverable and detector/event uniqueness makes every replay safe.

use chrono::{DateTime, Utc};
use futures::StreamExt;
use sea_orm::DatabaseConnection;
use serde_json::Value;
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};

use super::SuspicionType;

const JOB_KIND_SUBMISSION: i16 = 0;
const JOB_KIND_DIRECT_SUSPICION: i16 = 1;
const CLAIM_LIMIT: i64 = 32;
const LEASE_SECONDS: i64 = 300;
const OUTBOX_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
const DEFAULT_RECONCILE_SECONDS: u64 = 30;
const DEFAULT_FINALIZE_GRACE_SECONDS: u64 = 360;
const MAX_RECONCILE_SECONDS: u64 = 3600;
const GAME_RECONCILE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(25);
const RECONCILE_PASS_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);
const RECONCILE_GAME_CONCURRENCY: usize = 4;
const DIRTY_ABNORMAL_SOLVE: i64 = 1 << 0;
const DIRTY_STATISTICAL: i64 = 1 << 1;
const DIRTY_CORRELATION: i64 = 1 << 2;
const DIRTY_CONTAINER_ACCESS: i64 = 1 << 3;
const DIRTY_HONEYPOT: i64 = 1 << 4;
const DIRTY_EVENT_SECURITY: i64 = 1 << 5;
const DIRTY_ALL: i64 = DIRTY_ABNORMAL_SOLVE
    | DIRTY_STATISTICAL
    | DIRTY_CORRELATION
    | DIRTY_CONTAINER_ACCESS
    | DIRTY_HONEYPOT
    | DIRTY_EVENT_SECURITY;
mod watermarks;
use watermarks::*;
const RECONCILE_GAMES_SQL: &str = r#"
    WITH observed_clock AS MATERIALIZED (
      SELECT clock_timestamp() AS db_now
    )
    SELECT game.id,
           game.end_time_utc
             + ($1::bigint * INTERVAL '1 second') <= observed_clock.db_now
               AS barrier_backed_final
      FROM "Games" game
      CROSS JOIN observed_clock
      JOIN "SuspicionReconciliationState" reconciliation
        ON reconciliation.game_id = game.id
     WHERE game.deletion_pending = FALSE
       AND game.start_time_utc <= observed_clock.db_now
       AND (
             (
               game.end_time_utc > observed_clock.db_now
               AND reconciliation.evidence_closed_at_utc IS NULL
               AND reconciliation.dirty_generation > reconciliation.completed_generation
               AND reconciliation.dirty_mask <> 0
             )
             OR (
                  game.end_time_utc
                    + ($1::bigint * INTERVAL '1 second')
                      <= observed_clock.db_now
                  AND reconciliation.sealed_at_utc IS NULL
                )
           )
       AND (reconciliation.lease_expires_at_utc IS NULL
            OR reconciliation.lease_expires_at_utc <= observed_clock.db_now)
     ORDER BY game.id
     LIMIT 32
"#;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(i16)]
pub enum EvaluationSourceKind {
    Submission = 0,
    ContainerAccess = 2,
}

fn parse_reconcile_seconds(raw: Option<&str>) -> anyhow::Result<u64> {
    let seconds = match raw {
        Some(raw) => raw
            .parse::<u64>()
            .map_err(|_| anyhow::anyhow!("RSCTF_SUSPICION_RECONCILE_SECONDS must be an integer"))?,
        None => DEFAULT_RECONCILE_SECONDS,
    };
    if !(1..=MAX_RECONCILE_SECONDS).contains(&seconds) {
        anyhow::bail!(
            "RSCTF_SUSPICION_RECONCILE_SECONDS must be between 1 and {MAX_RECONCILE_SECONDS}"
        );
    }
    Ok(seconds)
}

fn parse_finalize_grace_seconds(raw: Option<&str>) -> anyhow::Result<u64> {
    let seconds = match raw {
        Some(raw) => raw.parse::<u64>().map_err(|_| {
            anyhow::anyhow!("RSCTF_SUSPICION_FINALIZE_GRACE_SECONDS must be an integer")
        })?,
        None => DEFAULT_FINALIZE_GRACE_SECONDS,
    };
    if !(1..=MAX_RECONCILE_SECONDS).contains(&seconds) {
        anyhow::bail!(
            "RSCTF_SUSPICION_FINALIZE_GRACE_SECONDS must be between 1 and {MAX_RECONCILE_SECONDS}"
        );
    }
    Ok(seconds)
}

pub fn validate_evaluation_reconciler_config() -> anyhow::Result<()> {
    parse_reconcile_seconds(
        std::env::var("RSCTF_SUSPICION_RECONCILE_SECONDS")
            .ok()
            .as_deref(),
    )?;
    parse_finalize_grace_seconds(
        std::env::var("RSCTF_SUSPICION_FINALIZE_GRACE_SECONDS")
            .ok()
            .as_deref(),
    )?;
    Ok(())
}

fn reconciliation_interval() -> std::time::Duration {
    let seconds = parse_reconcile_seconds(
        std::env::var("RSCTF_SUSPICION_RECONCILE_SECONDS")
            .ok()
            .as_deref(),
    )
    .expect("suspicion reconciler configuration must be validated before startup");
    std::time::Duration::from_secs(seconds)
}

fn finalization_grace_seconds() -> u64 {
    parse_finalize_grace_seconds(
        std::env::var("RSCTF_SUSPICION_FINALIZE_GRACE_SECONDS")
            .ok()
            .as_deref(),
    )
    .expect("suspicion reconciler configuration must be validated before startup")
}

#[derive(Debug, sqlx::FromRow)]
struct LeasedEvaluation {
    id: i64,
    job_kind: i16,
    source_kind: i16,
    source_id: i32,
    game_id: i32,
    participation_id: i32,
    challenge_id: Option<i32>,
    rule_kind: Option<i16>,
    evidence_key: String,
    observed_at_utc: DateTime<Utc>,
    attempts: i32,
}

fn payload_is_object(payload: &Value) -> AppResult<()> {
    if payload.is_object() {
        Ok(())
    } else {
        Err(AppError::internal(
            "suspicion evaluation payload must be a JSON object",
        ))
    }
}

/// Ensure the exact post-submit evaluation exists in the grading transaction.
/// m0089's AFTER INSERT trigger creates it for rolling compatibility with old
/// web replicas; this explicit check makes any conflicting/corrupt provenance
/// fail the new writer's transaction instead of being silently accepted.
#[allow(clippy::too_many_arguments)]
pub async fn enqueue_submission_evaluation(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    submission_id: i32,
    game_id: i32,
    participation_id: i32,
    challenge_id: i32,
    observed_at_utc: DateTime<Utc>,
) -> AppResult<bool> {
    let evidence_key = super::submission_evidence_key(submission_id);
    sqlx::query(
        r#"INSERT INTO "SuspicionEvaluationOutbox"
               (job_kind, source_kind, source_id, game_id, participation_id,
                challenge_id, rule_kind, evidence_key, observed_at_utc,
                evidence_payload, evidence_version)
           VALUES ($1, $2, $3, $4, $5, $6, NULL, $7, $8, '{}'::jsonb, 1)
           ON CONFLICT DO NOTHING"#,
    )
    .bind(JOB_KIND_SUBMISSION)
    .bind(EvaluationSourceKind::Submission as i16)
    .bind(submission_id)
    .bind(game_id)
    .bind(participation_id)
    .bind(challenge_id)
    .bind(&evidence_key)
    .bind(observed_at_utc)
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query_scalar(
        r#"SELECT EXISTS (
             SELECT 1
               FROM "SuspicionEvaluationOutbox"
              WHERE job_kind = $1
                AND source_kind = $2
                AND source_id = $3
                AND game_id = $4
                AND participation_id = $5
                AND challenge_id = $6
                AND rule_kind IS NULL
                AND evidence_key = $7
                AND observed_at_utc = $8
                AND evidence_payload = '{}'::jsonb
                AND evidence_version = 1
           )"#,
    )
    .bind(JOB_KIND_SUBMISSION)
    .bind(EvaluationSourceKind::Submission as i16)
    .bind(submission_id)
    .bind(game_id)
    .bind(participation_id)
    .bind(challenge_id)
    .bind(&evidence_key)
    .bind(observed_at_utc)
    .fetch_one(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

/// Atomically hand a proven non-monitor cross-team container access to the
/// standard idempotent suspicion-event writer. Honeypot telemetry is raw-only.
#[allow(clippy::too_many_arguments)]
pub async fn enqueue_direct_suspicion_evaluation(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    source_kind: EvaluationSourceKind,
    source_id: i32,
    game_id: i32,
    participation_id: i32,
    challenge_id: Option<i32>,
    ty: SuspicionType,
    evidence_key: &str,
    observed_at_utc: DateTime<Utc>,
    evidence_payload: Value,
) -> AppResult<bool> {
    if source_kind != EvaluationSourceKind::ContainerAccess
        || ty != SuspicionType::CrossTeamContainerAccess
        || challenge_id.is_none()
    {
        return Err(AppError::internal(
            "direct suspicion jobs require cross-team container access provenance",
        ));
    }
    payload_is_object(&evidence_payload)?;
    if !super::detectors::valid_evidence_key(evidence_key) {
        return Err(AppError::internal("invalid suspicion evidence key"));
    }
    let inserted = sqlx::query(
        r#"INSERT INTO "SuspicionEvaluationOutbox"
               (job_kind, source_kind, source_id, game_id, participation_id,
                challenge_id, rule_kind, evidence_key, observed_at_utc,
                evidence_payload, evidence_version)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 1)
           ON CONFLICT DO NOTHING"#,
    )
    .bind(JOB_KIND_DIRECT_SUSPICION)
    .bind(source_kind as i16)
    .bind(source_id)
    .bind(game_id)
    .bind(participation_id)
    .bind(challenge_id)
    .bind(ty.kind())
    .bind(evidence_key)
    .bind(observed_at_utc)
    .bind(sqlx::types::Json(evidence_payload))
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(inserted.rows_affected() == 1)
}

async fn claim_pending(
    pool: &sqlx::PgPool,
    limit: i64,
) -> AppResult<(Uuid, Vec<LeasedEvaluation>)> {
    let lease_token = Uuid::new_v4();
    let rows = sqlx::query_as::<_, LeasedEvaluation>(
        r#"WITH candidates AS MATERIALIZED (
               SELECT id
                 FROM "SuspicionEvaluationOutbox"
                WHERE completed_at_utc IS NULL
                  AND available_at_utc <= clock_timestamp()
                  AND (lease_expires_at_utc IS NULL
                       OR lease_expires_at_utc <= clock_timestamp())
                ORDER BY available_at_utc, id
                LIMIT $1
                FOR UPDATE SKIP LOCKED
           ), leased AS (
               UPDATE "SuspicionEvaluationOutbox" job
                  SET lease_token = $2,
                      lease_expires_at_utc = clock_timestamp()
                          + ($3::bigint * INTERVAL '1 second'),
                      attempts = attempts + 1
                 FROM candidates
                WHERE job.id = candidates.id
            RETURNING job.id, job.job_kind, job.source_kind, job.source_id,
                      job.game_id, job.participation_id, job.challenge_id,
                      job.rule_kind, job.evidence_key, job.observed_at_utc,
                      job.attempts
           )
           SELECT * FROM leased ORDER BY id"#,
    )
    .bind(limit.clamp(1, 256))
    .bind(lease_token)
    .bind(LEASE_SECONDS)
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok((lease_token, rows))
}

async fn evaluate_job(db: &DatabaseConnection, job: &LeasedEvaluation) -> AppResult<()> {
    match (job.job_kind, job.source_kind) {
        (JOB_KIND_SUBMISSION, kind) if kind == EvaluationSourceKind::Submission as i16 => {
            super::detectors::evaluate_submission_by_id(db, job.source_id).await?;
        }
        (JOB_KIND_DIRECT_SUSPICION, kind)
            if kind == EvaluationSourceKind::ContainerAccess as i16 =>
        {
            let rule_kind = job
                .rule_kind
                .and_then(SuspicionType::from_kind)
                .ok_or_else(|| AppError::internal("invalid direct suspicion rule kind"))?;
            if validate_direct_source(db.get_postgres_connection_pool(), job, rule_kind).await? {
                let mut codes = Vec::new();
                super::detectors::record_with_dedup_at(
                    db,
                    job.game_id,
                    job.participation_id,
                    job.challenge_id,
                    rule_kind,
                    &job.evidence_key,
                    job.observed_at_utc,
                    &mut codes,
                )
                .await?;
            }
        }
        _ => return Err(AppError::internal("unsupported suspicion evaluation job")),
    }
    Ok(())
}

async fn validate_direct_source(
    pool: &sqlx::PgPool,
    job: &LeasedEvaluation,
    rule_kind: SuspicionType,
) -> AppResult<bool> {
    let competitive = if job.source_kind == EvaluationSourceKind::ContainerAccess as i16 {
        if rule_kind != SuspicionType::CrossTeamContainerAccess {
            None
        } else {
            sqlx::query_scalar::<_, bool>(
                r#"SELECT access.connected_at_utc >= game.start_time_utc
                          AND access.connected_at_utc < game.end_time_utc
                       FROM "ContainerAccessEvents" access
                       JOIN "Games" game ON game.id = access.game_id
                      WHERE access.id = $1
                        AND access.game_id = $2
                        AND access.accessing_participation_id = $3
                        AND access.container_owner_participation_id <> $3
                        AND access.is_monitor = FALSE
                        AND access.challenge_id = $4
                        AND access.connected_at_utc = $5
                   "#,
            )
            .bind(job.source_id)
            .bind(job.game_id)
            .bind(job.participation_id)
            .bind(job.challenge_id)
            .bind(job.observed_at_utc)
            .fetch_optional(pool)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?
        }
    } else {
        None
    };

    competitive.ok_or_else(|| {
        AppError::internal("suspicion evaluation source provenance does not match its durable job")
    })
}

async fn complete_job(pool: &sqlx::PgPool, id: i64, lease_token: Uuid) -> AppResult<()> {
    let affected = sqlx::query(
        r#"UPDATE "SuspicionEvaluationOutbox"
              SET completed_at_utc = clock_timestamp(),
                  lease_token = NULL,
                  lease_expires_at_utc = NULL,
                  last_error = NULL
            WHERE id = $1 AND lease_token = $2"#,
    )
    .bind(id)
    .bind(lease_token)
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if affected.rows_affected() != 1 {
        return Err(AppError::internal("suspicion evaluation lease was lost"));
    }
    Ok(())
}

async fn release_failed_job(
    pool: &sqlx::PgPool,
    job: &LeasedEvaluation,
    lease_token: Uuid,
    error: &str,
) -> AppResult<()> {
    let exponent = u32::try_from(job.attempts.clamp(1, 11)).unwrap_or(11);
    let delay_seconds = 1_i64.checked_shl(exponent).unwrap_or(3600).min(3600);
    let affected = sqlx::query(
        r#"UPDATE "SuspicionEvaluationOutbox"
              SET available_at_utc = clock_timestamp()
                    + ($3::bigint * INTERVAL '1 second'),
                  lease_token = NULL,
                  lease_expires_at_utc = NULL,
                  last_error = LEFT($4, 4000)
            WHERE id = $1 AND lease_token = $2"#,
    )
    .bind(job.id)
    .bind(lease_token)
    .bind(delay_seconds)
    .bind(error)
    .execute(pool)
    .await
    .map_err(|db_error| AppError::internal(db_error.to_string()))?;
    if affected.rows_affected() != 1 {
        return Err(AppError::internal("suspicion evaluation lease was lost"));
    }
    Ok(())
}

/// Process a bounded batch. One poison job is backed off and cannot prevent
/// unrelated later jobs from being claimed or completed.
pub async fn reconcile_evaluation_outbox(db: &DatabaseConnection, limit: i64) -> AppResult<usize> {
    let pool = db.get_postgres_connection_pool();
    let (lease_token, jobs) = claim_pending(pool, limit).await?;
    let claimed = jobs.len();
    for job in jobs {
        match evaluate_job(db, &job).await {
            Ok(()) => complete_job(pool, job.id, lease_token).await?,
            Err(error) => {
                let message = error.to_string();
                release_failed_job(pool, &job, lease_token, &message).await?;
                tracing::warn!(
                    evaluation_job = job.id,
                    source_kind = job.source_kind,
                    source_id = job.source_id,
                    attempts = job.attempts,
                    %error,
                    "durable suspicion evaluation failed; retry scheduled"
                );
            }
        }
    }
    Ok(claimed)
}

/// Wait for every evidence producer that already owns this game's shared row
/// lock, then durably close competitive intake before releasing the barrier.
/// Every producer rechecks this marker after obtaining Games FOR SHARE, so a
/// later database-clock rollback cannot admit apparently pre-end evidence.
async fn close_competitive_evidence_window(
    pool: &sqlx::PgPool,
    game_id: i32,
    finalize_grace_seconds: u64,
) -> AppResult<bool> {
    let mut barrier = pool
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let final_snapshot_is_due = sqlx::query_scalar::<_, bool>(
        r#"SELECT end_time_utc
                    + ($2::bigint * INTERVAL '1 second') <= clock_timestamp()
             FROM "Games"
            WHERE id = $1 AND deletion_pending = FALSE
            FOR UPDATE"#,
    )
    .bind(game_id)
    .bind(i64::try_from(finalize_grace_seconds).expect("validated grace fits i64"))
    .fetch_optional(&mut *barrier)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("game not found"))?;
    if final_snapshot_is_due {
        sqlx::query(
            r#"INSERT INTO "SuspicionReconciliationState"
                   (game_id, evidence_closed_at_utc, attempts)
               VALUES ($1, clock_timestamp(), 0)
               ON CONFLICT (game_id) DO UPDATE
                 SET evidence_closed_at_utc = COALESCE(
                       "SuspicionReconciliationState".evidence_closed_at_utc,
                       EXCLUDED.evidence_closed_at_utc
                     )"#,
        )
        .bind(game_id)
        .execute(&mut *barrier)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    barrier
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(final_snapshot_is_due)
}

async fn incomplete_competitive_jobs(pool: &sqlx::PgPool, game_id: i32) -> AppResult<i64> {
    sqlx::query_scalar(
        r#"SELECT COUNT(*)::bigint
             FROM "SuspicionEvaluationOutbox" job
             JOIN "Games" game ON game.id = job.game_id
            WHERE job.game_id = $1
              AND job.completed_at_utc IS NULL
              AND job.observed_at_utc >= game.start_time_utc
              AND job.observed_at_utc < game.end_time_utc"#,
    )
    .bind(game_id)
    .fetch_one(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

async fn defer_final_for_incomplete_jobs(
    pool: &sqlx::PgPool,
    game_id: i32,
) -> AppResult<Option<i64>> {
    let incomplete = incomplete_competitive_jobs(pool, game_id).await?;
    if incomplete == 0 {
        return Ok(None);
    }
    let message = format!("{incomplete} in-window suspicion evaluation jobs remain incomplete");
    sqlx::query(
        r#"UPDATE "SuspicionReconciliationState"
              SET last_error = $2
            WHERE game_id = $1"#,
    )
    .bind(game_id)
    .bind(&message)
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    tracing::warn!(
        game = game_id,
        incomplete_jobs = incomplete,
        "final suspicion snapshot deferred until durable jobs finish"
    );
    Ok(Some(incomplete))
}

async fn reconcile_one_game(
    state: &SharedState,
    game_id: i32,
    seal: bool,
    finalize_grace_seconds: u64,
) -> AppResult<Option<usize>> {
    let Some(mut claim) = claim_game_reconciliation(state.pg(), game_id, seal).await? else {
        return Ok(None);
    };
    if seal
        && !close_competitive_evidence_window(state.pg(), game_id, finalize_grace_seconds).await?
    {
        // PostgreSQL's wall clock moved backward after game selection. Do not
        // scan or seal; a later pass will make a fresh database-time decision.
        sqlx::query(
            r#"UPDATE "SuspicionReconciliationState"
                  SET lease_token = NULL, lease_expires_at_utc = NULL
                WHERE game_id = $1 AND lease_token = $2"#,
        )
        .bind(game_id)
        .bind(claim.token)
        .execute(state.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(None);
    }
    if seal {
        // The final target must be captured after the evidence barrier drains
        // producers that entered before closure. The detector sweep is full,
        // while these cursors make the durable completion state exact.
        claim.sources = capture_source_window(state.pg(), game_id, true).await?;
    }

    let snapshot = if seal {
        super::detectors::ReconciliationSnapshot::BarrierBackedFinal
    } else {
        super::detectors::ReconciliationSnapshot::Live
    };
    let detector_work = async {
        let mut errors = Vec::new();
        let mut inserted = 0;
        let mut failed_mask = 0_i64;
        if seal {
            if let Some(incomplete) = defer_final_for_incomplete_jobs(state.pg(), game_id).await? {
                errors.push(format!(
                    "{incomplete} in-window suspicion evaluation jobs remain incomplete"
                ));
                return Ok::<_, AppError>((errors, inserted, claim.dirty_mask));
            }
        }
        // Live submission-derived rules are evaluated incrementally by the
        // durable per-submission outbox. Reloading every accepted and wrong
        // submission here would make idle cost grow for no new evidence. The
        // authoritative barrier-backed pass still rebuilds the full reference
        // snapshot after intake closes.
        if snapshot == super::detectors::ReconciliationSnapshot::BarrierBackedFinal
            && claim.dirty_mask & DIRTY_ABNORMAL_SOLVE != 0
        {
            if let Err(error) = super::cheat_checks::run_abnormal_solve_checks_for_snapshot(
                state, game_id, snapshot,
            )
            .await
            {
                errors.push(format!("abnormal solve: {error}"));
                failed_mask |= DIRTY_ABNORMAL_SOLVE;
            }
        }
        if claim.dirty_mask & DIRTY_STATISTICAL != 0 {
            if let Err(error) =
                super::cheat_stat::run_statistical_checks_for_snapshot(state, game_id, snapshot)
                    .await
            {
                errors.push(format!("statistical: {error}"));
                failed_mask |= DIRTY_STATISTICAL;
            }
        }
        if claim.dirty_mask & DIRTY_CORRELATION != 0 {
            if let Err(error) = super::correlation::run_correlation_checks_incremental(
                &state.db,
                game_id,
                snapshot,
                claim.sources.after.identity_observation_id,
                claim.sources.through.identity_observation_id,
            )
            .await
            {
                errors.push(format!("correlation: {error}"));
                failed_mask |= DIRTY_CORRELATION;
            }
        }
        if claim.dirty_mask & DIRTY_CONTAINER_ACCESS != 0 {
            if let Err(error) = super::container_access::run_container_access_checks_for_snapshot(
                state, game_id, snapshot,
            )
            .await
            {
                errors.push(format!("container access: {error}"));
                failed_mask |= DIRTY_CONTAINER_ACCESS;
            }
        }
        if claim.dirty_mask & DIRTY_HONEYPOT != 0 {
            if let Err(error) = super::run_honeypot_chain_checks(state, game_id).await {
                errors.push(format!("honeypot chain: {error}"));
                failed_mask |= DIRTY_HONEYPOT;
            }
        }
        if claim.dirty_mask & DIRTY_EVENT_SECURITY != 0 {
            match crate::services::event_security::derive_context_findings_incremental(
                state,
                game_id,
                crate::services::event_security::ContextFindingWatermarks {
                    dns_after: claim.sources.after.dns_revision,
                    dns_through: claim.sources.through.dns_revision,
                    network_after: claim.sources.after.network_revision,
                    network_through: claim.sources.through.network_revision,
                    flag_after: claim.sources.after.flag_transport_id,
                    flag_through: claim.sources.through.flag_transport_id,
                    cheat_after: claim.sources.after.cheat_info_id,
                    cheat_through: claim.sources.through.cheat_info_id,
                    final_snapshot: seal,
                },
            )
            .await
            {
                Ok(count) => inserted = count,
                Err(error) => {
                    errors.push(format!("event-security context: {error}"));
                    failed_mask |= DIRTY_EVENT_SECURITY;
                }
            }
        }
        Ok((errors, inserted, failed_mask))
    };
    let (errors, inserted, failed_mask) =
        match tokio::time::timeout(GAME_RECONCILE_DEADLINE, detector_work).await {
            Ok(result) => result?,
            Err(_) => (
                vec!["detector deadline exceeded".to_string()],
                0,
                claim.dirty_mask,
            ),
        };

    for error in &errors {
        tracing::warn!(game = game_id, %error, "suspicion game reconciliation detector failed");
    }
    let mut completion = state
        .pg()
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    record_game_reconciliation(
        &mut completion,
        game_id,
        &claim,
        seal,
        inserted,
        &errors,
        claim.dirty_mask & !failed_mask,
    )
    .await?;
    completion
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(Some(inserted))
}

/// Coalesced game-level sweeps. Ended games pause during the finalization grace;
/// a durable sealed marker then guarantees one barrier-backed final pass even
/// across control restarts. Practice never extends the competitive window.
pub async fn reconcile_games(state: &SharedState) -> AppResult<usize> {
    let finalize_grace_seconds = finalization_grace_seconds();
    let games: Vec<(i32, bool)> = sqlx::query_as(RECONCILE_GAMES_SQL)
        .bind(i64::try_from(finalize_grace_seconds).expect("validated grace fits i64"))
        .fetch_all(state.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    let pass = async {
        let mut work = futures::stream::iter(games)
            .map(|(game_id, seal)| async move {
                (
                    game_id,
                    reconcile_one_game(state, game_id, seal, finalize_grace_seconds).await,
                )
            })
            .buffer_unordered(RECONCILE_GAME_CONCURRENCY);
        let mut reconciled = 0;
        while let Some((game_id, result)) = work.next().await {
            match result {
                Ok(Some(_)) => reconciled += 1,
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(game = game_id, %error, "suspicion game reconciliation failed");
                }
            }
        }
        reconciled
    };
    tokio::time::timeout(RECONCILE_PASS_DEADLINE, pass)
        .await
        .map_err(|_| AppError::unavailable("Suspicion reconciliation pass timed out"))
}

#[derive(Debug, Clone)]
pub struct ManualReconciliationOperation {
    pub operation_id: Uuid,
    pub generation: i64,
    pub status: i16,
    pub inserted: Option<i32>,
}

async fn load_manual_operation(
    pool: &sqlx::PgPool,
    operation_id: Uuid,
    game_id: i32,
    requested_by: Uuid,
) -> AppResult<Option<ManualReconciliationOperation>> {
    let row: Option<(i32, Uuid, i64, i16, Option<i32>)> = sqlx::query_as(
        r#"SELECT game_id, requested_by, generation, status, inserted_count
             FROM "SuspicionReconciliationOperations"
            WHERE operation_id = $1"#,
    )
    .bind(operation_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((bound_game, bound_user, generation, status, inserted)) = row else {
        return Ok(None);
    };
    if bound_game != game_id || bound_user != requested_by {
        return Err(AppError::conflict(
            "Reconciliation operation ID is bound to another request",
        ));
    }
    Ok(Some(ManualReconciliationOperation {
        operation_id,
        generation,
        status,
        inserted,
    }))
}

/// Coalesce manual and scheduled anti-cheat work behind the same durable game
/// generation. An exact operation replay never starts another detector sweep.
pub async fn request_manual_reconciliation(
    state: &SharedState,
    game_id: i32,
    requested_by: Uuid,
    operation_id: Uuid,
) -> AppResult<ManualReconciliationOperation> {
    if let Some(existing) =
        load_manual_operation(state.pg(), operation_id, game_id, requested_by).await?
    {
        if existing.status != 0 {
            return Ok(existing);
        }
        let seal: bool = sqlx::query_scalar(
            r#"SELECT end_time_utc
                        + ($2::bigint * INTERVAL '1 second') <= clock_timestamp()
                 FROM "Games" WHERE id = $1"#,
        )
        .bind(game_id)
        .bind(i64::try_from(finalization_grace_seconds()).expect("validated grace fits i64"))
        .fetch_one(state.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        let _ = reconcile_one_game(state, game_id, seal, finalization_grace_seconds()).await?;
        return load_manual_operation(state.pg(), operation_id, game_id, requested_by)
            .await?
            .ok_or_else(|| AppError::internal("Reconciliation operation disappeared"));
    }
    let mut transaction = state
        .pg()
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let generation = sqlx::query_scalar::<_, i64>(
        r#"UPDATE "SuspicionReconciliationState" reconciliation
              SET dirty_generation = dirty_generation + 1,
                  dirty_mask = dirty_mask | $2
             FROM "Games" game
            WHERE reconciliation.game_id = $1
              AND game.id = reconciliation.game_id
              AND game.deletion_pending = FALSE
        RETURNING reconciliation.dirty_generation"#,
    )
    .bind(game_id)
    .bind(DIRTY_ALL)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Game not found"))?;
    let inserted = sqlx::query(
        r#"INSERT INTO "SuspicionReconciliationOperations"
               (operation_id, game_id, requested_by, generation)
           VALUES ($1, $2, $3, $4)
           ON CONFLICT (operation_id) DO NOTHING"#,
    )
    .bind(operation_id)
    .bind(game_id)
    .bind(requested_by)
    .bind(generation)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    if inserted == 0 {
        transaction
            .rollback()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return load_manual_operation(state.pg(), operation_id, game_id, requested_by)
            .await?
            .ok_or_else(|| AppError::conflict("Reconciliation operation raced"));
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    let seal: bool = sqlx::query_scalar(
        r#"SELECT end_time_utc
                    + ($2::bigint * INTERVAL '1 second') <= clock_timestamp()
             FROM "Games" WHERE id = $1"#,
    )
    .bind(game_id)
    .bind(i64::try_from(finalization_grace_seconds()).expect("validated grace fits i64"))
    .fetch_one(state.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let _ = reconcile_one_game(state, game_id, seal, finalization_grace_seconds()).await?;
    load_manual_operation(state.pg(), operation_id, game_id, requested_by)
        .await?
        .ok_or_else(|| AppError::internal("Reconciliation operation disappeared"))
}

/// Start the durable evaluator. Wire this as a required worker only for
/// `RuntimeRole::All | RuntimeRole::Development | RuntimeRole::Control |
/// RuntimeRole::Engine`; every other role enqueues but does not compete to run
/// historical sweeps.
pub fn start_evaluation_reconciler(
    state: SharedState,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let game_interval = reconciliation_interval();
        let mut next_game_reconciliation = tokio::time::Instant::now();
        loop {
            if *shutdown.borrow() {
                break;
            }
            if let Err(error) = reconcile_evaluation_outbox(&state.db, CLAIM_LIMIT).await {
                tracing::error!(%error, "suspicion evaluation reconciler pass failed");
            }
            if tokio::time::Instant::now() >= next_game_reconciliation {
                if let Err(error) = reconcile_games(&state).await {
                    tracing::error!(%error, "suspicion game reconciliation pass failed");
                }
                next_game_reconciliation = tokio::time::Instant::now() + game_interval;
            }
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
                () = tokio::time::sleep(OUTBOX_POLL_INTERVAL) => {}
            }
        }
    })
}

#[cfg(test)]
mod test_support;
#[cfg(test)]
#[path = "outbox_tests.rs"]
mod tests;
#[cfg(test)]
pub(crate) use test_support::seal_reconciled_game_for_test;
