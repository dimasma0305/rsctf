//! services/suspicion/cheat_stat.rs — cross-team **statistical** cheat detectors.
//!
//! Ported from RSCTF `Controllers/CheatReportController.cs`. Where the
//! per-submission detectors in [`super::detectors`] look at one team in
//! isolation, these run **pairwise / community-relative** analyses over every
//! participation in a game at once and are meant to be driven periodically by
//! the cheat-report sweep (`run_statistical_checks(&st, game_id)`), not on the
//! hot submission path.
//!
//! RSCTF works in `TeamId` space; each team has exactly one `Participation` per
//! game, so we key everything on `participation_id` (the unit the
//! `suspicion_event` audit table + scoring model use) and map `GameEvent.TeamId`
//! back through the participation table. Every fired signal is persisted with the
//! shared timestamp-aware dedup path, using stable challenge, pair, or source
//! identities and the same ledger/projection logic as the behavioral rules.
//!
//! Detectors (each cites the RSCTF source range it mirrors):
//! * **SequenceSimilarity** (Check 3, `cs:1183-1290`) — RSI =
//!   `0.7·Jaccard(solved sets) + 0.3·(LCS(solve order)/min len)` over informative
//!   (not commonly solved) challenges; flags **both** teams when `RSI >= 0.85`.
//!   PostgreSQL's inverted challenge index generates only pairs sharing at least
//!   three informative solves, avoiding both an all-pairs scan and the old
//!   top-50 blind spot.
//! * **SolutionRelay** (Check C, `cs:1292-1356`) — constant-lag temporal relay:
//!   per shared informative challenge the receiver's solve minus the source's,
//!   flagged when `mean ∈ [2,30]` min, population `stddev < 5` min, and coverage
//!   `>= 60%` (`>= 6` lags). Recorded against the receiver in each direction and
//!   evaluated independently of RSI.
//! * **AdaptiveFastSolve** (Check D, `cs:1141-1180`) — a solve at `< 5%` of the
//!   community **median** solve offset, only when that median `> 60` min, gated
//!   on `>= 8` community solves and the `IsChallengeEasy` + fast-cohort (`>= 3`
//!   other teams under 15% of median) suppression guards.
//! * **DirectedSolving** (Check E, `cs:1455-1522`) is telemetry-only and does
//!   not emit: `ChallengeOpened` is best-effort, so a missing audit row cannot
//!   prove that a team opened only the challenges it solved.

use super::*;
use crate::app_state::SharedState;
use std::collections::{BTreeMap, HashMap, HashSet};

// ─────────────────────────────────────────────────────────────────────────────
// Small numeric helpers (byte-for-byte with the C# they mirror)
// ─────────────────────────────────────────────────────────────────────────────

/// Fractional minutes between two instants — mirrors C# `TimeSpan.TotalMinutes`
/// (a `double`). RSCTF filters/averages lags and offsets on the *fractional*
/// value, so truncating to whole minutes (`num_minutes()`) would silently drift
/// the `>1 && <=60` lag filter and the relay mean/stddev.
fn minutes_between(from: chrono::DateTime<chrono::Utc>, to: chrono::DateTime<chrono::Utc>) -> f64 {
    (to - from).num_milliseconds() as f64 / 60_000.0
}

/// Length of the longest common subsequence of two challenge-id sequences
/// (mirrors RSCTF `GetLongestCommonSubsequence`, rolling one-row DP). Kept local
/// rather than borrowed from `controllers::game::cheat` to avoid a
/// controller→service layering inversion.
fn lcs_len(a: &[i32], b: &[i32]) -> usize {
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    let m = b.len();
    let mut dp = vec![0usize; m + 1];
    for &x in a {
        let mut prev = 0usize;
        for j in 0..m {
            let tmp = dp[j + 1];
            dp[j + 1] = if x == b[j] {
                prev + 1
            } else {
                dp[j + 1].max(dp[j])
            };
            prev = tmp;
        }
    }
    dp[m]
}

/// *True* median (even count → mean of the two middles). Used by Check D's
/// `challengeMedianSolveOffset`. Distinct from Check E's plain `sorted[len/2]`
/// index — the two RSCTF median conventions must **not** be unified.
fn true_median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    let mid = n / 2;
    if n.is_multiple_of(2) {
        (v[mid - 1] + v[mid]) / 2.0
    } else {
        v[mid]
    }
}

/// Candidate generation for cross-team correlation. Joining the canonical
/// projection by challenge lets PostgreSQL use `ix_firstsolves_challenge` and
/// emits only pairs with enough shared informative evidence; Rust never walks
/// the quadratic Cartesian product of all participating teams.
const COLLABORATION_CANDIDATES_SQL: &str = r#"
    WITH canonical AS MATERIALIZED (
        SELECT first_solve.participation_id,
               first_solve.challenge_id
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
           AND first_solve.challenge_id = ANY($5)
           AND participation.competitive_admitted_at_utc IS NOT NULL
           AND participation.competitive_admitted_at_utc < $4
    )
    SELECT source.participation_id, receiver.participation_id
      FROM canonical source
      JOIN canonical receiver
        ON receiver.challenge_id = source.challenge_id
       AND receiver.participation_id > source.participation_id
     GROUP BY source.participation_id, receiver.participation_id
    HAVING COUNT(*) >= 3
     ORDER BY source.participation_id, receiver.participation_id
"#;

pub(super) async fn collaboration_candidates(
    pool: &sqlx::PgPool,
    game_id: i32,
    window: super::detectors::CompetitiveGameWindow,
    informative_challenge_ids: &[i32],
) -> AppResult<Vec<(i32, i32)>> {
    if informative_challenge_ids.len() < 3 {
        return Ok(Vec::new());
    }

    sqlx::query_as(COLLABORATION_CANDIDATES_SQL)
        .bind(game_id)
        .bind(crate::utils::enums::AnswerResult::Accepted as i16)
        .bind(window.start)
        .bind(window.end)
        .bind(informative_challenge_ids)
        .fetch_all(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

fn is_relay_pattern(lags: &[f64], shared_count: usize) -> bool {
    if lags.len() < 6 || shared_count == 0 || (lags.len() as f64 / shared_count as f64) < 0.60 {
        return false;
    }

    let mean = lags.iter().sum::<f64>() / lags.len() as f64;
    if !(2.0..=30.0).contains(&mean) {
        return false;
    }
    let variance = lags
        .iter()
        .map(|lag| {
            let distance = lag - mean;
            distance * distance
        })
        .sum::<f64>()
        / lags.len() as f64;
    variance.sqrt() < 5.0
}

fn directed_solving_is_actionable() -> bool {
    false
}

fn statistical_snapshot_is_final(snapshot: super::detectors::ReconciliationSnapshot) -> bool {
    super::detectors::final_snapshot_ready(snapshot)
}

/// Persist one statistical signal via the shared dedup+score path (a throwaway
/// `codes` vec — the return codes matter only on the per-submission path).
async fn record(
    db: &sea_orm::DatabaseConnection,
    game_id: i32,
    participation_id: i32,
    challenge_id: Option<i32>,
    ty: SuspicionType,
    observed_at: chrono::DateTime<chrono::Utc>,
) -> AppResult<()> {
    let evidence_key = challenge_id
        .map(challenge_evidence_key)
        .unwrap_or_else(|| GLOBAL_EVIDENCE_KEY.to_string());
    record_with_evidence(
        db,
        game_id,
        participation_id,
        challenge_id,
        ty,
        &evidence_key,
        observed_at,
    )
    .await
}

async fn record_with_evidence(
    db: &sea_orm::DatabaseConnection,
    game_id: i32,
    participation_id: i32,
    challenge_id: Option<i32>,
    ty: SuspicionType,
    evidence_key: &str,
    observed_at: chrono::DateTime<chrono::Utc>,
) -> AppResult<()> {
    let mut codes: Vec<i16> = Vec::new();
    super::detectors::record_with_dedup_at(
        db,
        game_id,
        participation_id,
        challenge_id,
        ty,
        evidence_key,
        observed_at,
        &mut codes,
    )
    .await
}

// ─────────────────────────────────────────────────────────────────────────────
// Entry point
// ─────────────────────────────────────────────────────────────────────────────

/// Run the cross-team statistical cheat checks over an entire game, persisting
/// a `suspicion_event` (deduped by a stable aggregate evidence key) for each team
/// a detector implicates. Ported from the community-relative / pairwise checks
/// of RSCTF `CheatReportController` (Checks 3, C, D, E). Idempotent: safe to run
/// on every sweep.
pub async fn run_statistical_checks(st: &SharedState, game_id: i32) -> AppResult<()> {
    run_statistical_checks_for_snapshot(st, game_id, super::detectors::ReconciliationSnapshot::Live)
        .await
}

pub(super) async fn run_statistical_checks_for_snapshot(
    st: &SharedState,
    game_id: i32,
    snapshot: super::detectors::ReconciliationSnapshot,
) -> AppResult<()> {
    let db = &st.db;

    let Some(window) = super::detectors::load_competitive_game_window(st.pg(), game_id).await?
    else {
        return Ok(());
    };
    // Every rule below is community-relative and therefore non-monotonic while
    // solves/opens can still arrive. SuspicionEvents are immutable, so a live
    // partial snapshot could leave evidence that the final population would
    // suppress. Evaluate once the configured competitive window is closed.
    if !statistical_snapshot_is_final(snapshot) {
        return Ok(());
    }
    let start = window.start;

    // Accepted submissions for the game, time-ordered (drives sequences,
    // per-challenge stats, cohort suppression). All four detectors are
    // accepted-only — wrong submissions feed only the easy-challenge gate below.
    let mut accepted = super::detectors::load_canonical_solves(st.pg(), game_id, window).await?;
    accepted.sort_by_key(|s| s.submit_time_utc);

    // Wrong submissions, keyed (participation, challenge) → times, for the
    // zero-attempt-rate component of `IsChallengeEasy`.
    let wrong = super::detectors::load_competitive_wrong_attempts(st.pg(), game_id, window).await?;
    let mut wrong_by_part_chal: HashMap<(i32, i32), Vec<chrono::DateTime<chrono::Utc>>> =
        HashMap::new();
    for w in &wrong {
        wrong_by_part_chal
            .entry((w.participation_id, w.challenge_id))
            .or_default()
            .push(w.submit_time_utc);
    }

    // The immutable pre-end admitted cohort is the denominator; later status
    // changes and post-end practice joins cannot make common challenges look
    // artificially hard.
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

    // ── Per-challenge community statistics ──────────────────────────────────
    let mut challenge_accepts: BTreeMap<i32, Vec<&super::detectors::CanonicalSolve>> =
        BTreeMap::new();
    for s in &accepted {
        challenge_accepts.entry(s.challenge_id).or_default().push(s);
    }

    let mut challenge_solve_count: HashMap<i32, usize> = HashMap::new();
    let mut challenge_median_offset: HashMap<i32, f64> = HashMap::new();
    let mut zero_attempt_rate: HashMap<i32, f64> = HashMap::new();
    for (&cid, solvers) in &challenge_accepts {
        // g.Count() — counts accepted submissions (unique per team ⇒ team count).
        challenge_solve_count.insert(cid, solvers.len());

        // True median of solve offsets (minutes since game start).
        let offsets: Vec<f64> = solvers
            .iter()
            .map(|s| minutes_between(start, s.submit_time_utc))
            .collect();
        challenge_median_offset.insert(cid, true_median(offsets));

        // Fraction of solvers who had NO wrong attempt before their solve.
        let zero_attempt_solvers = solvers
            .iter()
            .filter(|s| {
                wrong_by_part_chal
                    .get(&(s.participation_id, cid))
                    .map(|ws| !ws.iter().any(|&wt| wt < s.submit_time_utc))
                    .unwrap_or(true)
            })
            .count();
        zero_attempt_rate.insert(cid, zero_attempt_solvers as f64 / solvers.len() as f64);
    }

    // Easy challenge: solve-rate > 40% of participating teams, OR
    // zero-attempt-rate > 30%. Suppresses FP-heavy per-challenge signals.
    let is_easy = |cid: i32| -> bool {
        super::detectors::is_easy_challenge(
            *challenge_solve_count.get(&cid).unwrap_or(&0),
            team_participating_count,
            *zero_attempt_rate.get(&cid).unwrap_or(&0.0),
        )
    };
    // Ordering commonness is deliberately prevalence-only. Reusing the
    // ZeroWrong/AdaptiveFast `zero_attempt_rate` suppression here would let a
    // copied first-try solve sequence label its own challenges easy and erase
    // every collusion candidate.
    let is_common_ordering_challenge = |cid: i32| -> bool {
        super::detectors::is_common_ordering_challenge(
            *challenge_solve_count.get(&cid).unwrap_or(&0),
            team_participating_count,
        )
    };
    let informative_challenge_ids: Vec<i32> = challenge_accepts
        .keys()
        .copied()
        .filter(|challenge_id| !is_common_ordering_challenge(*challenge_id))
        .collect();
    let informative_challenges: HashSet<i32> = informative_challenge_ids.iter().copied().collect();

    // ── Check D: Adaptive Fast Solve ────────────────────────────────────────
    // Per accepted submission: solve offset < 5% of the community median while
    // the median > 60 min (a genuinely hard challenge), gated on >= 8 community
    // solves, not easy, and no >=3-team fast cohort (specialist cluster).
    for sub in &accepted {
        let cid = sub.challenge_id;
        if is_easy(cid) {
            continue;
        }
        if *challenge_solve_count.get(&cid).unwrap_or(&0) < 8 {
            continue;
        }
        let team_offset = minutes_between(start, sub.submit_time_utc);
        let median_offset = *challenge_median_offset.get(&cid).unwrap_or(&0.0);
        if median_offset > 60.0 && team_offset > 0.0 && team_offset < median_offset * 0.05 {
            // Cohort suppression: >= 3 OTHER teams also under 15% of median ⇒
            // legitimate specialist cluster, not a lone outlier.
            let fast_cohort = accepted
                .iter()
                .filter(|s| {
                    s.challenge_id == cid
                        && s.participation_id != sub.participation_id
                        && minutes_between(start, s.submit_time_utc) < median_offset * 0.15
                })
                .count();
            if fast_cohort < 3 {
                record(
                    db,
                    game_id,
                    sub.participation_id,
                    Some(cid),
                    SuspicionType::AdaptiveFastSolve,
                    sub.submit_time_utc,
                )
                .await?;
            }
        }
    }

    // ── Check 3 + Check C: Sequence Similarity & Solution Relay ──────────────
    // Easy/common solves do not meaningfully identify a copied order. Use an
    // inverted-index SQL query to nominate only pairs sharing at least three
    // informative canonical solves; unlike the former Take(50), every team can
    // become a candidate without an O(team²) application scan.
    let mut seq_map: BTreeMap<i32, Vec<&super::detectors::CanonicalSolve>> = BTreeMap::new();
    for solve in &accepted {
        if informative_challenges.contains(&solve.challenge_id) {
            seq_map
                .entry(solve.participation_id)
                .or_default()
                .push(solve);
        }
    }

    let candidates =
        collaboration_candidates(st.pg(), game_id, window, &informative_challenge_ids).await?;
    for (pa, pb) in candidates {
        let (Some(raw_a), Some(raw_b)) = (seq_map.get(&pa), seq_map.get(&pb)) else {
            continue;
        };
        let seq_a: Vec<i32> = raw_a.iter().map(|solve| solve.challenge_id).collect();
        let seq_b: Vec<i32> = raw_b.iter().map(|solve| solve.challenge_id).collect();
        let set_a: HashSet<i32> = seq_a.iter().copied().collect();
        let set_b: HashSet<i32> = seq_b.iter().copied().collect();
        let shared_ids: Vec<i32> = set_a.intersection(&set_b).copied().collect();
        let shared_count = shared_ids.len();
        if shared_count < 3 {
            continue;
        }

        let union_count = set_a.union(&set_b).count();
        let pair_observed_at = raw_a
            .iter()
            .chain(raw_b.iter())
            .filter(|solve| {
                set_a.contains(&solve.challenge_id) && set_b.contains(&solve.challenge_id)
            })
            .map(|solve| solve.submit_time_utc)
            .max()
            .expect("candidate pairs share at least three solves");
        let jaccard = shared_count as f64 / union_count as f64;
        let lcs_score = lcs_len(&seq_a, &seq_b) as f64 / seq_a.len().min(seq_b.len()) as f64;
        let rsi = jaccard * 0.7 + lcs_score * 0.3;
        if rsi >= 0.85 {
            let pair_key = format!("pair:{}:{}", pa.min(pb), pa.max(pb));
            record_with_evidence(
                db,
                game_id,
                pa,
                None,
                SuspicionType::SequenceSimilarity,
                &pair_key,
                pair_observed_at,
            )
            .await?;
            record_with_evidence(
                db,
                game_id,
                pb,
                None,
                SuspicionType::SequenceSimilarity,
                &pair_key,
                pair_observed_at,
            )
            .await?;
        }

        // Relay is directional temporal evidence, not evidence of identical
        // global solve order. Evaluate it for every candidate independently of
        // RSI so unrelated extra solves cannot hide a stable source→receiver lag.
        if shared_count >= 6 {
            let times_a: HashMap<i32, chrono::DateTime<chrono::Utc>> = raw_a
                .iter()
                .map(|solve| (solve.challenge_id, solve.submit_time_utc))
                .collect();
            let times_b: HashMap<i32, chrono::DateTime<chrono::Utc>> = raw_b
                .iter()
                .map(|solve| (solve.challenge_id, solve.submit_time_utc))
                .collect();
            let lags_a_to_b: Vec<f64> = shared_ids
                .iter()
                .filter_map(|challenge_id| {
                    Some(minutes_between(
                        *times_a.get(challenge_id)?,
                        *times_b.get(challenge_id)?,
                    ))
                })
                .filter(|lag| *lag > 1.0 && *lag <= 60.0)
                .collect();
            let lags_b_to_a: Vec<f64> = shared_ids
                .iter()
                .filter_map(|challenge_id| {
                    Some(minutes_between(
                        *times_b.get(challenge_id)?,
                        *times_a.get(challenge_id)?,
                    ))
                })
                .filter(|lag| *lag > 1.0 && *lag <= 60.0)
                .collect();

            if is_relay_pattern(&lags_a_to_b, shared_count) {
                let source_key = format!("source:{pa}");
                record_with_evidence(
                    db,
                    game_id,
                    pb,
                    None,
                    SuspicionType::SolutionRelay,
                    &source_key,
                    pair_observed_at,
                )
                .await?;
            }
            if is_relay_pattern(&lags_b_to_a, shared_count) {
                let source_key = format!("source:{pb}");
                record_with_evidence(
                    db,
                    game_id,
                    pa,
                    None,
                    SuspicionType::SolutionRelay,
                    &source_key,
                    pair_observed_at,
                )
                .await?;
            }
        }
    }

    // DirectedSolving intentionally has no actionable implementation. Raw
    // ChallengeOpened events remain available as telemetry, but their
    // best-effort absence/ratio cannot create immutable suspicion evidence.
    if directed_solving_is_actionable() {
        return Err(AppError::internal(
            "DirectedSolving requires a durable barrier-fenced evidence source",
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        directed_solving_is_actionable, is_relay_pattern, lcs_len, statistical_snapshot_is_final,
        true_median, COLLABORATION_CANDIDATES_SQL,
    };
    use crate::services::suspicion::detectors::{
        is_common_ordering_challenge, is_easy_challenge, ReconciliationSnapshot,
    };

    #[test]
    fn sequence_candidates_are_canonical_indexed_and_unbounded() {
        assert!(COLLABORATION_CANDIDATES_SQL.contains("\"FirstSolves\""));
        assert!(COLLABORATION_CANDIDATES_SQL.contains("first_solve.challenge_id = ANY($5)"));
        assert!(COLLABORATION_CANDIDATES_SQL.contains("HAVING COUNT(*) >= 3"));
        assert!(!COLLABORATION_CANDIDATES_SQL.contains("LIMIT 50"));
    }

    #[test]
    fn directed_solving_is_telemetry_only() {
        assert!(!directed_solving_is_actionable());
    }

    #[test]
    fn ordering_commonness_is_prevalence_only_and_strict() {
        assert!(!is_common_ordering_challenge(0, 0));
        assert!(!is_common_ordering_challenge(4, 10));
        assert!(is_common_ordering_challenge(5, 10));
        assert!(!is_common_ordering_challenge(1, 10));
        assert!(is_easy_challenge(1, 10, 0.31));
    }

    #[test]
    fn lcs_and_true_median_preserve_order_statistics() {
        assert_eq!(lcs_len(&[1, 2, 3, 4], &[1, 3, 2, 4]), 3);
        assert_eq!(true_median(vec![4.0, 1.0, 3.0, 2.0]), 2.5);
        assert_eq!(true_median(vec![9.0, 1.0, 5.0]), 5.0);
    }

    #[test]
    fn relay_is_directional_stable_and_independent_of_sequence_rsi() {
        assert!(is_relay_pattern(&[8.0, 8.5, 9.0, 9.5, 10.0, 10.5], 8));
        assert!(!is_relay_pattern(&[8.0, 8.5, 9.0, 9.5, 10.0, 10.5], 11));
        assert!(!is_relay_pattern(&[2.0, 8.0, 14.0, 20.0, 26.0, 30.0], 6));
        assert!(!is_relay_pattern(
            &[-8.0, -8.5, -9.0, -9.5, -10.0, -10.5],
            6
        ));
    }

    #[test]
    fn community_relative_statistics_are_final_only() {
        assert!(!statistical_snapshot_is_final(ReconciliationSnapshot::Live));
        assert!(statistical_snapshot_is_final(
            ReconciliationSnapshot::BarrierBackedFinal
        ));
    }
}
