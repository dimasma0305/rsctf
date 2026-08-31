//! Submission-derived behavioral detectors and suspicion-event persistence.
use super::*;

/// Burst: this many distinct challenges solved …
const BURST_MIN_SOLVES: usize = 3;
/// … within this window (seconds) trips [`SuspicionType::Burst`].
const BURST_WINDOW_SECS: i64 = 60;
/// HighWrongRate needs this many wrong answers for one challenge inside the
/// rolling 60-second window encoded in [`HIGH_WRONG_RATE_HITS_SQL`].
const HIGH_WRONG_MIN: i64 = 40;
const COMMON_SOLVE_RATE: f64 = 0.40;
const EASY_ZERO_ATTEMPT_RATE: f64 = 0.30;
/// Hoarding: solved this long (seconds) after the instance's last container
/// operation (a destroy, in the fire case) — RSCTF uses 60 minutes.
const HOARDING_MIN_GAP_SECS: i64 = 60 * 60;
const MAX_EVIDENCE_KEY_BYTES: usize = 128;
// Persisted game, challenge, and participation ids are positive. Reserve one
// negative first key for the participant-wide detector/submit lock namespace.
const SUSPICION_SCORE_LOCK_NAMESPACE: i32 = -1_389_606_228;

/// Competitive interval; submissions at or after `end` are practice evidence.
#[derive(Clone, Copy, Debug)]
pub(super) struct CompetitiveGameWindow {
    pub start: chrono::DateTime<chrono::Utc>,
    pub end: chrono::DateTime<chrono::Utc>,
}

/// Reconciliation authority supplied by the durable game-seal path. A wall
/// clock past `Games.end_time_utc` is not enough to authorize non-monotonic
/// detectors: the final variant is issued only after the configured grace and
/// the scheduler's game-row barrier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ReconciliationSnapshot {
    Live,
    BarrierBackedFinal,
}

#[derive(Clone, Debug)]
pub(super) struct CanonicalSolve {
    pub participation_id: i32,
    pub team_id: i32,
    pub challenge_id: i32,
    pub submit_time_utc: chrono::DateTime<chrono::Utc>,
    pub container_id: Option<uuid::Uuid>,
    pub container_last_operation_at_submit: Option<chrono::DateTime<chrono::Utc>>,
    pub container_was_loaded_at_submit: Option<bool>,
}

#[derive(Clone, Debug)]
pub(super) struct CompetitiveWrongAttempt {
    pub participation_id: i32,
    pub team_id: i32,
    pub challenge_id: i32,
    pub submit_time_utc: chrono::DateTime<chrono::Utc>,
}

type CanonicalSolveRow = (
    i32,
    i32,
    i32,
    chrono::DateTime<chrono::Utc>,
    Option<uuid::Uuid>,
    Option<chrono::DateTime<chrono::Utc>>,
    Option<bool>,
);

type SubmissionObservationRow = (
    chrono::DateTime<chrono::Utc>,
    Option<uuid::Uuid>,
    Option<chrono::DateTime<chrono::Utc>>,
    Option<bool>,
);

pub(super) fn is_common_ordering_challenge(solve_count: usize, participant_count: usize) -> bool {
    participant_count > 0 && solve_count as f64 / participant_count as f64 > COMMON_SOLVE_RATE
}

pub(super) fn is_easy_challenge(
    solve_count: usize,
    participant_count: usize,
    zero_attempt_rate: f64,
) -> bool {
    is_common_ordering_challenge(solve_count, participant_count)
        || zero_attempt_rate > EASY_ZERO_ATTEMPT_RATE
}

#[inline]
pub(super) fn in_competitive_window(
    at: chrono::DateTime<chrono::Utc>,
    window: CompetitiveGameWindow,
) -> bool {
    at >= window.start && at < window.end
}

#[inline]
pub(super) fn final_snapshot_ready(snapshot: ReconciliationSnapshot) -> bool {
    snapshot == ReconciliationSnapshot::BarrierBackedFinal
}

pub(super) fn is_hoarded_submission(
    submitted_at: chrono::DateTime<chrono::Utc>,
    has_container: bool,
    last_operation: Option<chrono::DateTime<chrono::Utc>>,
    was_loaded: Option<bool>,
) -> bool {
    matches!(
        (last_operation, was_loaded),
        (Some(last_operation), Some(false))
            if !has_container
                && submitted_at - last_operation
                    > chrono::Duration::seconds(HOARDING_MIN_GAP_SECS)
    )
}

/// Earliest canonical solve that completes a qualifying burst, independent of
/// which durable submission job happens to replay first.
fn earliest_burst_completion(
    mut solve_times: Vec<chrono::DateTime<chrono::Utc>>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    if solve_times.len() < BURST_MIN_SOLVES {
        return None;
    }
    solve_times.sort_unstable();
    for end in (BURST_MIN_SOLVES - 1)..solve_times.len() {
        let start = end + 1 - BURST_MIN_SOLVES;
        if solve_times[end] - solve_times[start] <= chrono::Duration::seconds(BURST_WINDOW_SECS) {
            return Some(solve_times[end]);
        }
    }
    None
}

pub(super) async fn load_competitive_game_window(
    pool: &sqlx::PgPool,
    game_id: i32,
) -> AppResult<Option<CompetitiveGameWindow>> {
    let row: Option<(chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>)> =
        sqlx::query_as(
            r#"SELECT start_time_utc, end_time_utc
                 FROM "Games"
                WHERE id = $1"#,
        )
        .bind(game_id)
        .fetch_optional(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(row.map(|(start, end)| CompetitiveGameWindow { start, end }))
}

/// One scoreboard-authoritative solve per `(participation, challenge)`.
pub(super) async fn load_canonical_solves(
    pool: &sqlx::PgPool,
    game_id: i32,
    window: CompetitiveGameWindow,
) -> AppResult<Vec<CanonicalSolve>> {
    load_canonical_solves_scoped(pool, game_id, window, None).await
}

async fn load_canonical_solves_scoped(
    pool: &sqlx::PgPool,
    game_id: i32,
    window: CompetitiveGameWindow,
    participation_id: Option<i32>,
) -> AppResult<Vec<CanonicalSolve>> {
    let rows: Vec<CanonicalSolveRow> = sqlx::query_as(
        r#"SELECT submission.participation_id,
                  submission.team_id,
                  submission.challenge_id,
                  submission.submit_time_utc,
                  submission.container_id,
                  submission.container_last_operation_at_submit,
                  submission.container_was_loaded_at_submit
             FROM "FirstSolves" first_solve
             JOIN "Submissions" submission
               ON submission.id = first_solve.submission_id
              AND submission.participation_id = first_solve.participation_id
              AND submission.challenge_id = first_solve.challenge_id
             JOIN "Participations" participation
               ON participation.id = submission.participation_id
              AND participation.game_id = submission.game_id
            WHERE submission.game_id = $1
              AND submission.status = $2
              AND submission.submit_time_utc >= $3
              AND submission.submit_time_utc < $4
              AND participation.competitive_admitted_at_utc IS NOT NULL
              AND participation.competitive_admitted_at_utc < $4
              AND ($5::integer IS NULL OR submission.participation_id = $5)
            ORDER BY submission.submit_time_utc, submission.id"#,
    )
    .bind(game_id)
    .bind(crate::utils::enums::AnswerResult::Accepted as i16)
    .bind(window.start)
    .bind(window.end)
    .bind(participation_id)
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    Ok(rows
        .into_iter()
        .map(
            |(
                participation_id,
                team_id,
                challenge_id,
                submit_time_utc,
                container_id,
                container_last_operation_at_submit,
                container_was_loaded_at_submit,
            )| CanonicalSolve {
                participation_id,
                team_id,
                challenge_id,
                submit_time_utc,
                container_id,
                container_last_operation_at_submit,
                container_was_loaded_at_submit,
            },
        )
        .collect())
}

pub(super) async fn load_competitive_wrong_attempts(
    pool: &sqlx::PgPool,
    game_id: i32,
    window: CompetitiveGameWindow,
) -> AppResult<Vec<CompetitiveWrongAttempt>> {
    let rows: Vec<(i32, i32, i32, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        r#"SELECT submission.participation_id,
                  submission.team_id,
                  submission.challenge_id,
                  submission.submit_time_utc
             FROM "Submissions" submission
             JOIN "Participations" participation
               ON participation.id = submission.participation_id
              AND participation.game_id = submission.game_id
            WHERE submission.game_id = $1
              AND submission.status = $2
              AND submission.submit_time_utc >= $3
              AND submission.submit_time_utc < $4
              AND participation.competitive_admitted_at_utc IS NOT NULL
              AND participation.competitive_admitted_at_utc < $4
            ORDER BY submission.submit_time_utc, submission.id"#,
    )
    .bind(game_id)
    .bind(crate::utils::enums::AnswerResult::WrongAnswer as i16)
    .bind(window.start)
    .bind(window.end)
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    Ok(rows
        .into_iter()
        .map(
            |(participation_id, team_id, challenge_id, submit_time_utc)| CompetitiveWrongAttempt {
                participation_id,
                team_id,
                challenge_id,
                submit_time_utc,
            },
        )
        .collect())
}

/// The single HighWrongRate definition used by both live evaluation and the
/// report sweep: at least 40 wrong attempts on one challenge in a rolling 60s
/// window, unless that challenge's canonical solve followed the window anchor
/// within five minutes. `participation_id = None` evaluates the whole game.
const HIGH_WRONG_RATE_HITS_SQL: &str = r#"
    WITH wrong_windows AS MATERIALIZED (
        SELECT submission.participation_id,
               submission.challenge_id,
               submission.submit_time_utc AS anchor_time,
               COUNT(*) OVER (
                   PARTITION BY submission.participation_id, submission.challenge_id
                   ORDER BY submission.submit_time_utc
                   RANGE BETWEEN CURRENT ROW
                         AND '60 seconds'::interval FOLLOWING
               ) AS wrong_count,
               NTH_VALUE(
                   submission.submit_time_utc,
                   ($7::bigint)::integer
               ) OVER (
                   PARTITION BY submission.participation_id, submission.challenge_id
                   ORDER BY submission.submit_time_utc
                   RANGE BETWEEN CURRENT ROW
                         AND '60 seconds'::interval FOLLOWING
               ) AS threshold_time
          FROM "Submissions" submission
          JOIN "Participations" participation
            ON participation.id = submission.participation_id
           AND participation.game_id = submission.game_id
         WHERE submission.game_id = $1
           AND submission.status = $2
           AND submission.submit_time_utc >= $3
           AND submission.submit_time_utc < $4
           AND ($5::integer IS NULL OR submission.participation_id = $5)
           AND participation.competitive_admitted_at_utc IS NOT NULL
           AND participation.competitive_admitted_at_utc < $4
    ), canonical_solves AS MATERIALIZED (
        SELECT submission.participation_id,
               submission.challenge_id,
               submission.submit_time_utc
          FROM "FirstSolves" first_solve
          JOIN "Submissions" submission
            ON submission.id = first_solve.submission_id
           AND submission.participation_id = first_solve.participation_id
           AND submission.challenge_id = first_solve.challenge_id
          JOIN "Participations" participation
            ON participation.id = submission.participation_id
           AND participation.game_id = submission.game_id
         WHERE submission.game_id = $1
           AND submission.status = $6
           AND submission.submit_time_utc >= $3
           AND submission.submit_time_utc < $4
           AND participation.competitive_admitted_at_utc IS NOT NULL
           AND participation.competitive_admitted_at_utc < $4
    )
    SELECT wrong_window.participation_id,
           wrong_window.challenge_id,
           MIN(wrong_window.threshold_time) AS observed_at
      FROM wrong_windows wrong_window
     WHERE wrong_window.wrong_count >= $7
       AND LEAST(
               wrong_window.anchor_time + '5 minutes'::interval,
               $4::timestamptz
           ) <= CURRENT_TIMESTAMP
       AND NOT EXISTS (
           SELECT 1
             FROM canonical_solves solve
            WHERE solve.participation_id = wrong_window.participation_id
              AND solve.challenge_id = wrong_window.challenge_id
              AND solve.submit_time_utc >= wrong_window.anchor_time
              AND solve.submit_time_utc
                    <= wrong_window.anchor_time + '5 minutes'::interval
       )
     GROUP BY wrong_window.participation_id, wrong_window.challenge_id
     ORDER BY wrong_window.participation_id, wrong_window.challenge_id
"#;

pub(super) async fn high_wrong_rate_hits(
    pool: &sqlx::PgPool,
    game_id: i32,
    window: CompetitiveGameWindow,
    participation_id: Option<i32>,
) -> AppResult<Vec<(i32, i32, chrono::DateTime<chrono::Utc>)>> {
    let mut connection = pool
        .acquire()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    high_wrong_rate_hits_connection(&mut connection, game_id, window, participation_id).await
}

async fn high_wrong_rate_hits_connection(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    window: CompetitiveGameWindow,
    participation_id: Option<i32>,
) -> AppResult<Vec<(i32, i32, chrono::DateTime<chrono::Utc>)>> {
    sqlx::query_as(HIGH_WRONG_RATE_HITS_SQL)
        .bind(game_id)
        .bind(crate::utils::enums::AnswerResult::WrongAnswer as i16)
        .bind(window.start)
        .bind(window.end)
        .bind(participation_id)
        .bind(crate::utils::enums::AnswerResult::Accepted as i16)
        .bind(HIGH_WRONG_MIN)
        .fetch_all(connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

pub(super) fn valid_evidence_key(evidence_key: &str) -> bool {
    !evidence_key.trim().is_empty() && evidence_key.len() <= MAX_EVIDENCE_KEY_BYTES
}

/// Serialize detector and submit writes for one participation.
pub(crate) async fn lock_participation_suspicion_writes(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    participation_id: i32,
) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock($1, $2)")
        .bind(SUSPICION_SCORE_LOCK_NAMESPACE)
        .bind(participation_id)
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

const INSERT_SUSPICION_EVENT_SQL: &str = r#"
    WITH participant AS MATERIALIZED (
        SELECT id
          FROM "Participations"
         WHERE id = $2 AND game_id = $1
         FOR UPDATE
    ), inserted AS (
        INSERT INTO "SuspicionEvents"
            (game_id, participation_id, challenge_id, kind, evidence_key,
             score_delta, created_at)
        SELECT $1, participant.id, $3, $4, $5, $6, $7
          FROM participant
         -- The reconciliation stamp is a BEFORE INSERT trigger. Filter a
         -- steady replay by the exact unique key before that trigger runs;
         -- ON CONFLICT remains the final guard for concurrent races.
         WHERE NOT EXISTS (
               SELECT 1 FROM "SuspicionEvents" existing
                WHERE existing.game_id = $1
                  AND existing.participation_id = participant.id
                  AND existing.kind = $4
                  AND existing.evidence_key = $5
         )
        ON CONFLICT (game_id, participation_id, kind, evidence_key) DO NOTHING
        RETURNING id
    )
    SELECT EXISTS (SELECT 1 FROM participant),
           EXISTS (SELECT 1 FROM inserted)
"#;

#[allow(clippy::too_many_arguments)]
async fn persist_suspicion_event_with_weight_guarded(
    pool: &sqlx::PgPool,
    game_id: i32,
    participation_id: i32,
    challenge_id: Option<i32>,
    ty: SuspicionType,
    evidence_key: &str,
    weight: i32,
    description: &str,
    mut observed_at: chrono::DateTime<chrono::Utc>,
    high_wrong_window: Option<CompetitiveGameWindow>,
) -> AppResult<bool> {
    if !valid_evidence_key(evidence_key) {
        return Err(AppError::internal("invalid suspicion evidence key"));
    }
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    // Take the participation score scope before the shared audit/deletion fence
    // to preserve the submit path's lock order across duplicate writers.
    lock_participation_suspicion_writes(&mut transaction, participation_id)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if !crate::services::participation_evidence::lock_historical_audit_insert_scope(
        &mut transaction,
        game_id,
        challenge_id,
        &[participation_id],
    )
    .await?
    {
        return Err(AppError::not_found("participation not found"));
    }
    if let Some(window) = high_wrong_window {
        let hit_challenge_id = challenge_id.ok_or_else(|| {
            AppError::internal("HighWrongRate evidence requires a challenge identity")
        })?;
        let rechecked = high_wrong_rate_hits_connection(
            &mut transaction,
            game_id,
            window,
            Some(participation_id),
        )
        .await?
        .into_iter()
        .find(|(hit_participation_id, hit_challenge_id_, _)| {
            *hit_participation_id == participation_id && *hit_challenge_id_ == hit_challenge_id
        });
        let Some((_, _, rechecked_observed_at)) = rechecked else {
            return Ok(false);
        };
        observed_at = rechecked_observed_at;
    }
    let (participant_exists, inserted): (bool, bool) = sqlx::query_as(INSERT_SUSPICION_EVENT_SQL)
        .bind(game_id)
        .bind(participation_id)
        .bind(challenge_id)
        .bind(ty.kind())
        .bind(evidence_key)
        .bind(weight)
        .bind(observed_at)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    if !participant_exists {
        return Err(AppError::not_found("participation not found"));
    }
    if inserted {
        let new_score =
            super::recompute_participation_suspicion_score(&mut transaction, participation_id)
                .await?;
        tracing::info!(
            participation_id,
            delta = weight,
            reason = description,
            new_score,
            "suspicion event recorded"
        );
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(inserted)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn record_with_dedup_at(
    db: &DatabaseConnection,
    game_id: i32,
    participation_id: i32,
    challenge_id: Option<i32>,
    ty: SuspicionType,
    evidence_key: &str,
    observed_at: chrono::DateTime<chrono::Utc>,
    codes: &mut Vec<i16>,
) -> AppResult<()> {
    let kind = ty.kind();
    let (weight, description) = resolve_entry(db, ty).await?;
    let inserted = persist_suspicion_event_with_weight_guarded(
        db.get_postgres_connection_pool(),
        game_id,
        participation_id,
        challenge_id,
        ty,
        evidence_key,
        weight,
        description,
        observed_at,
        None,
    )
    .await?;

    if inserted && !codes.contains(&kind) {
        codes.push(kind);
    }
    Ok(())
}

/// Persist a mature HighWrongRate incident after rechecking the shared
/// challenge/window predicate under the same participation lock used by submit.
/// This closes the query→insert race where a suppressing solve could otherwise
/// commit while the detector waited for the lock.
pub(super) async fn record_high_wrong_rate_with_dedup(
    db: &DatabaseConnection,
    game_id: i32,
    participation_id: i32,
    challenge_id: i32,
    window: CompetitiveGameWindow,
    observed_at: chrono::DateTime<chrono::Utc>,
    codes: &mut Vec<i16>,
) -> AppResult<()> {
    let ty = SuspicionType::HighWrongRate;
    let kind = ty.kind();
    let (weight, description) = resolve_entry(db, ty).await?;
    let evidence_key = challenge_evidence_key(challenge_id);
    let inserted = persist_suspicion_event_with_weight_guarded(
        db.get_postgres_connection_pool(),
        game_id,
        participation_id,
        Some(challenge_id),
        ty,
        &evidence_key,
        weight,
        description,
        observed_at,
        Some(window),
    )
    .await?;
    if inserted && !codes.contains(&kind) {
        codes.push(kind);
    }
    Ok(())
}

/// Run the DB-tractable cheat-suspicion rule checks for a single flag
/// submission, persist a [`suspicion_event`] row per distinct rule that fires,
/// rebuild the participation's score projection, and return the rule codes
/// (`SuspicionEvents.kind`) that hit.
///
/// Rules evaluated here:
/// * **StolenFlag** — immutable grading-time `CheatInfo` proves that this
///   submission used another participation's dynamic flag.
/// * **HighWrongRate** — 40+ wrong submissions on one challenge inside 60s,
///   unless its canonical solve follows within five minutes (RSCTF Check H).
/// * **Burst** — `BURST_MIN_SOLVES`+ distinct challenges solved within
///   `BURST_WINDOW_SECS` (RSCTF Check 8).
/// * **Hoarding** — a container challenge solved `HOARDING_MIN_GAP_SECS` after
///   its instance's last container operation (RSCTF Check 6).
///
/// Identity observations are evaluated separately by `run_correlation_checks`.
pub async fn evaluate_submission(
    db: &DatabaseConnection,
    game_id: i32,
    participation_id: i32,
    submission_id: i32,
    challenge: &crate::models::data::game_challenge::Model,
    _answer: &str,
) -> AppResult<Vec<i16>> {
    evaluate_submission_inner(
        db,
        game_id,
        participation_id,
        submission_id,
        challenge.id,
        challenge.challenge_type,
    )
    .await
}

async fn evaluate_submission_inner(
    db: &DatabaseConnection,
    game_id: i32,
    participation_id: i32,
    submission_id: i32,
    challenge_id: i32,
    challenge_type: crate::utils::enums::ChallengeType,
) -> AppResult<Vec<i16>> {
    let mut fired: Vec<SuspicionType> = Vec::new();
    let pool = db.get_postgres_connection_pool();
    let window = load_competitive_game_window(pool, game_id)
        .await?
        .ok_or_else(|| AppError::not_found("game not found"))?;
    let current: Option<SubmissionObservationRow> = sqlx::query_as(
        r#"SELECT submit_time_utc,
                  container_id,
                  container_last_operation_at_submit,
                  container_was_loaded_at_submit
             FROM "Submissions"
            WHERE id = $1
              AND game_id = $2
              AND participation_id = $3
              AND challenge_id = $4"#,
    )
    .bind(submission_id)
    .bind(game_id)
    .bind(participation_id)
    .bind(challenge_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((submission_time, container_id, container_last_operation, container_was_loaded)) =
        current
    else {
        return Err(AppError::not_found("submission not found"));
    };
    if !in_competitive_window(submission_time, window) {
        return Ok(Vec::new());
    }

    let stolen: bool = sqlx::query_scalar(
        r#"SELECT EXISTS (
                   SELECT 1
                     FROM "CheatInfo"
                    WHERE submission_id = $1
                      AND game_id = $2
                      AND submit_participation_id = $3
                      AND challenge_id = $4
                      AND source_participation_id <> $3
               )"#,
    )
    .bind(submission_id)
    .bind(game_id)
    .bind(participation_id)
    .bind(challenge_id)
    .fetch_one(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if stolen {
        fired.push(SuspicionType::StolenFlag);
    }

    // Persist one SuspicionEvent per distinct rule, rebuild the projection, and
    // return the detected `kind` codes. A repeated stolen-flag
    // submission receives its own durable incident key and remains in `codes`
    // for this observation.
    let mut codes: Vec<i16> = Vec::new();
    for ty in fired {
        let kind = ty.kind();
        if codes.contains(&kind) {
            continue;
        }
        let evidence_key = submission_evidence_key(submission_id);

        record_with_dedup_at(
            db,
            game_id,
            participation_id,
            Some(challenge_id),
            ty,
            &evidence_key,
            submission_time,
            &mut codes,
        )
        .await?;
        if !codes.contains(&kind) {
            codes.push(kind);
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Behavioral / brute-force rules over canonical competitive evidence. Each
    // persists at most one event per stable aggregate evidence key, deduped
    // against the audit table by [`record_with_dedup_at`]. Community-relative
    // ZeroWrongAttempts is deliberately sweep-only so the easy-challenge
    // suppression is evaluated from one complete snapshot for every team.
    // ─────────────────────────────────────────────────────────────────────────
    let canonical_solves =
        load_canonical_solves_scoped(pool, game_id, window, Some(participation_id)).await?;
    let participant_solves: Vec<&CanonicalSolve> = canonical_solves.iter().collect();
    let earliest_accept_here = participant_solves
        .iter()
        .find(|solve| solve.challenge_id == challenge_id)
        .map(|solve| solve.submit_time_utc);

    // ── Rule: HighWrongRate ──────────────────────────────────────────────────
    // Use the exact same challenge-local rolling-window query as the report
    // sweep. It may backfill another challenge for this participant when a prior
    // detector attempt failed; evidence identity remains that challenge.
    for (_, hit_challenge_id, hit_observed_at) in
        high_wrong_rate_hits(pool, game_id, window, Some(participation_id)).await?
    {
        record_high_wrong_rate_with_dedup(
            db,
            game_id,
            participation_id,
            hit_challenge_id,
            window,
            hit_observed_at,
            &mut codes,
        )
        .await?;
    }

    // ── Rule: Burst ──────────────────────────────────────────────────────────
    // >= BURST_MIN_SOLVES distinct challenges solved within BURST_WINDOW_SECS —
    // automated submission or shared flags entered in one go (RSCTF Check 8).
    if let Some(burst_observed_at) = earliest_burst_completion(
        participant_solves
            .iter()
            .map(|solve| solve.submit_time_utc)
            .collect(),
    ) {
        record_with_dedup_at(
            db,
            game_id,
            participation_id,
            None,
            SuspicionType::Burst,
            GLOBAL_EVIDENCE_KEY,
            burst_observed_at,
            &mut codes,
        )
        .await?;
    }

    // WrongFlagLeakage is intentionally not reconstructed from mutable live
    // flags. Confirmed foreign flags are graded into immutable CheatInfo and
    // replayed above as StolenFlag; legacy wrong answers remain telemetry.
    // ── Rule: Hoarding ───────────────────────────────────────────────────────
    // A canonical solve long after the submit-time instance's last operation,
    // when the immutable snapshot proves it was unloaded with no container.
    // Legacy NULL snapshots emit nothing; replay never reads mutable instances.
    if challenge_type.is_container()
        && earliest_accept_here == Some(submission_time)
        && is_hoarded_submission(
            submission_time,
            container_id.is_some(),
            container_last_operation,
            container_was_loaded,
        )
    {
        let evidence_key = challenge_evidence_key(challenge_id);
        record_with_dedup_at(
            db,
            game_id,
            participation_id,
            Some(challenge_id),
            SuspicionType::Hoarding,
            &evidence_key,
            submission_time,
            &mut codes,
        )
        .await?;
    }

    Ok(codes)
}

/// Replay submission-derived suspicion evaluation from durable database
/// identity. The outbox uses this narrow loader after a restart; callers do not
/// need to retain request-local challenge or answer objects.
pub async fn evaluate_submission_by_id(
    db: &DatabaseConnection,
    submission_id: i32,
) -> AppResult<Vec<i16>> {
    let row: Option<(i32, i32, i32, i16)> = sqlx::query_as(
        r#"SELECT submission.game_id,
                  submission.participation_id,
                  submission.challenge_id,
                  challenge."Type"
             FROM "Submissions" submission
             JOIN "GameChallenges" challenge
               ON challenge.id = submission.challenge_id
              AND challenge.game_id = submission.game_id
            WHERE submission.id = $1"#,
    )
    .bind(submission_id)
    .fetch_optional(db.get_postgres_connection_pool())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((game_id, participation_id, challenge_id, challenge_type)) = row else {
        return Err(AppError::not_found("submission not found"));
    };
    let challenge_type =
        <crate::utils::enums::ChallengeType as sea_orm::ActiveEnum>::try_from_value(
            &challenge_type,
        )
        .map_err(|error| AppError::internal(error.to_string()))?;
    evaluate_submission_inner(
        db,
        game_id,
        participation_id,
        submission_id,
        challenge_id,
        challenge_type,
    )
    .await
}

#[cfg(test)]
#[path = "detectors_tests.rs"]
mod tests;
