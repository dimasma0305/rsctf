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
//! shared `record_with_dedup` insert path (one event per stable global or
//! challenge key), reusing the exact weight/score-bump logic the
//! behavioral rules use.
//!
//! Detectors (each cites the RSCTF source range it mirrors):
//! * **SequenceSimilarity** (Check 3, `cs:1183-1290`) — pairwise RSI =
//!   `0.7·Jaccard(solved sets) + 0.3·(LCS(solve order)/min len)`; flags **both**
//!   teams when `RSI >= 0.85`.
//! * **SolutionRelay** (Check C, `cs:1292-1356`) — nested inside the
//!   SequenceSimilarity pair loop (so it only ever examines pairs that already
//!   cleared `RSI >= 0.85`); constant-lag temporal relay: per shared challenge
//!   the receiver's solve minus the source's, flagged when `mean ∈ [2,30]` min,
//!   population `stddev < 5` min, and coverage `>= 60%` of shared challenges
//!   (`>= 6` lags). Recorded against the **receiver** in each direction.
//! * **AdaptiveFastSolve** (Check D, `cs:1141-1180`) — a solve at `< 5%` of the
//!   community **median** solve offset, only when that median `> 60` min, gated
//!   on `>= 8` community solves and the `IsChallengeEasy` + fast-cohort (`>= 3`
//!   other teams under 15% of median) suppression guards.
//! * **DirectedSolving** (Check E, `cs:1455-1522`) — a team whose
//!   opened/solved-challenge ratio is `< 1.05` (`>= 8` solves) while the
//!   community median ratio is `>= 1.5` (they open only what they solve).

use super::*;
use crate::app_state::SharedState;
use crate::models::data::{game, game_event, submission};
use crate::utils::enums::{AnswerResult, EventType};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
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

/// Persist one statistical signal via the shared dedup+score path (a throwaway
/// `codes` vec — the return codes matter only on the per-submission path).
async fn record(
    db: &sea_orm::DatabaseConnection,
    game_id: i32,
    participation_id: i32,
    challenge_id: Option<i32>,
    ty: SuspicionType,
) -> AppResult<()> {
    let mut codes: Vec<i16> = Vec::new();
    let evidence_key = challenge_id
        .map(challenge_evidence_key)
        .unwrap_or_else(|| GLOBAL_EVIDENCE_KEY.to_string());
    super::detectors::record_with_dedup(
        db,
        game_id,
        participation_id,
        challenge_id,
        ty,
        &evidence_key,
        &mut codes,
    )
    .await
}

/// Check C `ReportRelay`: flag `receiver_pid` for `SolutionRelay` when `lags`
/// (fractional-minute gaps, already filtered to `(1, 60]`) form a constant-lag
/// relay across `>= 60%` of `shared_count` shared challenges.
async fn report_relay(
    db: &sea_orm::DatabaseConnection,
    game_id: i32,
    lags: &[f64],
    shared_count: usize,
    receiver_pid: i32,
) -> AppResult<()> {
    if lags.len() < 6 {
        return Ok(());
    }
    // Coverage denominator is the DISTINCT shared-challenge count, not the lag
    // count (relay sharing spans all challenges; coincidence clusters locally).
    let coverage = lags.len() as f64 / shared_count as f64;
    if coverage < 0.60 {
        return Ok(());
    }
    let mean = lags.iter().sum::<f64>() / lags.len() as f64;
    // Population stddev (÷ N), matching C# `.Select(..).Average()` on the
    // squared deviations.
    let variance = lags.iter().map(|l| (l - mean) * (l - mean)).sum::<f64>() / lags.len() as f64;
    let stddev = variance.sqrt();
    // Constant-lag relay: mean 2–30 min, stddev < 5 min.
    if !(2.0..=30.0).contains(&mean) || stddev >= 5.0 {
        return Ok(());
    }
    record(
        db,
        game_id,
        receiver_pid,
        None,
        SuspicionType::SolutionRelay,
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
    let db = &st.db;

    let Some(game) = game::Entity::find_by_id(game_id).one(db).await? else {
        return Ok(());
    };
    let start = game.start_time_utc;

    // Accepted submissions for the game, time-ordered (drives sequences,
    // per-challenge stats, cohort suppression). All four detectors are
    // accepted-only — wrong submissions feed only the easy-challenge gate below.
    let mut accepted = submission::Entity::find()
        .filter(submission::Column::GameId.eq(game_id))
        .filter(submission::Column::Status.eq(AnswerResult::Accepted))
        .all(db)
        .await?;
    accepted.sort_by_key(|s| s.submit_time_utc);

    // Wrong submissions, keyed (participation, challenge) → times, for the
    // zero-attempt-rate component of `IsChallengeEasy`.
    let wrong = submission::Entity::find()
        .filter(submission::Column::GameId.eq(game_id))
        .filter(submission::Column::Status.eq(AnswerResult::WrongAnswer))
        .all(db)
        .await?;
    let mut wrong_by_part_chal: HashMap<(i32, i32), Vec<chrono::DateTime<chrono::Utc>>> =
        HashMap::new();
    for w in &wrong {
        wrong_by_part_chal
            .entry((w.participation_id, w.challenge_id))
            .or_default()
            .push(w.submit_time_utc);
    }

    // Participations in this game = the participating teams. team_id →
    // participation_id lets us map GameEvent.TeamId into participation space.
    let parts = participation::Entity::find()
        .filter(participation::Column::GameId.eq(game_id))
        .all(db)
        .await?;
    let team_to_part: HashMap<i32, i32> = parts.iter().map(|p| (p.team_id, p.id)).collect();
    let team_participating_count = parts.len();

    // ── Per-challenge community statistics ──────────────────────────────────
    let mut challenge_accepts: BTreeMap<i32, Vec<&submission::Model>> = BTreeMap::new();
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
        let by_solve = team_participating_count > 0
            && *challenge_solve_count.get(&cid).unwrap_or(&0) as f64
                / team_participating_count as f64
                > 0.40;
        let by_zero = *zero_attempt_rate.get(&cid).unwrap_or(&0.0) > 0.30;
        by_solve || by_zero
    };

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
                )
                .await?;
            }
        }
    }

    // ── Check 3 + Check C: Sequence Similarity & Solution Relay ──────────────
    // Ordered accepted-solve sequence per participation (accepted is already
    // time-sorted, so each vec is in solve order). Keep those with >= 3 solves,
    // take the 50 longest (tie-broken by participation id for determinism —
    // RSCTF's Take(50) is order-undefined on ties, benign unless a game has
    // >50 teams).
    let mut seq_map: BTreeMap<i32, Vec<&submission::Model>> = BTreeMap::new();
    for s in &accepted {
        seq_map.entry(s.participation_id).or_default().push(s);
    }
    let mut team_seqs: Vec<(i32, Vec<&submission::Model>)> =
        seq_map.into_iter().filter(|(_, v)| v.len() >= 3).collect();
    team_seqs.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));
    team_seqs.truncate(50);

    for i in 0..team_seqs.len() {
        for j in (i + 1)..team_seqs.len() {
            let (pa, raw_a) = (team_seqs[i].0, &team_seqs[i].1);
            let (pb, raw_b) = (team_seqs[j].0, &team_seqs[j].1);

            let seq_a: Vec<i32> = raw_a.iter().map(|s| s.challenge_id).collect();
            let seq_b: Vec<i32> = raw_b.iter().map(|s| s.challenge_id).collect();
            let set_a: HashSet<i32> = seq_a.iter().copied().collect();
            let set_b: HashSet<i32> = seq_b.iter().copied().collect();

            // Distinct shared challenges (set intersection). Threshold >= 3.
            let shared_ids: Vec<i32> = set_a
                .iter()
                .copied()
                .filter(|c| set_b.contains(c))
                .collect();
            let shared_count = shared_ids.len();
            if shared_count < 3 {
                continue;
            }
            let union_count = set_a.union(&set_b).count();
            if union_count == 0 {
                continue;
            }

            let jaccard = shared_count as f64 / union_count as f64;
            let lcs = lcs_len(&seq_a, &seq_b);
            let min_len = seq_a.len().min(seq_b.len());
            let lcs_score = if min_len == 0 {
                0.0
            } else {
                lcs as f64 / min_len as f64
            };
            let rsi = jaccard * 0.7 + lcs_score * 0.3;
            if rsi < 0.85 {
                continue;
            }

            // Flag BOTH teams — the copy is symmetric.
            record(db, game_id, pa, None, SuspicionType::SequenceSimilarity).await?;
            record(db, game_id, pb, None, SuspicionType::SequenceSimilarity).await?;

            // Check C stays nested here (rsi >= 0.85 already holds, so the inner
            // rsi >= 0.7 gate is trivially true) plus >= 6 shared challenges.
            if rsi >= 0.7 && shared_count >= 6 {
                let times_a: HashMap<i32, chrono::DateTime<chrono::Utc>> = raw_a
                    .iter()
                    .map(|s| (s.challenge_id, s.submit_time_utc))
                    .collect();
                let times_b: HashMap<i32, chrono::DateTime<chrono::Utc>> = raw_b
                    .iter()
                    .map(|s| (s.challenge_id, s.submit_time_utc))
                    .collect();

                // Direction A→B: positive lag = B solved after A. Receiver = B.
                let lags_a_to_b: Vec<f64> = shared_ids
                    .iter()
                    .filter_map(|cid| match (times_a.get(cid), times_b.get(cid)) {
                        (Some(ta), Some(tb)) => Some(minutes_between(*ta, *tb)),
                        _ => None,
                    })
                    .filter(|&lag| lag > 1.0 && lag <= 60.0)
                    .collect();
                // Direction B→A: positive lag = A solved after B. Receiver = A.
                let lags_b_to_a: Vec<f64> = shared_ids
                    .iter()
                    .filter_map(|cid| match (times_a.get(cid), times_b.get(cid)) {
                        (Some(ta), Some(tb)) => Some(minutes_between(*tb, *ta)),
                        _ => None,
                    })
                    .filter(|&lag| lag > 1.0 && lag <= 60.0)
                    .collect();

                report_relay(db, game_id, &lags_a_to_b, shared_count, pb).await?;
                report_relay(db, game_id, &lags_b_to_a, shared_count, pa).await?;
            }
        }
    }

    // ── Check E: Directed Solving ───────────────────────────────────────────
    // Opened-challenge set per participation from ChallengeOpened events
    // (values[0] = challenge id string).
    let opens = game_event::Entity::find()
        .filter(game_event::Column::GameId.eq(game_id))
        .filter(game_event::Column::EventType.eq(EventType::ChallengeOpened))
        .all(db)
        .await?;
    let mut opened_by_part: BTreeMap<i32, HashSet<i32>> = BTreeMap::new();
    for e in &opens {
        let Some(&pid) = team_to_part.get(&e.team_id) else {
            continue;
        };
        if let Some(cid) = e
            .values
            .get(0)
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<i32>().ok())
        {
            opened_by_part.entry(pid).or_default().insert(cid);
        }
    }

    // Solved-challenge set per participation.
    let mut solved_by_part: BTreeMap<i32, HashSet<i32>> = BTreeMap::new();
    for s in &accepted {
        solved_by_part
            .entry(s.participation_id)
            .or_default()
            .insert(s.challenge_id);
    }

    // Community median open/solve ratio (>= 4 solves & > 0 opens qualify).
    // Plain sorted[len/2] index — NOT a true median — default 2.0 when empty.
    let mut ratios: Vec<f64> = solved_by_part
        .iter()
        .filter_map(|(pid, solved)| {
            let opened = opened_by_part.get(pid).map(|o| o.len()).unwrap_or(0);
            if solved.len() >= 4 && opened > 0 {
                Some(opened as f64 / solved.len() as f64)
            } else {
                None
            }
        })
        .collect();
    ratios.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let community_median = if ratios.is_empty() {
        2.0
    } else {
        ratios[ratios.len() / 2]
    };

    // Suppress entirely if the whole game browses little (focused sprint).
    if community_median >= 1.5 {
        for (pid, solved) in &solved_by_part {
            if solved.len() < 8 {
                continue;
            }
            let opened = opened_by_part.get(pid).map(|o| o.len()).unwrap_or(0);
            if opened == 0 {
                continue;
            }
            let exploration_ratio = opened as f64 / solved.len() as f64;
            if exploration_ratio >= 1.05 {
                continue;
            }
            record(db, game_id, *pid, None, SuspicionType::DirectedSolving).await?;
        }
    }

    Ok(())
}
