//! Abnormal-solve cheat checks — the submission-pattern half of RSCTF
//! `Controllers/CheatReportController.cs`, ported as a whole-game sweep.
//!
//! RSCTF's `CheatReportController.Get` rebuilds the entire cheat report on every
//! monitor request, running a battery of per-submission "abnormal solve" checks
//! against accepted submissions, wrong submissions, and the `Download` /
//! `ContainerStart` / `ContainerDestroy` / `ChallengeOpened` GameEvents. This
//! module ports the *submission-pattern* subset of those checks and persists a
//! `suspicion_event` for each one that fires, reusing the exact dedup + insert +
//! score path ([`super::detectors::record_with_dedup`]) the per-submission live
//! detector uses. Each check therefore fires at most once per participation,
//! rule, and stable global/challenge evidence key.
//!
//! Checks implemented here, with their RSCTF `CheatReportController` origin and
//! exact thresholds:
//!
//! | Rule | RSCTF check | Threshold |
//! | --- | --- | --- |
//! | [`SuspicionType::FastSolveOpen`] | 7a | solve `< 2min` after first `ChallengeOpened` |
//! | [`SuspicionType::FastSolveDownload`] | 7b | attachment solve `< 2min` after first `Download` |
//! | [`SuspicionType::FastSolveContainer`] | 7c | blackbox-container solve `< 2min` after first `ContainerStart` |
//! | [`SuspicionType::NoDownload`] | 4 | attachment challenge solved with no prior `Download` |
//! | [`SuspicionType::NoContainer`] | 5 | container challenge solved with no prior `ContainerStart` |
//! | [`SuspicionType::Hoarding`] | 6 | last op before solve was a destroy, and solve `> 60min` after it |
//! | [`SuspicionType::ZeroWrongAttempts`] | A | dynamic, not-easy, `solveCount >= 5`, zero wrong before solve |
//! | [`SuspicionType::HighWrongRate`] | H1 | `>= 40` wrong within a 60s window (unless solved within 5min) |
//! | [`SuspicionType::AutomatedPattern`] | H2 | `>= 10` consecutive wrong intervals `< 2s` |
//! | [`SuspicionType::FirstBloodAnomaly`] | J | first blood whose 2nd solve is `2+ hours` later |
//!
//! RSCTF does **not** gate this endpoint on the game having ended, so neither
//! does this sweep. Network/identity context checks (SharedIP, fingerprint, …)
//! live in [`super::detectors`]; the collusion / sequence-similarity report lives
//! in `controllers::game::cheat`.

use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, Utc};

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

use super::*;
use crate::app_state::SharedState;
use crate::utils::enums::{AnswerResult, ChallengeType, EventType, FileType};

// ── Thresholds (mirrors the RSCTF constants) ────────────────────────────────
/// FastSolve: solved within this many milliseconds of the first interaction
/// (`TimeSpan.FromMinutes(2)`), strict `<`.
const FAST_SOLVE_MS: i64 = 2 * 60 * 1000;
/// Hoarding: solved more than this many milliseconds after the last container
/// destroy (`TimeSpan.FromMinutes(60)`), strict `>`.
const HOARDING_MIN_GAP_MS: i64 = 60 * 60 * 1000;
/// Check A gate: challenge must have at least this many accepted solves.
const ZERO_WRONG_MIN_SOLVE_COUNT: usize = 5;
/// H1 (`BurstWrongThreshold`): wrong answers within a 60s window.
const BURST_WRONG_THRESHOLD: usize = 40;
/// H window length (`AddSeconds(60)`), in milliseconds.
const BURST_WINDOW_MS: i64 = 60 * 1000;
/// H1 solve-suppression window (`AddMinutes(5)`), in milliseconds.
const BURST_SOLVE_SUPPRESS_MS: i64 = 5 * 60 * 1000;
/// H2 (`AutoSpeedCount`): consecutive sub-2s intervals that trip AutomatedPattern.
const AUTO_SPEED_COUNT: usize = 10;
/// H2 interval ceiling (`< 2.0s`), in milliseconds.
const AUTO_SPEED_INTERVAL_MS: i64 = 2 * 1000;
/// H group floor: only (team,challenge) wrong-groups with `>= 5` wrongs are
/// considered (RSCTF `.Where(g => g.Count() >= 5)`).
const H_GROUP_MIN_WRONGS: usize = 5;
/// FirstBloodAnomaly gap (`TimeSpan.FromHours(2)`), in milliseconds.
const FIRST_BLOOD_GAP_MS: i64 = 2 * 60 * 60 * 1000;
/// Check A / IsChallengeEasy: solve-rate above this fraction of participating
/// teams marks the challenge "easy" (FP-heavy) and suppresses Check A.
const EASY_SOLVE_RATE: f64 = 0.40;
/// … or a zero-wrong-attempt rate above this fraction.
const EASY_ZERO_ATTEMPT_RATE: f64 = 0.30;

/// Run the whole-game abnormal-solve cheat sweep, persisting a `suspicion_event`
/// (and bumping the participation's suspicion score) for every check that fires.
///
/// Ported from RSCTF `CheatReportController.Get`'s abnormal-solve battery. The
/// per-check results are not returned — RSCTF surfaces them as
/// `CheatReport.abnormalSolves`, which the monitor endpoint rebuilds from the
/// persisted events. See the TODO at the end for populating that field directly.
pub async fn run_abnormal_solve_checks(st: &SharedState, game_id: i32) -> AppResult<()> {
    use crate::models::data::{
        flag_context, game_challenge, game_event, game_instance, submission,
    };

    // ── Data gathering ──────────────────────────────────────────────────────
    let challenges = game_challenge::Entity::find()
        .filter(game_challenge::Column::GameId.eq(game_id))
        .all(&st.db)
        .await?;
    if challenges.is_empty() {
        return Ok(());
    }
    let challenge_ids: Vec<i32> = challenges.iter().map(|c| c.id).collect();
    let challenge_map: HashMap<i32, game_challenge::Model> =
        challenges.iter().map(|c| (c.id, c.clone())).collect();

    // Participating teams: (game_id, team_id) -> participation is 1:1, so a
    // participation count is the "participating team count" RSCTF uses.
    let parts = participation::Entity::find()
        .filter(participation::Column::GameId.eq(game_id))
        .all(&st.db)
        .await?;
    let team_participating_count = parts.len();
    let part_team: HashMap<i32, i32> = parts.iter().map(|p| (p.id, p.team_id)).collect();

    // Accepted + wrong submissions for the game.
    let accepted = submission::Entity::find()
        .filter(submission::Column::GameId.eq(game_id))
        .filter(submission::Column::Status.eq(AnswerResult::Accepted))
        .all(&st.db)
        .await?;
    let wrong = submission::Entity::find()
        .filter(submission::Column::GameId.eq(game_id))
        .filter(submission::Column::Status.eq(AnswerResult::WrongAnswer))
        .all(&st.db)
        .await?;

    // Game events, bucketed into (team, challenge) time lists per event type.
    let events = game_event::Entity::find()
        .filter(game_event::Column::GameId.eq(game_id))
        .all(&st.db)
        .await?;
    let mut ev_download: HashMap<(i32, i32), Vec<DateTime<Utc>>> = HashMap::new();
    let mut ev_start: HashMap<(i32, i32), Vec<DateTime<Utc>>> = HashMap::new();
    let mut ev_destroy: HashMap<(i32, i32), Vec<DateTime<Utc>>> = HashMap::new();
    let mut ev_open: HashMap<(i32, i32), Vec<DateTime<Utc>>> = HashMap::new();
    for ev in &events {
        // Values[0] is the parseable challenge id (RSCTF `int.TryParse(Values[0])`).
        let Some(cid) = ev
            .values
            .get(0)
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<i32>().ok())
        else {
            continue;
        };
        let bucket = match ev.event_type {
            EventType::Download => &mut ev_download,
            EventType::ContainerStart => &mut ev_start,
            EventType::ContainerDestroy => &mut ev_destroy,
            EventType::ChallengeOpened => &mut ev_open,
            _ => continue,
        };
        bucket
            .entry((ev.team_id, cid))
            .or_default()
            .push(ev.publish_time_utc);
    }

    // ── RequiresLocalDownload precomputes (RSCTF lines 125-215) ──────────────
    // att_id -> (file type, has a backing local file)
    let mut att_ref: BTreeSet<i32> = BTreeSet::new();
    for c in &challenges {
        if let Some(a) = c.attachment_id {
            att_ref.insert(a);
        }
    }
    let flag_ctxs = flag_context::Entity::find()
        .filter(flag_context::Column::ChallengeId.is_in(challenge_ids.clone()))
        .all(&st.db)
        .await?;
    for fc in &flag_ctxs {
        if let Some(a) = fc.attachment_id {
            att_ref.insert(a);
        }
    }
    let att_map: HashMap<i32, (FileType, bool)> = {
        use crate::models::data::attachment;
        let ids: Vec<i32> = att_ref.iter().copied().collect();
        let mut m = HashMap::new();
        if !ids.is_empty() {
            for a in attachment::Entity::find()
                .filter(attachment::Column::Id.is_in(ids))
                .all(&st.db)
                .await?
            {
                m.insert(a.id, (a.file_type, a.local_file_id.is_some()));
            }
        }
        m
    };
    let att_is_local = |aid: i32| -> bool {
        att_map
            .get(&aid)
            .map(|(t, has)| *t == FileType::Local && *has)
            .unwrap_or(false)
    };

    // fc.id -> (challenge_id, attachment is local) — for per-instance dynamic reqs.
    let fc_map: HashMap<i32, (Option<i32>, bool)> = flag_ctxs
        .iter()
        .map(|f| {
            (
                f.id,
                (
                    f.challenge_id,
                    f.attachment_id.map(att_is_local).unwrap_or(false),
                ),
            )
        })
        .collect();

    // Per-challenge dynamic requirement: any flag context of the challenge has a
    // local attachment.
    let mut per_chal_dyn: HashMap<i32, bool> = HashMap::new();
    for fc in &flag_ctxs {
        if let Some(cid) = fc.challenge_id {
            let local = fc.attachment_id.map(att_is_local).unwrap_or(false);
            let e = per_chal_dyn.entry(cid).or_insert(false);
            *e = *e || local;
        }
    }

    // Per-(team,challenge) dynamic requirement: instances of the challenge whose
    // assigned flag context has a local attachment.
    let instances = game_instance::Entity::find()
        .filter(game_instance::Column::ChallengeId.is_in(challenge_ids.clone()))
        .all(&st.db)
        .await?;
    let mut per_team_dyn: HashMap<(i32, i32), bool> = HashMap::new();
    for inst in &instances {
        let Some(team_id) = part_team.get(&inst.participation_id).copied() else {
            continue;
        };
        let Some(fid) = inst.flag_id else { continue };
        let local = fc_map.get(&fid).map(|(_, l)| *l).unwrap_or(false);
        let e = per_team_dyn
            .entry((team_id, inst.challenge_id))
            .or_insert(false);
        *e = *e || local;
    }

    let requires_local_download = |chal: &game_challenge::Model, team_id: i32| -> bool {
        if chal.challenge_type == ChallengeType::DynamicAttachment {
            if let Some(b) = per_team_dyn.get(&(team_id, chal.id)) {
                return *b;
            }
            return per_chal_dyn.get(&chal.id).copied().unwrap_or(false);
        }
        // Non-dynamic-attachment: use the challenge's own attachment.
        chal.attachment_id.map(att_is_local).unwrap_or(false)
    };

    // ── Wrong-submission index + easy-challenge precomputes ──────────────────
    // (team, challenge) -> wrong submissions, ascending by time.
    let mut wrong_by_tc: HashMap<(i32, i32), Vec<submission::Model>> = HashMap::new();
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
    let mut accepted_by_chal: HashMap<i32, Vec<submission::Model>> = HashMap::new();
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
        (team_participating_count > 0
            && solve_count(cid) as f64 / team_participating_count as f64 > EASY_SOLVE_RATE)
            || zero_attempt_rate(cid) > EASY_ZERO_ATTEMPT_RATE
    };

    // Single shared out-param for record_with_dedup (DB dedup makes this idempotent).
    let mut codes: Vec<i16> = Vec::new();

    // Helper to fire a rule for a participation.
    macro_rules! fire {
        ($pid:expr, $cid:expr, $ty:expr) => {{
            let challenge_id = $cid;
            let evidence_key = challenge_id
                .map(challenge_evidence_key)
                .unwrap_or_else(|| GLOBAL_EVIDENCE_KEY.to_string());
            super::detectors::record_with_dedup(
                &st.db,
                game_id,
                $pid,
                challenge_id,
                $ty,
                &evidence_key,
                &mut codes,
            )
            .await?;
        }};
    }

    // ── Per-accepted-submission checks (4, 5, 6, 7a-c, A) ────────────────────
    for sub in &accepted {
        let Some(chal) = challenge_map.get(&sub.challenge_id) else {
            continue;
        };
        let pid = sub.participation_id;
        let cid = sub.challenge_id;
        let key = (sub.team_id, cid);
        let solve_t = sub.submit_time_utc;

        let downloads = ev_download.get(&key);
        let starts = ev_start.get(&key);
        let destroys = ev_destroy.get(&key);
        let opens = ev_open.get(&key);

        let req_dl = requires_local_download(chal, sub.team_id);
        let is_container = chal.challenge_type.is_container();

        // Check 4: NoDownload — attachment challenge solved, no prior download.
        if req_dl {
            let has_dl = downloads
                .map(|ds| ds.iter().any(|d| *d <= solve_t))
                .unwrap_or(false);
            if !has_dl {
                fire!(pid, Some(cid), SuspicionType::NoDownload);
            }
        }

        // Check 5: NoContainer — container challenge solved, no container start
        // at or before the solve (covers solve-before-start too).
        if is_container {
            let has_start = starts
                .map(|s| s.iter().any(|d| *d <= solve_t))
                .unwrap_or(false);
            if !has_start {
                fire!(pid, Some(cid), SuspicionType::NoContainer);
            }
        }

        // The 6 / 7 block only runs when there is at least one interaction, matching
        // RSCTF's `if (interactions.Any())` guard.
        let has_interaction = downloads.map(|v| !v.is_empty()).unwrap_or(false)
            || starts.map(|v| !v.is_empty()).unwrap_or(false)
            || opens.map(|v| !v.is_empty()).unwrap_or(false);

        if has_interaction {
            // Check 6: Hoarding — last container op before solve was a destroy, and
            // the solve is > 60min after it.
            if is_container {
                if let Some(destroys) = destroys {
                    let relevant_destroys: Vec<DateTime<Utc>> =
                        destroys.iter().copied().filter(|d| *d < solve_t).collect();
                    if !relevant_destroys.is_empty() {
                        let last_destroy = *relevant_destroys.iter().max().unwrap();
                        let last_start =
                            starts.and_then(|s| s.iter().copied().filter(|x| *x < solve_t).max());
                        // Last action was a destroy (no start after it).
                        let destroy_last = match last_start {
                            Some(ls) => last_destroy > ls,
                            None => true,
                        };
                        if destroy_last
                            && (solve_t - last_destroy).num_milliseconds() > HOARDING_MIN_GAP_MS
                        {
                            fire!(pid, Some(cid), SuspicionType::Hoarding);
                        }
                    }
                }
            }

            // Check 7a: FastSolve-Open — applies to all challenges.
            if let Some(opens) = opens {
                if let Some(first_open) = opens.iter().copied().filter(|t| *t <= solve_t).min() {
                    if (solve_t - first_open).num_milliseconds() < FAST_SOLVE_MS {
                        fire!(pid, Some(cid), SuspicionType::FastSolveOpen);
                    }
                }
            }

            // Check 7b: FastSolve-Download — attachment challenges only.
            if req_dl {
                if let Some(downloads) = downloads {
                    if let Some(first_dl) =
                        downloads.iter().copied().filter(|t| *t <= solve_t).min()
                    {
                        if (solve_t - first_dl).num_milliseconds() < FAST_SOLVE_MS {
                            fire!(pid, Some(cid), SuspicionType::FastSolveDownload);
                        }
                    }
                }
            }

            // Check 7c: FastSolve-Container — container challenge with NO local
            // attachment (blackbox), solved right after container start.
            if is_container && !req_dl {
                if let Some(starts) = starts {
                    if let Some(first_start) =
                        starts.iter().copied().filter(|t| *t <= solve_t).min()
                    {
                        if (solve_t - first_start).num_milliseconds() < FAST_SOLVE_MS {
                            fire!(pid, Some(cid), SuspicionType::FastSolveContainer);
                        }
                    }
                }
            }
        }

        // Check A: ZeroWrongAttempts — dynamic, not-easy, real solver base, and no
        // wrong submissions before the solve.
        if chal.challenge_type.is_dynamic()
            && !is_challenge_easy(cid)
            && solve_count(cid) >= ZERO_WRONG_MIN_SOLVE_COUNT
        {
            let wrongs_before = wrong_by_tc
                .get(&key)
                .map(|ws| ws.iter().filter(|w| w.submit_time_utc < solve_t).count())
                .unwrap_or(0);
            if wrongs_before == 0 {
                fire!(pid, Some(cid), SuspicionType::ZeroWrongAttempts);
            }
        }
    }

    // ── Check H: HighWrongRate (H1) + AutomatedPattern (H2) ──────────────────
    // Only (team,challenge) wrong-groups with >= 5 wrongs are considered.
    for ((team_id, cid), wrongs) in &wrong_by_tc {
        if wrongs.len() < H_GROUP_MIN_WRONGS {
            continue;
        }
        // participation id for this (team, challenge): any wrong submission carries it.
        let pid = wrongs[0].participation_id;

        // H1: >= 40 wrong within a 60s window, unless solved within 5min of the
        // window start. Fires on the first qualifying window (RSCTF breaks there).
        if wrongs.len() >= BURST_WRONG_THRESHOLD {
            for anchor in wrongs {
                let window_end_ms = BURST_WINDOW_MS;
                let burst = wrongs
                    .iter()
                    .filter(|w| {
                        let d = (w.submit_time_utc - anchor.submit_time_utc).num_milliseconds();
                        d >= 0 && d <= window_end_ms
                    })
                    .count();
                if burst >= BURST_WRONG_THRESHOLD {
                    let solved_after = accepted_by_chal
                        .get(cid)
                        .map(|solves| {
                            solves.iter().any(|s| {
                                s.team_id == *team_id
                                    && s.submit_time_utc >= anchor.submit_time_utc
                                    && (s.submit_time_utc - anchor.submit_time_utc)
                                        .num_milliseconds()
                                        <= BURST_SOLVE_SUPPRESS_MS
                            })
                        })
                        .unwrap_or(false);
                    if !solved_after {
                        fire!(pid, Some(*cid), SuspicionType::HighWrongRate);
                    }
                    break;
                }
            }
        }

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
                    fire!(pid, Some(*cid), SuspicionType::AutomatedPattern);
                    break;
                }
            }
        }
    }

    // ── Check J: FirstBloodAnomaly ───────────────────────────────────────────
    // First blood whose second solve is 2+ hours later.
    for (cid, solves) in &accepted_by_chal {
        if solves.len() < 2 {
            continue;
        }
        let mut ordered = solves.clone();
        ordered.sort_by_key(|s| s.submit_time_utc);
        let first_blood = &ordered[0];
        let second_time = ordered[1].submit_time_utc;
        let gap = (second_time - first_blood.submit_time_utc).num_milliseconds();
        if gap < FIRST_BLOOD_GAP_MS {
            continue;
        }
        fire!(
            first_blood.participation_id,
            Some(*cid),
            SuspicionType::FirstBloodAnomaly
        );
    }

    // TODO(cheat-report): RSCTF also returns each fired check as a
    // `CheatReport.abnormalSolves` row (team/challenge/type/time/details). The
    // monitor `cheat_report` endpoint currently rebuilds its lists from the
    // persisted `suspicion_event` rows this sweep writes; if a richly-detailed
    // abnormalSolves payload is wanted, collect the fired `(pid, cid, ty)` tuples
    // above into a returned Vec and shape them there.
    Ok(())
}
