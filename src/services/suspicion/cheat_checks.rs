//! Abnormal-solve cheat checks — the submission-pattern half of RSCTF
//! `Controllers/CheatReportController.cs`, ported as a whole-game sweep.
//!
//! RSCTF's `CheatReportController.Get` rebuilds the entire cheat report on every
//! monitor request, running a battery of per-submission "abnormal solve" checks
//! against canonical accepted submissions, wrong submissions, and immutable
//! grading-time container snapshots. This
//! module ports the *submission-pattern* subset of those checks and persists a
//! `suspicion_event` for each one that fires, reusing the exact dedup + insert +
//! score path ([`super::detectors::record_with_dedup_at`]) the per-submission live
//! detector uses. Each check therefore fires at most once per participation,
//! rule, and stable global/challenge evidence key.
//!
//! Checks implemented here, with their RSCTF `CheatReportController` origin and
//! exact thresholds:
//!
//! | Rule | RSCTF check | Threshold |
//! | --- | --- | --- |
//! | `FastSolveOpen` / `Download` / `Container` | 7a-c | disabled: source clocks and asynchronous event commits are not strong evidence |
//! | `NoDownload` | 4 | disabled: absence of best-effort telemetry is not evidence |
//! | `NoContainer` | 5 | disabled: absence of best-effort telemetry is not evidence |
//! | [`SuspicionType::Hoarding`] | 6 | immutable submit snapshot shows an unloaded instance with no container for `> 60min` |
//! | [`SuspicionType::ZeroWrongAttempts`] | A | dynamic, not-easy, `solveCount >= 5`, zero wrong before solve |
//! | [`SuspicionType::HighWrongRate`] | H1 | `>= 40` wrong within a 60s window (unless solved within 5min) |
//! | [`SuspicionType::AutomatedPattern`] | H2 | `>= 10` consecutive wrong intervals `< 2s` |
//! | [`SuspicionType::FirstBloodAnomaly`] | J | first blood whose 2nd solve is `2+ hours` later |
//!
//! Every evidence source is fenced to the game's competitive `[start, end)`
//! interval, so post-game practice cannot rewrite the concluded event's audit
//! record. Network/identity context checks live in [`super::correlation`].

use std::collections::HashMap;

use chrono::{DateTime, Utc};

use super::*;
use crate::app_state::SharedState;

// ── Thresholds (mirrors the RSCTF constants) ────────────────────────────────
/// Check A gate: challenge must have at least this many accepted solves.
const ZERO_WRONG_MIN_SOLVE_COUNT: usize = 5;
/// H2 (`AutoSpeedCount`): consecutive sub-2s intervals that trip AutomatedPattern.
const AUTO_SPEED_COUNT: usize = 10;
/// H2 interval ceiling (`< 2.0s`), in milliseconds.
const AUTO_SPEED_INTERVAL_MS: i64 = 2 * 1000;
/// H group floor: only (team,challenge) wrong-groups with `>= 5` wrongs are
/// considered (RSCTF `.Where(g => g.Count() >= 5)`).
const H_GROUP_MIN_WRONGS: usize = 5;
/// FirstBloodAnomaly gap (`TimeSpan.FromHours(2)`), in milliseconds.
const FIRST_BLOOD_GAP_MS: i64 = 2 * 60 * 60 * 1000;
const AUTOMATED_PATTERN_CONTEXT_ROWS: i64 = (AUTO_SPEED_COUNT + 1) as i64;
const MAX_INCREMENTAL_SUBMISSION_DELTAS: i64 = super::reconciliation::SOURCE_BATCH;

const INCREMENTAL_AUTOMATED_PATTERN_SQL: &str = r#"
    WITH changed_wrong AS MATERIALIZED (
        SELECT job.id AS job_id, submission.id AS submission_id,
               submission.participation_id, submission.challenge_id,
               submission.submit_time_utc
          FROM "SuspicionEvaluationOutbox" job
          JOIN "Submissions" submission
            ON submission.id = job.source_id
           AND submission.game_id = job.game_id
           AND submission.participation_id = job.participation_id
           AND submission.challenge_id = job.challenge_id
          JOIN "Games" game ON game.id = job.game_id
          JOIN "Participations" participation
            ON participation.id = submission.participation_id
           AND participation.game_id = submission.game_id
         WHERE job.game_id = $1
           AND job.reconciliation_version > $2
           AND job.reconciliation_version <= $3
           AND job.completed_at_utc IS NOT NULL
           AND job.job_kind = 0
           AND job.challenge_id IS NOT NULL
           AND submission.status = $4
           AND job.observed_at_utc >= game.start_time_utc
           AND job.observed_at_utc < game.end_time_utc
           AND participation.competitive_admitted_at_utc IS NOT NULL
           AND participation.competitive_admitted_at_utc < game.end_time_utc
         ORDER BY job.reconciliation_version
         LIMIT $6
    ), bounded_windows AS MATERIALIZED (
        SELECT changed.job_id, changed.participation_id,
               changed.challenge_id, context.id,
               context.submit_time_utc
          FROM changed_wrong changed
          JOIN LATERAL (
              SELECT submission.id, submission.submit_time_utc
                FROM "Submissions" submission
                JOIN "Games" game ON game.id = submission.game_id
               WHERE submission.game_id = $1
                 AND submission.participation_id = changed.participation_id
                 AND submission.challenge_id = changed.challenge_id
                 AND submission.status = $4
                 AND (submission.submit_time_utc, submission.id)
                       <= (changed.submit_time_utc, changed.submission_id)
                 AND submission.submit_time_utc >= game.start_time_utc
                 AND submission.submit_time_utc < game.end_time_utc
               ORDER BY submission.submit_time_utc DESC, submission.id DESC
               LIMIT $5
          ) context ON TRUE
    ), intervals AS (
        SELECT bounded.*,
               bounded.submit_time_utc - LAG(bounded.submit_time_utc) OVER (
                   PARTITION BY bounded.job_id
                   ORDER BY bounded.submit_time_utc, bounded.id
               ) AS cadence_interval
          FROM bounded_windows bounded
    ), qualifying_windows AS (
        SELECT job_id, participation_id, challenge_id,
               MAX(submit_time_utc) AS observed_at
          FROM intervals
         GROUP BY job_id, participation_id, challenge_id
        HAVING COUNT(*) FILTER (
                   WHERE cadence_interval >= INTERVAL '0 seconds'
                     AND cadence_interval < INTERVAL '2 seconds'
               ) >= 10
    )
    SELECT participation_id, challenge_id, MIN(observed_at)
      FROM qualifying_windows
     GROUP BY participation_id, challenge_id
     ORDER BY participation_id, challenge_id
"#;

fn zero_wrong_snapshot_is_final(snapshot: super::detectors::ReconciliationSnapshot) -> bool {
    super::detectors::final_snapshot_ready(snapshot)
}

fn first_blood_anomaly_observed_at(mut solve_times: Vec<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    if solve_times.len() < 2 {
        return None;
    }
    solve_times.sort_unstable();
    (solve_times[1] - solve_times[0] >= chrono::Duration::milliseconds(FIRST_BLOOD_GAP_MS))
        .then_some(solve_times[1])
}

/// Run the whole-game abnormal-solve cheat sweep, persisting a `suspicion_event`
/// and rebuilding the score projection for every check that fires.
///
/// Ported from RSCTF `CheatReportController.Get`'s abnormal-solve battery. The
/// per-check results are not returned — RSCTF surfaces them as
/// `CheatReport.abnormalSolves`, which the monitor endpoint rebuilds from the
/// persisted events. See the TODO at the end for populating that field directly.
pub async fn run_abnormal_solve_checks(st: &SharedState, game_id: i32) -> AppResult<()> {
    run_abnormal_solve_checks_for_snapshot(
        st,
        game_id,
        super::detectors::ReconciliationSnapshot::Live,
    )
    .await
}

/// Evaluate only the cadence groups touched by a bounded completed-outbox
/// slice. Stolen-flag, burst, high-wrong-rate, and hoarding rules already run
/// once per durable submission job; this SQL aggregate supplies the one live
/// abnormal-solve rule that needs a sequence of wrong attempts.
pub(crate) async fn run_abnormal_solve_checks_incremental(
    st: &SharedState,
    game_id: i32,
    cursor: super::reconciliation::SourceCursor,
) -> AppResult<()> {
    if cursor.after >= cursor.through {
        return Ok(());
    }
    let hits: Vec<(i32, i32, DateTime<Utc>)> = sqlx::query_as(INCREMENTAL_AUTOMATED_PATTERN_SQL)
        .bind(game_id)
        .bind(cursor.after)
        .bind(cursor.through)
        .bind(crate::utils::enums::AnswerResult::WrongAnswer as i16)
        .bind(AUTOMATED_PATTERN_CONTEXT_ROWS)
        .bind(MAX_INCREMENTAL_SUBMISSION_DELTAS)
        .fetch_all(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let mut codes = Vec::new();
    for (participation_id, challenge_id, observed_at) in hits {
        super::detectors::record_with_dedup_at(
            &st.db,
            game_id,
            participation_id,
            Some(challenge_id),
            SuspicionType::AutomatedPattern,
            &challenge_evidence_key(challenge_id),
            observed_at,
            &mut codes,
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn run_abnormal_solve_checks_for_snapshot(
    st: &SharedState,
    game_id: i32,
    snapshot: super::detectors::ReconciliationSnapshot,
) -> AppResult<()> {
    let Some(window) = super::detectors::load_competitive_game_window(st.pg(), game_id).await?
    else {
        return Ok(());
    };
    let final_snapshot_authorized = zero_wrong_snapshot_is_final(snapshot);

    // ── Data gathering ──────────────────────────────────────────────────────
    let challenge_rows: Vec<(i32, i16)> = sqlx::query_as(
        r#"SELECT id, "Type"
             FROM "GameChallenges"
            WHERE game_id = $1"#,
    )
    .bind(game_id)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if challenge_rows.is_empty() {
        return Ok(());
    }
    let mut challenge_map = HashMap::with_capacity(challenge_rows.len());
    for (challenge_id, challenge_type) in challenge_rows {
        let challenge_type =
            <crate::utils::enums::ChallengeType as sea_orm::ActiveEnum>::try_from_value(
                &challenge_type,
            )
            .map_err(|error| AppError::internal(error.to_string()))?;
        challenge_map.insert(challenge_id, challenge_type);
    }

    // The immutable pre-end admitted cohort is the participating-team
    // denominator. Later status changes and post-end practice joins cannot
    // rewrite it.
    let team_participating_count: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)
             FROM "Participations"
            WHERE game_id = $1
              AND competitive_admitted_at_utc IS NOT NULL
              AND competitive_admitted_at_utc < $2"#,
    )
    .bind(game_id)
    .bind(window.end)
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let team_participating_count = usize::try_from(team_participating_count)
        .map_err(|_| AppError::internal("participation count exceeds usize"))?;

    // Canonical first solves prevent correct-flag replay from inflating solver
    // counts, medians, or blood order. Wrong attempts retain every competitive
    // incident because cadence is the signal for H1/H2.
    let accepted = super::detectors::load_canonical_solves(st.pg(), game_id, window).await?;
    let wrong = super::detectors::load_competitive_wrong_attempts(st.pg(), game_id, window).await?;

    // ── Wrong-submission index + easy-challenge precomputes ──────────────────
    // (team, challenge) -> wrong submissions, ascending by time.
    let mut wrong_by_tc: HashMap<(i32, i32), Vec<super::detectors::CompetitiveWrongAttempt>> =
        HashMap::new();
    for w in &wrong {
        wrong_by_tc
            .entry((w.team_id, w.challenge_id))
            .or_default()
            .push(w.clone());
    }
    for v in wrong_by_tc.values_mut() {
        v.sort_by_key(|s| s.submit_time_utc);
    }

    // challenge -> accepted submissions (solver base for Check A / easy / J).
    let mut accepted_by_chal: HashMap<i32, Vec<super::detectors::CanonicalSolve>> = HashMap::new();
    for s in &accepted {
        accepted_by_chal
            .entry(s.challenge_id)
            .or_default()
            .push(s.clone());
    }
    // RSCTF challengeSolveCount = count of accepted submissions per challenge.
    let solve_count =
        |cid: i32| -> usize { accepted_by_chal.get(&cid).map(|v| v.len()).unwrap_or(0) };

    // zeroAttemptRatePerChallenge: fraction of solvers with no wrong before solve.
    let zero_attempt_rate = |cid: i32| -> f64 {
        let Some(solvers) = accepted_by_chal.get(&cid) else {
            return 0.0;
        };
        if solvers.is_empty() {
            return 0.0;
        }
        let zero = solvers
            .iter()
            .filter(|s| {
                wrong_by_tc
                    .get(&(s.team_id, cid))
                    .map(|ws| !ws.iter().any(|w| w.submit_time_utc < s.submit_time_utc))
                    .unwrap_or(true)
            })
            .count();
        zero as f64 / solvers.len() as f64
    };

    // IsChallengeEasy (RSCTF 714-717).
    let is_challenge_easy = |cid: i32| -> bool {
        super::detectors::is_easy_challenge(
            solve_count(cid),
            team_participating_count,
            zero_attempt_rate(cid),
        )
    };

    // Single shared out-param for record_with_dedup (DB dedup makes this idempotent).
    let mut codes: Vec<i16> = Vec::new();

    // Helper to fire a rule for a participation.
    macro_rules! fire_at {
        ($pid:expr, $cid:expr, $ty:expr, $observed_at:expr) => {{
            let challenge_id = $cid;
            let evidence_key = challenge_id
                .map(challenge_evidence_key)
                .unwrap_or_else(|| GLOBAL_EVIDENCE_KEY.to_string());
            super::detectors::record_with_dedup_at(
                &st.db,
                game_id,
                $pid,
                challenge_id,
                $ty,
                &evidence_key,
                $observed_at,
                &mut codes,
            )
            .await?;
        }};
    }

    // ── Per-accepted-submission checks (4, 5, 6, 7a-c, A) ────────────────────
    for sub in &accepted {
        let Some(challenge_type) = challenge_map.get(&sub.challenge_id) else {
            continue;
        };
        let pid = sub.participation_id;
        let cid = sub.challenge_id;
        let key = (sub.team_id, cid);
        let solve_t = sub.submit_time_utc;

        let is_container = challenge_type.is_container();

        // NoDownload / NoContainer intentionally do not emit suspicion. Their
        // source actions are best-effort audit rows; absence of lossy telemetry
        // is not affirmative evidence.

        // Hoarding is derived only from the canonical solve's immutable
        // grading-time container snapshot. Legacy rows without both snapshot
        // fields stay unclassified; delayed sweeps never reinterpret mutable
        // GameInstances or best-effort event telemetry.
        if is_container
            && super::detectors::is_hoarded_submission(
                solve_t,
                sub.container_id.is_some(),
                sub.container_last_operation_at_submit,
                sub.container_was_loaded_at_submit,
            )
        {
            fire_at!(pid, Some(cid), SuspicionType::Hoarding, solve_t);
        }

        // FastSolve kinds 12-14 also intentionally do not emit. Even a frozen
        // positive snapshot inherits application-clock and asynchronous event
        // ordering ambiguity, so it remains raw audit telemetry rather than a
        // durable suspicion incident.

        // Check A: ZeroWrongAttempts — dynamic, not-easy, real solver base, and no
        // wrong submissions before the solve.
        if final_snapshot_authorized
            && challenge_type.is_dynamic()
            && !is_challenge_easy(cid)
            && solve_count(cid) >= ZERO_WRONG_MIN_SOLVE_COUNT
        {
            let wrongs_before = wrong_by_tc
                .get(&key)
                .map(|ws| ws.iter().filter(|w| w.submit_time_utc < solve_t).count())
                .unwrap_or(0);
            if wrongs_before == 0 {
                fire_at!(pid, Some(cid), SuspicionType::ZeroWrongAttempts, solve_t);
            }
        }
    }

    // ── Check H1: HighWrongRate ──────────────────────────────────────────────
    // Shared verbatim with the live submission evaluator, including canonical
    // solve suppression and the challenge evidence identity.
    for (pid, cid, observed_at) in
        super::detectors::high_wrong_rate_hits(st.pg(), game_id, window, None).await?
    {
        super::detectors::record_high_wrong_rate_with_dedup(
            &st.db,
            game_id,
            pid,
            cid,
            window,
            observed_at,
            &mut codes,
        )
        .await?;
    }

    // ── Check H2: AutomatedPattern ───────────────────────────────────────────
    // Only (team,challenge) wrong-groups with >= 5 wrongs are considered.
    for ((_team_id, cid), wrongs) in &wrong_by_tc {
        if wrongs.len() < H_GROUP_MIN_WRONGS {
            continue;
        }
        // participation id for this (team, challenge): any wrong submission carries it.
        let pid = wrongs[0].participation_id;

        // H2: >= 10 consecutive intervals under 2s.
        if wrongs.len() > AUTO_SPEED_COUNT {
            let mut machine = 0usize;
            for pair in wrongs.windows(2) {
                let iv = (pair[1].submit_time_utc - pair[0].submit_time_utc).num_milliseconds();
                if (0..AUTO_SPEED_INTERVAL_MS).contains(&iv) {
                    machine += 1;
                } else {
                    machine = 0;
                }
                if machine >= AUTO_SPEED_COUNT {
                    fire_at!(
                        pid,
                        Some(*cid),
                        SuspicionType::AutomatedPattern,
                        pair[1].submit_time_utc
                    );
                    break;
                }
            }
        }
    }

    // ── Check J: FirstBloodAnomaly ───────────────────────────────────────────
    // First blood whose second solve is 2+ hours later.
    if final_snapshot_authorized {
        for (cid, solves) in &accepted_by_chal {
            let Some(observed_at) = first_blood_anomaly_observed_at(
                solves.iter().map(|solve| solve.submit_time_utc).collect(),
            ) else {
                continue;
            };
            let first_blood = solves
                .iter()
                .min_by_key(|solve| solve.submit_time_utc)
                .expect("first-blood anomaly requires two solves");
            fire_at!(
                first_blood.participation_id,
                Some(*cid),
                SuspicionType::FirstBloodAnomaly,
                observed_at
            );
        }
    }

    // TODO(cheat-report): RSCTF also returns each fired check as a
    // `CheatReport.abnormalSolves` row (team/challenge/type/time/details). The
    // monitor `cheat_report` endpoint currently rebuilds its lists from the
    // persisted `suspicion_event` rows this sweep writes; if a richly-detailed
    // abnormalSolves payload is wanted, collect the fired `(pid, cid, ty)` tuples
    // above into a returned Vec and shape them there.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        first_blood_anomaly_observed_at, zero_wrong_snapshot_is_final,
        INCREMENTAL_AUTOMATED_PATTERN_SQL,
    };
    use crate::services::suspicion::detectors::ReconciliationSnapshot;

    #[test]
    fn zero_wrong_community_suppression_is_final_only() {
        assert!(!zero_wrong_snapshot_is_final(ReconciliationSnapshot::Live));
        assert!(zero_wrong_snapshot_is_final(
            ReconciliationSnapshot::BarrierBackedFinal
        ));
    }

    #[test]
    fn first_blood_is_final_and_observed_at_the_qualifying_second_solve() {
        let first = chrono::Utc::now();
        let exact_threshold = first + chrono::Duration::hours(2);
        assert_eq!(
            first_blood_anomaly_observed_at(vec![exact_threshold, first]),
            Some(exact_threshold)
        );
        assert_eq!(
            first_blood_anomaly_observed_at(vec![
                first + chrono::Duration::hours(3),
                first,
                first + chrono::Duration::hours(1),
            ]),
            None,
            "a late-arriving canonical solve between the first two suppresses the anomaly"
        );
        assert_eq!(
            first_blood_anomaly_observed_at(vec![
                first,
                exact_threshold - chrono::Duration::milliseconds(1),
            ]),
            None
        );
    }

    #[test]
    fn ambiguous_fast_solve_telemetry_has_no_incident_emitter() {
        let actionable_reference = ["SuspicionType", "::FastSolve"].concat();
        assert!(!include_str!("cheat_checks.rs").contains(&actionable_reference));
    }

    #[test]
    fn live_cadence_is_delta_nominated_and_database_aggregated() {
        assert!(INCREMENTAL_AUTOMATED_PATTERN_SQL.contains("job.reconciliation_version > $2"));
        assert!(INCREMENTAL_AUTOMATED_PATTERN_SQL.contains("changed_wrong AS MATERIALIZED"));
        assert!(INCREMENTAL_AUTOMATED_PATTERN_SQL.contains("JOIN LATERAL"));
        assert!(INCREMENTAL_AUTOMATED_PATTERN_SQL.contains("LIMIT $5"));
        assert!(INCREMENTAL_AUTOMATED_PATTERN_SQL.contains("LIMIT $6"));
        assert!(INCREMENTAL_AUTOMATED_PATTERN_SQL
            .contains("<= (changed.submit_time_utc, changed.submission_id)"));
        assert!(INCREMENTAL_AUTOMATED_PATTERN_SQL.contains("HAVING COUNT(*) FILTER"));
        assert!(!INCREMENTAL_AUTOMATED_PATTERN_SQL.contains("changed_groups"));
        assert!(!INCREMENTAL_AUTOMATED_PATTERN_SQL.contains("SELECT submission.*"));
    }
}
