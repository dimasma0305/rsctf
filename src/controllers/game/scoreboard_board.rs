//! Scoreboard computation (dynamic scoring + blood + timelines + build_scoreboard)
//! — split from scoreboard.rs to stay under the 1000-line rule.
use super::*;

type FirstSolve = (DateTime<Utc>, Option<Uuid>);
type SolvesByParticipation = HashMap<i32, HashMap<i32, FirstSolve>>;
type EligibleSolve = (DateTime<Utc>, i32, i32, bool, bool, Option<Uuid>);

#[derive(Clone, Default)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum GameLoadFlight {
    Found(game::Model),
    NotFound,
    #[default]
    Failed,
}

/// Port of RSCTF `GameChallenge.CalculateChallengeScore`: the current worth of a
/// challenge at `accepted_count` distinct eligible solves, decayed from
/// `original` down toward the `min_rate` floor along the chosen curve, then
/// floored. Computed in `f64` to mirror C# `double` byte-for-byte.
pub(crate) fn calculate_challenge_score(
    original: i32,
    min_rate: f64,
    difficulty: f64,
    accepted_count: i32,
    curve: ScoreCurve,
) -> i32 {
    // Persisted rows are constrained and every write boundary validates these
    // values, but retain a defensive boundary here for rolling upgrades and old
    // databases. Invalid metadata must never produce negative/NaN scoreboard
    // values.
    let original = original.max(0);
    let min_rate = if min_rate.is_finite() {
        min_rate.clamp(0.0, 1.0)
    } else {
        0.25
    };
    let difficulty = if difficulty.is_finite() && difficulty > 0.0 {
        difficulty
    } else {
        5.0
    };
    if accepted_count <= 1 {
        return original;
    }
    let factor = match curve {
        ScoreCurve::Linear => {
            min_rate.max(1.0 - (1.0 - min_rate) * ((accepted_count - 1) as f64 / difficulty))
        }
        ScoreCurve::Logarithmic => {
            min_rate + (1.0 - min_rate) / (1.0 + (accepted_count as f64).ln() / difficulty)
        }
        // Standard (default): the historical exponential decay.
        ScoreCurve::Standard => {
            min_rate + (1.0 - min_rate) * ((1 - accepted_count) as f64 / difficulty).exp()
        }
    };
    (original as f64 * factor).floor() as i32
}

/// Round half-to-even (banker's rounding), matching C# `Convert.ToInt32(double)`
/// — RSCTF applies this to `challenge.Score * bloodFactor`. (Scores are
/// non-negative, so only the positive half-case is exercised.)
fn banker_round(x: f64) -> i32 {
    let floor = x.floor();
    let diff = x - floor;
    let rounded = if diff > 0.5 {
        floor + 1.0
    } else if diff < 0.5 || (floor as i64) % 2 == 0 {
        floor
    } else {
        floor + 1.0
    };
    rounded as i32
}

fn compare_scoreboard_rows(
    a: &(ScoreboardItem, DateTime<Utc>),
    b: &(ScoreboardItem, DateTime<Utc>),
) -> std::cmp::Ordering {
    b.0.score
        .cmp(&a.0.score)
        .then_with(|| a.1.cmp(&b.1))
        .then_with(|| a.0.id.cmp(&b.0.id))
}

fn may_rank_overall(division_id: Option<i32>, defaults: &HashMap<i32, i32>) -> bool {
    match division_id {
        None => true,
        Some(id) => defaults.get(&id).is_some_and(|permissions| {
            GamePermission(*permissions).contains(GamePermission::RANK_OVERALL)
        }),
    }
}

/// Fold a scoreboard item's (submit-time-ordered) solves into a cumulative
/// `{time, score}` series — one RSCTF `TopTimeLine`.
fn build_timeline_series(item: &ScoreboardItem) -> Json {
    let mut acc: i64 = 0;
    let series: Vec<Json> = item
        .solved_challenges
        .iter()
        .map(|c| {
            acc += c.score as i64;
            serde_json::json!({ "time": c.time.timestamp_millis(), "score": acc })
        })
        .collect();
    serde_json::json!({ "id": item.id, "name": item.name, "items": series })
}

/// Compute the scoreboard with dynamic (decayed) per-challenge scoring and
/// first/second/third-blood bonuses, mirroring RSCTF `GameRepository.GenScoreboard`:
/// each challenge's current worth decays with its distinct-team solve count, the
/// three earliest eligible solvers earn the blood bonus, and every team's score is
/// the sum of its (blood-adjusted) contributions. Ranking is preserved (score
/// desc, then earlier last-accepted submission first); `timelines` carries the
/// cumulative series for the top teams overall and per division.
/// Cache TTL for a computed scoreboard rendering. Short: a jeopardy solve does not
/// event-flush the cache, so this bounds solve-visibility staleness, while still
/// collapsing every client's ~10s `/details` + `/scoreboard` poll into at most one
/// full recompute per variant per window (they were recomputed on every request).
const SCOREBOARD_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(5);

/// Cache keys keyed on `(game, is_monitor)`: `_ScoreBoard_{id}` (monitor/live) and
/// `_ScoreBoardFrozen_{id}` (public, freeze-aware) — exactly the keys the cron /
/// team / admin paths already invalidate.
fn scoreboard_cache_key(g: &game::Model, is_monitor: bool) -> String {
    if is_monitor {
        format!("_ScoreBoard_{}", g.id)
    } else {
        format!("_ScoreBoardFrozen_{}", g.id)
    }
}

/// The scoreboard's wire body as a raw JSON string, from cache or freshly built.
///
/// Our success responses are the **raw model** (no envelope), so the cached JSON
/// string *is* the response body — on a hit it's returned verbatim, skipping the
/// `deserialize -> Model -> re-serialize` round-trip a 2–3 KB board would
/// otherwise pay on every request. The hot `/scoreboard` handler ships these
/// bytes straight to the client. The public variant is freeze-aware *inside*
/// [`build_scoreboard`], so a cached copy can never leak post-freeze solves (only
/// ever up to `SCOREBOARD_CACHE_TTL` stale).
/// Coalesces concurrent scoreboard recomputes so a cache-TTL-expiry stampede
/// doesn't dogpile the DB — at 500 clients, every request in flight when the 5s
/// cache expired used to rebuild the board (an all-submissions scan + scoring)
/// simultaneously, spiking Postgres. Now one caller rebuilds per key, the rest
/// await its JSON.
static SCOREBOARD_SF: std::sync::LazyLock<
    crate::utils::single_flight::SingleFlight<Option<bytes::Bytes>>,
> = std::sync::LazyLock::new(crate::utils::single_flight::SingleFlight::new);

static GAME_ROW_CACHE_GENERATIONS: std::sync::LazyLock<std::sync::RwLock<HashMap<i32, u64>>> =
    std::sync::LazyLock::new(|| std::sync::RwLock::new(HashMap::new()));

pub(super) fn game_row_cache_generation(id: i32) -> u64 {
    GAME_ROW_CACHE_GENERATIONS
        .read()
        .ok()
        .and_then(|generations| generations.get(&id).copied())
        .unwrap_or(0)
}

pub(super) fn cache_game_row_if_current(id: i32, game: game::Model, generation: u64) {
    if game_row_cache_generation(id) != generation {
        return;
    }
    if let Ok(mut cache) = GAME_ROW_CACHE.write() {
        // Recheck after taking the cache write lock so invalidation cannot slip
        // between the generation check and insertion.
        if game_row_cache_generation(id) == generation {
            cache.insert(id, (game, std::time::Instant::now()));
        }
    }
}

pub(crate) fn invalidate_game_row_cache(id: i32) {
    // Cache lock first is the global order also used by insertion. Incrementing
    // makes a pre-invalidation single-flight leader unable to repopulate the row.
    if let Ok(mut cache) = GAME_ROW_CACHE.write() {
        cache.remove(&id);
        if let Ok(mut generations) = GAME_ROW_CACHE_GENERATIONS.write() {
            let generation = generations.entry(id).or_insert(0);
            *generation = generation.wrapping_add(1);
        }
    }
}

/// The scoreboard's wire body as raw bytes, from cache or freshly built. On a hit
/// the returned `Bytes` is a refcount clone (no copy) and the handler ships it as
/// the response body with zero copy.
pub(crate) async fn build_scoreboard_json(
    st: &SharedState,
    g: &game::Model,
    is_monitor: bool,
) -> AppResult<bytes::Bytes> {
    let key = scoreboard_cache_key(g, is_monitor);
    if let Some(bytes) = st.cache.get(&key).await {
        return Ok(bytes);
    }
    // Miss: single-flight the rebuild. A failed leader is broadcast as one
    // failure; followers must not turn it into a synchronized recompute herd.
    let (st2, g2, key2) = (st.clone(), g.clone(), key.clone());
    let coalesced = SCOREBOARD_SF
        .run(&key, move || async move {
            // Another leader may have just populated the cache.
            if let Some(bytes) = st2.cache.get(&key2).await {
                return Some(bytes);
            }
            let model = build_scoreboard(&st2, &g2, is_monitor).await.ok()?;
            let json = serde_json::to_vec(&model).ok()?;
            st2.cache
                .set(&key2, &json, Some(SCOREBOARD_CACHE_TTL))
                .await;
            Some(bytes::Bytes::from(json))
        })
        .await;
    coalesced.ok_or_else(|| AppError::internal("scoreboard cache fill failed"))
}

/// [`build_scoreboard_json`] as a deserialized [`ScoreboardModel`], for callers
/// that project the board (`/solvers`, `/details`). A deserialize failure (schema
/// drift) is a safe miss → recompute.
pub(crate) async fn build_scoreboard_cached(
    st: &SharedState,
    g: &game::Model,
    is_monitor: bool,
) -> AppResult<ScoreboardModel> {
    let bytes = build_scoreboard_json(st, g, is_monitor).await?;
    serde_json::from_slice::<ScoreboardModel>(&bytes).map_err(|e| AppError::internal(e.to_string()))
}

pub(crate) async fn build_scoreboard(
    st: &SharedState,
    g: &game::Model,
    is_monitor: bool,
) -> AppResult<ScoreboardModel> {
    let game_id = g.id;

    // Accepted participations.
    let parts = participation::Entity::find()
        .filter(participation::Column::GameId.eq(game_id))
        .filter(participation::Column::Status.eq(ParticipationStatus::Accepted))
        .all(&st.db)
        .await?;

    // Challenge columns and scoring metadata have ONE eligibility source. Disabled
    // or rejected rows are absent from both presentation and every scoring fold,
    // so an old accepted submission can never survive as a hidden point/cell.
    let challenges = game_challenge::Entity::find()
        .filter(game_challenge::Column::GameId.eq(game_id))
        .filter(game_challenge::Column::IsEnabled.eq(true))
        .filter(game_challenge::Column::ReviewStatus.eq(ChallengeReviewStatus::Active))
        .order_by_asc(game_challenge::Column::Category)
        .order_by_asc(game_challenge::Column::Title)
        .order_by_asc(game_challenge::Column::Id)
        .all(&st.db)
        .await?;
    let (mut challenges_map, challenge_count) = build_challenges_map(&challenges);
    let meta_of: HashMap<i32, &game_challenge::Model> =
        challenges.iter().map(|c| (c.id, c)).collect();

    // Accepted submissions -> distinct solved challenges (earliest solve time per
    // participation+challenge). This is RSCTF's `FirstSolve` snapshot set.
    // Raw SQL on the heaviest scan in the board build: fetch ONLY the four
    // columns the aggregation reads. `submission::Entity::find().all()` would pull
    // every column — including `answer` (the submitted flag string, unbounded) —
    // and entity-map it, for every accepted submission (a table that grows without
    // bound). `Accepted as i16` binds the enum's own `#[repr(i16)]` discriminant,
    // so there is no magic number. Runs on the same pool sea-orm uses (shared
    // connections + prepared-statement cache).
    let subs: Vec<(i32, i32, DateTime<Utc>, Option<Uuid>)> = sqlx::query_as(
        r#"SELECT submission.participation_id, submission.challenge_id,
                  submission.submit_time_utc, submission.user_id
             FROM "Submissions" submission
             JOIN "GameChallenges" challenge
               ON challenge.id = submission.challenge_id
              AND challenge.game_id = submission.game_id
              AND challenge.is_enabled
              AND challenge.review_status = $3
            WHERE submission.game_id = $1 AND submission.status = $2"#,
    )
    .bind(game_id)
    .bind(AnswerResult::Accepted as i16)
    .bind(ChallengeReviewStatus::Active as i16)
    .fetch_all(st.pg())
    .await
    .map_err(|e| AppError::internal(e.to_string()))?;

    // participation_id -> (challenge_id -> (earliest solve time, that solve's user_id)).
    // RSCTF's `FirstSolve` snapshot carries UserName, so we track the submitting user
    // of the earliest accepted submission and surface it on the ChallengeItem.
    let mut solved: SolvesByParticipation = HashMap::new();
    for (participation_id, challenge_id, submit_time_utc, user_id) in &subs {
        let per = solved.entry(*participation_id).or_default();
        per.entry(*challenge_id)
            .and_modify(|(t, u)| {
                if *submit_time_utc < *t {
                    *t = *submit_time_utc;
                    *u = *user_id;
                }
            })
            .or_insert((*submit_time_utc, *user_id));
    }

    // Resolve the submitting users to display names (RSCTF `ChallengeItem.UserName`).
    // Collect to an owned Vec first: passing a lazy iterator that borrows `solved`
    // into the async `user_name_map` over-constrains the returned future's lifetime
    // (the handler then fails axum's `for<'a>` Handler bound — "FnOnce not general").
    let solve_user_ids: Vec<Uuid> = solved
        .values()
        .flat_map(|per| per.values().filter_map(|(_, u)| *u))
        .collect();
    let user_names = user_name_map(st, solve_user_ids.into_iter()).await?;

    // The game's divisions (top-level `divisions` list; RSCTF `DivisionItem`: id + name).
    let divisions = division::Entity::find()
        .filter(division::Column::GameId.eq(game_id))
        .all(&st.db)
        .await?;
    let divisions_json: Vec<Json> = divisions
        .iter()
        .map(|d| serde_json::json!({ "id": d.id, "name": d.name }))
        .collect();

    // Division permission maps (RSCTF `CheckDivisionPermission`): a division's
    // per-challenge override, else its default. Preloaded once to avoid a query per
    // solve. `division_challenge_config` is keyed `(division_id, challenge_id)` with
    // no game_id column, so scope it by the game's division ids.
    let default_perms: HashMap<i32, i32> = divisions
        .iter()
        .map(|d| (d.id, d.default_permissions))
        .collect();
    let div_ids: Vec<i32> = divisions.iter().map(|d| d.id).collect();
    let mut div_challenge_perms: HashMap<(i32, i32), i32> = HashMap::new();
    if !div_ids.is_empty() {
        let cfgs = division_challenge_config::Entity::find()
            .filter(division_challenge_config::Column::DivisionId.is_in(div_ids))
            .all(&st.db)
            .await?;
        for c in cfgs {
            div_challenge_perms.insert((c.division_id, c.challenge_id), c.permissions);
        }
    }
    // Effective `GamePermission` for a participation's division on a challenge.
    // A genuine no-division participation keeps ALL. A stale/mismatched division
    // id fails closed; treating it as no division would silently grant every bit.
    let perm_of = |division_id: Option<i32>, chal_id: i32| -> GamePermission {
        let Some(div_id) = division_id else {
            return GamePermission(GamePermission::ALL);
        };
        if let Some(p) = div_challenge_perms.get(&(div_id, chal_id)) {
            return GamePermission(*p);
        }
        if let Some(p) = default_perms.get(&div_id) {
            return GamePermission(*p);
        }
        GamePermission(0)
    };

    let team_names = team_name_map(st, parts.iter().map(|p| p.team_id)).await?;
    let team_avatars = team_avatar_map(st, parts.iter().map(|p| p.team_id)).await?;
    let part_team: HashMap<i32, i32> = parts.iter().map(|p| (p.id, p.team_id)).collect();

    // ICPC freeze: a non-monitor viewing the game inside `[FreezeTimeUtc, EndTimeUtc)`
    // gets the FROZEN projection — post-freeze solves are fully invisible (they don't
    // contribute to dynamic solve counts, blood, ranks, or timelines). Monitors always
    // see the live board. Mirrors RSCTF `GameController.Scoreboard` + `GenScoreboard`.
    let now = Utc::now();
    let cutoff: Option<DateTime<Utc>> = match g.freeze_time_utc {
        Some(freeze) if !is_monitor && now >= freeze && now < g.end_time_utc => Some(freeze),
        _ => None,
    };
    let is_frozen_view = cutoff.is_some();

    // Per-snapshot eligibility (RSCTF `GenScoreboard`): a solve is "valid" when it lands
    // inside the game window and before the challenge deadline; the division's
    // GamePermission then gates GetScore / AffectDynamicScore / GetBlood independently.
    // `accepted_count` (the dynamic solve count) is driven by AffectDynamicScore, not
    // GetScore. Practice mode waives the game-window bound — a task-directed deviation
    // from RSCTF, whose `GenScoreboard` window check is unconditional.
    let mut accepted_count: HashMap<i32, i32> = HashMap::new();
    // (time, part_id, chal_id, score_eligible, blood_eligible, user_id)
    let mut solve_list: Vec<EligibleSolve> = Vec::new();
    for p in &parts {
        let Some(per) = solved.get(&p.id) else {
            continue;
        };
        for (cid, (t, uid)) in per {
            let Some(challenge) = meta_of.get(cid) else {
                continue;
            };
            // Freeze cutoff: post-freeze snapshots are entirely invisible.
            if cutoff.is_some_and(|cut| *t >= cut) {
                continue;
            }
            let within_window = g.practice_mode || (*t >= g.start_time_utc && *t < g.end_time_utc);
            let within_deadline = challenge.deadline_utc.is_none_or(|d| *t <= d);
            let within_valid = within_window && within_deadline;
            let perm = perm_of(p.division_id, *cid);
            let score_eligible = within_valid && perm.contains(GamePermission::GET_SCORE);
            let affect_dynamic =
                within_valid && perm.contains(GamePermission::AFFECT_DYNAMIC_SCORE);
            let blood_eligible = within_valid && perm.contains(GamePermission::GET_BLOOD);
            if affect_dynamic {
                *accepted_count.entry(*cid).or_insert(0) += 1;
            }
            solve_list.push((*t, p.id, *cid, score_eligible, blood_eligible, *uid));
        }
    }

    // Current (dynamic) score per challenge. A&D / KotH are live-scored — 0 here,
    // matching RSCTF `GenScoreboard`.
    let mut current_score: HashMap<i32, i32> = HashMap::new();
    for c in &challenges {
        let score = if matches!(
            c.challenge_type,
            ChallengeType::AttackDefense | ChallengeType::KingOfTheHill
        ) {
            0
        } else {
            calculate_challenge_score(
                c.original_score,
                c.min_score_rate,
                c.difficulty,
                accepted_count.get(&c.id).copied().unwrap_or(0),
                c.score_curve,
            )
        };
        current_score.insert(c.id, score);
    }

    // Packed blood-bonus bits -> per-tier multiplicative factor (`bits/1000 + 1`),
    // computed in `f32` like RSCTF (`int * float` then banker's round).
    let bonus = g.blood_bonus_value;
    let blood_factor = |slot: usize| -> f32 {
        let bits = match slot {
            0 => (bonus >> 20) & 0x3ff,
            1 => (bonus >> 10) & 0x3ff,
            _ => bonus & 0x3ff,
        };
        bits as f32 / 1000.0 + 1.0
    };

    // Assign blood tiers + per-solve contributions in submit-time order.
    solve_list.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let mut per_part_items: HashMap<i32, Vec<ChallengeItem>> = HashMap::new();
    // Last SCORE-ELIGIBLE solve time per participation (RSCTF only advances
    // LastSubmissionTime for scoring solves, so ineligible late solves can't push a
    // team above one with earlier eligible solves). Solves are iterated in ascending
    // time order, so the final insert wins.
    let mut last_solve: HashMap<i32, DateTime<Utc>> = HashMap::new();
    let mut blood_count: HashMap<i32, usize> = HashMap::new();
    let mut challenge_bloods: HashMap<i32, Vec<Blood>> = HashMap::new();
    for (time, part_id, chal_id, score_eligible, blood_eligible, user_id) in &solve_list {
        let score = current_score.get(chal_id).copied().unwrap_or(0);
        let disable_blood = meta_of
            .get(chal_id)
            .map(|c| c.disable_blood_bonus)
            .unwrap_or(false);
        let mut sub_type = SubmissionType::Normal;
        // Bloods: a team that cannot score (score_eligible) must not consume a
        // first/second/third-blood SLOT, which would downgrade the tier of every
        // legitimately-scoring solver after it (RSCTF gates blood on BloodEligible
        // && ScoreEligible).
        if *blood_eligible && *score_eligible && !disable_blood {
            let count = blood_count.entry(*chal_id).or_insert(0);
            if *count < 3 {
                sub_type = match *count {
                    0 => SubmissionType::FirstBlood,
                    1 => SubmissionType::SecondBlood,
                    _ => SubmissionType::ThirdBlood,
                };
                *count += 1;
                if let Some(team_id) = part_team.get(part_id) {
                    challenge_bloods.entry(*chal_id).or_default().push(Blood {
                        id: *team_id,
                        name: team_names.get(team_id).cloned().unwrap_or_default(),
                        avatar: team_avatars.get(team_id).cloned().flatten(),
                        submit_time_utc: Some(*time),
                    });
                }
            }
        }
        // Contribution: 0 when the division cannot score this challenge; otherwise the
        // base score for Normal solves and the banker's-rounded blood-adjusted score
        // for the three bloods. When the game's blood bonus is zero the factor is
        // exactly 1.0, so this collapses to the base score (RSCTF `NoBonus` branch).
        let contribution = if *score_eligible {
            match sub_type {
                SubmissionType::FirstBlood => banker_round((score as f32 * blood_factor(0)) as f64),
                SubmissionType::SecondBlood => {
                    banker_round((score as f32 * blood_factor(1)) as f64)
                }
                SubmissionType::ThirdBlood => banker_round((score as f32 * blood_factor(2)) as f64),
                _ => score,
            }
        } else {
            0
        };
        // Every solve is listed in the team's cells (score 0 when ineligible), but only
        // score-eligible solves advance the tie-break last-submission time.
        if *score_eligible {
            last_solve.insert(*part_id, *time);
        }
        per_part_items
            .entry(*part_id)
            .or_default()
            .push(ChallengeItem {
                id: *chal_id,
                score: contribution,
                submission_type: sub_type,
                user_name: user_id.and_then(|u| user_names.get(&u).cloned()),
                time: *time,
            });
    }

    // Backfill the challenge column map: dynamic score, live solve count, bloods.
    for infos in challenges_map.values_mut() {
        for info in infos.iter_mut() {
            info.score = current_score.get(&info.id).copied().unwrap_or(info.score);
            info.solved = accepted_count.get(&info.id).copied().unwrap_or(0);
            if let Some(bloods) = challenge_bloods.remove(&info.id) {
                info.bloods = bloods;
            }
        }
    }

    let mut items: Vec<(ScoreboardItem, DateTime<Utc>)> = parts
        .iter()
        .map(|p| {
            // Already time-ordered: solves were pushed in the global sort order.
            let solved_challenges = per_part_items.remove(&p.id).unwrap_or_default();
            let score: i64 = solved_challenges.iter().map(|c| c.score as i64).sum();
            let last = last_solve
                .get(&p.id)
                .copied()
                .unwrap_or(DateTime::<Utc>::MIN_UTC);
            (
                ScoreboardItem {
                    id: p.team_id,
                    name: team_names.get(&p.team_id).cloned().unwrap_or_default(),
                    bio: None,
                    division_id: p.division_id,
                    avatar: team_avatars.get(&p.team_id).cloned().flatten(),
                    score,
                    rank: 0,
                    division_rank: None,
                    last_submission_time: last,
                    solved_count: solved_challenges.len(),
                    solved_challenges,
                },
                last,
            )
        })
        .collect();

    // Rank: score desc, then earlier last-solve first, then stable team id. The
    // final key makes exact ties deterministic across database query plans.
    items.sort_by(compare_scoreboard_rows);

    // Rank (1-based, overall) plus per-division rank: teams are iterated in the same
    // overall order, and DivisionRank is a running 1-based counter per divisionId
    // (RSCTF `GenScoreboard`).
    let mut overall_count = 0;
    let mut division_counts: HashMap<i32, i32> = HashMap::new();
    let ranked: Vec<ScoreboardItem> = items
        .into_iter()
        .map(|(mut item, _)| {
            let valid_division = item
                .division_id
                .and_then(|div_id| default_perms.get(&div_id).map(|p| (div_id, *p)));
            if may_rank_overall(item.division_id, &default_perms) {
                overall_count += 1;
                item.rank = overall_count;
            }
            if let Some((div_id, _)) = valid_division {
                let rank = division_counts.entry(div_id).or_insert(0);
                *rank += 1;
                item.division_rank = Some(*rank);
            }
            item
        })
        .collect();

    // Timelines: cumulative-score series for the top-10 overall (keyed 0) and the
    // top-10 of each division, as a list of RSCTF `TimeLineItem { divisionId, teams }`.
    // The overall entry is always emitted (the client field is required).
    let mut overall: Vec<Json> = Vec::new();
    let mut by_div: BTreeMap<i32, Vec<Json>> = BTreeMap::new();
    for item in &ranked {
        if (1..=10).contains(&item.rank) {
            overall.push(build_timeline_series(item));
        }
        if let (Some(div_id), Some(drank)) = (item.division_id, item.division_rank) {
            if drank <= 10 {
                by_div
                    .entry(div_id)
                    .or_default()
                    .push(build_timeline_series(item));
            }
        }
    }
    let mut timelines: Vec<Json> = vec![serde_json::json!({ "divisionId": 0, "teams": overall })];
    for (div_id, teams) in by_div {
        timelines.push(serde_json::json!({ "divisionId": div_id, "teams": teams }));
    }

    Ok(ScoreboardModel {
        update_time_utc: Utc::now(),
        blood_bonus: g.blood_bonus_value,
        timelines,
        items: ranked,
        divisions: divisions_json,
        challenges: challenges_map,
        challenge_count,
        freeze: g.freeze_time_utc,
        is_frozen_view,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: i32, score: i64, last: DateTime<Utc>) -> (ScoreboardItem, DateTime<Utc>) {
        (
            ScoreboardItem {
                id,
                name: id.to_string(),
                bio: None,
                division_id: None,
                avatar: None,
                score,
                rank: 0,
                division_rank: None,
                last_submission_time: last,
                solved_challenges: Vec::new(),
                solved_count: 0,
            },
            last,
        )
    }

    #[test]
    fn exact_ties_use_team_id_as_the_stable_final_key() {
        let last = DateTime::<Utc>::MIN_UTC;
        let mut rows = vec![row(9, 100, last), row(2, 100, last), row(5, 100, last)];
        rows.sort_by(compare_scoreboard_rows);
        assert_eq!(
            rows.into_iter().map(|r| r.0.id).collect::<Vec<_>>(),
            [2, 5, 9]
        );
    }

    #[test]
    fn overall_rank_is_default_division_permission_and_fails_closed() {
        let defaults = HashMap::from([
            (1, GamePermission::RANK_OVERALL),
            (2, GamePermission::GET_SCORE),
        ]);
        assert!(may_rank_overall(None, &defaults));
        assert!(may_rank_overall(Some(1), &defaults));
        assert!(!may_rank_overall(Some(2), &defaults));
        assert!(!may_rank_overall(Some(404), &defaults));
    }

    #[test]
    fn score_formula_defensively_bounds_legacy_bad_metadata() {
        assert_eq!(
            calculate_challenge_score(-100, -1.0, 0.0, 10, ScoreCurve::Linear),
            0
        );
        let score =
            calculate_challenge_score(100, f64::NAN, f64::INFINITY, 10, ScoreCurve::Standard);
        assert!((0..=100).contains(&score));
    }

    #[test]
    fn game_row_invalidation_fences_an_inflight_generation() {
        let game_id = i32::MIN;
        let before = game_row_cache_generation(game_id);
        invalidate_game_row_cache(game_id);
        assert_ne!(game_row_cache_generation(game_id), before);
    }
}
