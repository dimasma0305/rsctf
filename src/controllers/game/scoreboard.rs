//! Scoreboard, notices, events, participations, and the monitor submission feed + Excel exports.
use super::*;
use sea_orm::sea_query::{Alias, Expr, Func};
use std::collections::BinaryHeap;

#[cfg(test)]
#[path = "solver_page_tests.rs"]
mod solver_page_tests;

const DEFAULT_SOLVER_PAGE_SIZE: u64 = 20;
const MAX_SOLVER_PAGE_SIZE: u64 = 100;
const MAX_SOLVER_SKIP: u64 = 10_000;
const SOLVER_PAGE_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(5);

static SOLVER_PAGE_SF: std::sync::LazyLock<
    crate::utils::single_flight::SingleFlight<Option<bytes::Bytes>>,
> = std::sync::LazyLock::new(crate::utils::single_flight::SingleFlight::new);

/// Bounded solver row used by the player challenge modal. Unlike the legacy
/// scoreboard projection it contains only fields the modal renders.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeSolverPreviewModel {
    pub team_name: String,
    pub team_avatar: Option<String>,
    pub user_name: Option<String>,
    #[serde(rename = "type")]
    pub submission_type: SubmissionType,
    #[serde(with = "crate::utils::datetime::millis")]
    pub time: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeSolverPageModel {
    pub data: Vec<ChallengeSolverPreviewModel>,
    pub total: i64,
    pub next_skip: Option<u64>,
}

pub(super) fn bounded_solver_page(query: &SolversQuery) -> AppResult<(usize, usize)> {
    let count = query
        .count
        .unwrap_or(DEFAULT_SOLVER_PAGE_SIZE)
        .max(1)
        .min(MAX_SOLVER_PAGE_SIZE);
    let skip = query.skip.unwrap_or(0);
    if skip > MAX_SOLVER_SKIP {
        return Err(AppError::bad_request("Solver page offset is too large"));
    }
    Ok((skip as usize, count as usize))
}

// ---------------------------------------------------------------------------
// Notices / Events / Participations
// ---------------------------------------------------------------------------

/// RSCTF `GameController.Notices` uses `[Range(0, 100)] count = 100` (not the shared
/// `PageParams` default of 50), so notices gets its own query defaults + clamp.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoticesPageParams {
    #[serde(default = "notices_default_count")]
    count: u64,
    #[serde(default)]
    skip: u64,
}

fn notices_default_count() -> u64 {
    100
}

impl NoticesPageParams {
    /// Clamp `count` to RSCTF's `[Range(0, 100)]`.
    fn limit(&self) -> u64 {
        self.count.clamp(0, 100)
    }
}

/// `GET /api/game/{id}/notices`
pub async fn notices(
    State(st): State<SharedState>,
    MaybeUser(maybe): MaybeUser,
    Path(id): Path<i32>,
    Query(page): Query<NoticesPageParams>,
) -> AppResult<RequestResponse<Vec<GameNoticeModel>>> {
    let g = load_game(&st, id).await?;
    let is_monitor = maybe.as_ref().is_some_and(|u| u.is_monitor());
    if g.hidden && !is_monitor {
        return Err(AppError::not_found("Game not found"));
    }
    // RSCTF `Notices` denies a not-yet-started game (no monitor exemption).
    if Utc::now() < g.start_time_utc {
        return Err(AppError::game_not_started());
    }

    let now = Utc::now();

    // RSCTF `GetLatestNotices` publish-time gate: a Normal (admin) notice is visible
    // only once its scheduled `PublishTimeUtc` has arrived (system notices — blood /
    // hint / new-challenge — are always eligible), else it leaks when created.
    let mut query = game_notice::Entity::find()
        .filter(game_notice::Column::GameId.eq(id))
        .filter(
            Condition::any()
                .add(game_notice::Column::NoticeType.ne(NoticeType::Normal))
                .add(game_notice::Column::PublishTimeUtc.lte(now)),
        );

    // During the ICPC freeze window [FreezeTimeUtc, EndTimeUtc), hide blood notices
    // published at/after the freeze from non-monitors — they reveal the standings
    // movement the frozen scoreboard conceals (the live broadcast is already
    // suppressed in `submit`; this closes the polling path). After the game ends,
    // everyone sees them again. Applied BEFORE skip/take, mirroring RSCTF
    // `GameController.Notices` (filter the notice set, then paginate).
    if crate::utils::scoring::public_scoreboard_frozen(
        g.freeze_time_utc,
        g.end_time_utc,
        now,
        is_monitor,
    ) {
        let freeze = g
            .freeze_time_utc
            .expect("a frozen scoreboard view has a freeze timestamp");
        query = query.filter(
            Condition::any()
                .add(game_notice::Column::PublishTimeUtc.lt(freeze))
                .add(
                    Condition::all()
                        .add(game_notice::Column::NoticeType.ne(NoticeType::FirstBlood))
                        .add(game_notice::Column::NoticeType.ne(NoticeType::SecondBlood))
                        .add(game_notice::Column::NoticeType.ne(NoticeType::ThirdBlood)),
                ),
        );
    }

    // RSCTF orders `Type == Normal ? now : PublishTimeUtc` DESC: Normal (admin) notices
    // pin to the top (as if published now), the rest by publish time desc. A CASE keeps
    // this at SQL level, before skip/take (in-memory sorting would race the pagination).
    let order_expr: sea_orm::sea_query::SimpleExpr = sea_orm::sea_query::CaseStatement::new()
        .case(
            game_notice::Column::NoticeType.eq(NoticeType::Normal),
            sea_orm::sea_query::Expr::value(now),
        )
        .finally(game_notice::Column::PublishTimeUtc.into_expr())
        .into();
    let rows = query
        .order_by(order_expr, sea_orm::sea_query::Order::Desc)
        .offset(page.skip)
        .limit(page.limit())
        .all(&st.db)
        .await?;

    let data = rows
        .into_iter()
        .map(|n| GameNoticeModel {
            id: n.id,
            notice_type: n.notice_type,
            values: n.values,
            time: n.publish_time_utc,
        })
        .collect();
    Ok(RequestResponse::ok(data))
}

/// `GET /api/game/{id}/events` — requires Monitor.
///
/// `hideContainer` drops container-lifecycle events and `search` matches the
/// event-scoped team/user/value projection before pagination. Zero/omitted
/// counts use the bounded default; no live request can materialize all rows.
pub async fn events(
    State(st): State<SharedState>,
    MonitorUser(_user): MonitorUser,
    Path(id): Path<i32>,
    Query(q): Query<EventQuery>,
) -> AppResult<RequestResponse<Vec<GameEventModel>>> {
    let g = load_game(&st, id).await?;
    // RSCTF `Events` denies a not-yet-started game (before the event query runs).
    if Utc::now() < g.start_time_utc {
        return Err(AppError::game_not_started());
    }

    let data = monitor_history::load_events(st.pg(), id, &q).await?;
    Ok(RequestResponse::ok(data))
}

/// `GET /api/game/{id}/participations` — requires game admin. RSCTF gates this
/// with `[RequireGameAdmin]` (`GameController.cs`), which resolves to a platform
/// Admin OR an `EventManager`/co-manager of THIS game. rsctf mirrors that: a
/// platform admin, or a `game_manager` row for `(id, user.id)`, may list the
/// game's participations to review/accept teams.
pub async fn participations(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<Vec<ParticipationInfoModel>>> {
    let _ = load_game(&st, id).await?;

    // Game-admin gate (mirrors `edit::manager_or_admin`): platform admin, or a
    // co-manager of this specific game.
    if !user.is_admin()
        && game_manager::Entity::find()
            .filter(game_manager::Column::GameId.eq(id))
            .filter(game_manager::Column::UserId.eq(user.id))
            .count(&st.db)
            .await?
            == 0
    {
        return Err(AppError::Forbidden);
    }

    let parts = participation::Entity::find()
        .filter(participation::Column::GameId.eq(id))
        .order_by_asc(participation::Column::TeamId)
        .all(&st.db)
        .await?;

    // Team rows for the participating teams.
    let team_ids: Vec<i32> = parts.iter().map(|p| p.team_id).collect();
    let teams: HashMap<i32, team::Model> = if team_ids.is_empty() {
        HashMap::new()
    } else {
        team::Entity::find()
            .filter(team::Column::Id.is_in(team_ids))
            .all(&st.db)
            .await?
            .into_iter()
            .map(|t| (t.id, t))
            .collect()
    };

    // Registered members per participation — RSCTF emits `part.Members.Select(m
    // => m.UserId)`, i.e. the user-id GUIDs (not usernames). Sourced from the
    // `user_participation` rows keyed by participation id.
    let links = user_participation::Entity::find()
        .filter(user_participation::Column::GameId.eq(id))
        .all(&st.db)
        .await?;
    let mut members_by_part: HashMap<i32, Vec<Uuid>> = HashMap::new();
    for l in &links {
        members_by_part
            .entry(l.participation_id)
            .or_default()
            .push(l.user_id);
    }

    // Team roster (RSCTF `team.Members`): the `team_member` rows for each
    // participating team plus the team captain, deduped, resolved to the
    // `ProfileUserInfoModel` shape the client's `TeamWithDetailedUserInfo`
    // expects (userId/userName/email/...).
    let roster_rows = if teams.is_empty() {
        Vec::new()
    } else {
        team_member::Entity::find()
            .filter(team_member::Column::TeamId.is_in(teams.keys().copied().collect::<Vec<_>>()))
            .all(&st.db)
            .await?
    };
    let mut roster_by_team: HashMap<i32, Vec<Uuid>> = HashMap::new();
    for r in &roster_rows {
        roster_by_team.entry(r.team_id).or_default().push(r.user_id);
    }
    // Resolve every roster + captain user id to a user row.
    let mut member_ids: HashSet<Uuid> = roster_rows.iter().map(|r| r.user_id).collect();
    for t in teams.values() {
        member_ids.insert(t.captain_id);
    }
    let member_users: HashMap<Uuid, user::Model> = if member_ids.is_empty() {
        HashMap::new()
    } else {
        user::Entity::find()
            .filter(user::Column::Id.is_in(member_ids.into_iter().collect::<Vec<_>>()))
            .all(&st.db)
            .await?
            .into_iter()
            .map(|u| (u.id, u))
            .collect()
    };
    // camelCase `ProfileUserInfoModel` for one team member.
    let member_info = |u: &user::Model| -> Json {
        serde_json::json!({
            "userId": u.id,
            "role": u.role,
            "userName": u.user_name,
            "email": u.email,
            "bio": u.bio,
            "phone": u.phone_number,
            "realName": u.real_name,
            "stdNumber": u.std_number,
            "avatar": u.avatar_url(),
            "hasManagedGames": false,
        })
    };

    let data = parts
        .into_iter()
        .map(|p| {
            let t = teams.get(&p.team_id);
            // Roster user ids: captain first, then team_member rows, deduped.
            let mut member_uids: Vec<Uuid> = Vec::new();
            let mut seen: HashSet<Uuid> = HashSet::new();
            if let Some(t) = t {
                if seen.insert(t.captain_id) {
                    member_uids.push(t.captain_id);
                }
            }
            for uid in roster_by_team.get(&p.team_id).into_iter().flatten() {
                if seen.insert(*uid) {
                    member_uids.push(*uid);
                }
            }
            let members: Vec<Json> = member_uids
                .into_iter()
                .filter_map(|uid| member_users.get(&uid).map(member_info))
                .collect();
            let team = TeamWithDetailedUserInfo {
                id: p.team_id,
                locked: t.map(|t| t.locked).unwrap_or(false),
                captain_id: t.map(|t| t.captain_id).unwrap_or_default(),
                name: t.map(|t| t.name.clone()),
                bio: t.and_then(|t| t.bio.clone()),
                avatar: t.and_then(|t| t.avatar_url()),
                members,
            };
            ParticipationInfoModel {
                registered_members: members_by_part.remove(&p.id).unwrap_or_default(),
                id: p.id,
                team,
                division_id: p.division_id,
                status: p.status,
            }
        })
        .collect();
    Ok(RequestResponse::ok(data))
}

// ---------------------------------------------------------------------------
// Scoreboard
// ---------------------------------------------------------------------------

/// `GET /api/game/{id}/scoreboard` — team ranking by summed solved-challenge score.
///
/// The single hottest read on the platform (every play/scoreboard page polls it),
/// so it takes the fast path end-to-end: a 1s-cached game row (no per-request
/// Postgres lookup) and the pre-serialized cached board bytes returned verbatim
/// (no `deserialize -> re-serialize`). The body is byte-identical to
/// `RequestResponse::ok(model)` — the raw model as `application/json`.
pub async fn scoreboard(
    State(st): State<SharedState>,
    MaybeUser(maybe): MaybeUser,
    Path(id): Path<i32>,
) -> AppResult<Response> {
    let g = load_game_cached(&st, id).await?;
    let is_monitor = maybe.as_ref().is_some_and(|u| u.is_monitor());
    if g.hidden && !is_monitor {
        return Err(AppError::not_found("Game not found"));
    }
    // RSCTF `Scoreboard` denies a not-yet-started game (no monitor exemption).
    if Utc::now() < g.start_time_utc {
        return Err(AppError::game_not_started());
    }

    let json = build_scoreboard_json(&st, &g, is_monitor).await?;
    Ok(([(header::CONTENT_TYPE, "application/json")], json).into_response())
}

/// `GET /api/game/{id}/challenges/{challengeId}/solvers` — teams that solved one
/// challenge, ordered by solve time. Mirrors RSCTF `GameController.GetChallengeSolvers`:
/// a projection of the (freeze-aware) scoreboard. For each team whose `solvedChallenges`
/// holds `challengeId`, emit a `ChallengeSolverModel` (rank/team/avatar from the item,
/// userName/type/time/score from the solved cell). `RequireUser` + Accepted-participant
/// gate remains in force after closeout so the read-only challenge archive can show
/// final solvers. A non-monitor
/// inside `[FreezeTimeUtc, EndTimeUtc)` gets the FROZEN board, keeping post-freeze solves
/// hidden. `count` defaults to 20 and is capped at 100; the bounded heap prevents a
/// client from making this compatibility route clone and sort the full roster.
pub async fn challenge_solvers(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, challenge_id)): Path<(i32, i32)>,
    Query(q): Query<SolversQuery>,
) -> AppResult<RequestResponse<Vec<ChallengeSolverModel>>> {
    let ctx = context_info(&st, &user, id, false).await?;
    load_playable_challenge(&st, id, challenge_id).await?;
    let permission = effective_permission(&st, &ctx.participation, challenge_id).await?;
    if !permission.contains(GamePermission::VIEW_CHALLENGE) {
        return Err(AppError::not_found("Challenge not found"));
    }
    let (skip, count) = bounded_solver_page(&q)?;
    let board = build_scoreboard_cached(&st, &ctx.game, user.is_monitor()).await?;

    // Keep only the earliest `skip + count` positions while scanning. The
    // compatibility DTO still needs the scoreboard rank/score, but memory and
    // cloning are now bounded by the requested page rather than roster size.
    let window = skip.saturating_add(count);
    let mut earliest: BinaryHeap<(DateTime<Utc>, usize, usize)> =
        BinaryHeap::with_capacity(window.saturating_add(1));
    for (item_index, item) in board.items.iter().enumerate() {
        let Some(solve_index) = item
            .solved_challenges
            .iter()
            .position(|solve| solve.id == challenge_id)
        else {
            continue;
        };
        let position = (
            item.solved_challenges[solve_index].time,
            item_index,
            solve_index,
        );
        if earliest.len() < window {
            earliest.push(position);
        } else if earliest.peek().is_some_and(|latest| position < *latest) {
            earliest.pop();
            earliest.push(position);
        }
    }

    let paged = earliest
        .into_sorted_vec()
        .into_iter()
        .skip(skip)
        .map(|(_, item_index, solve_index)| {
            let item = &board.items[item_index];
            let solve = &item.solved_challenges[solve_index];
            ChallengeSolverModel {
                rank: item.rank,
                team_name: item.name.clone(),
                team_avatar: item.avatar.clone(),
                user_name: solve.user_name.clone(),
                submission_type: solve.submission_type,
                time: solve.time,
                score: solve.score,
            }
        })
        .collect();
    Ok(RequestResponse::ok(paged))
}

#[derive(sqlx::FromRow)]
pub(super) struct ChallengeSolverPreviewRow {
    pub(super) team_name: String,
    team_avatar_hash: Option<String>,
    user_name: Option<String>,
    submit_time_utc: DateTime<Utc>,
    blood_eligible: bool,
    blood_position: i64,
    pub(super) total: i64,
}

const CHALLENGE_SOLVER_PAGE_SQL: &str = r#"
WITH solver_base AS (
    SELECT participation.id AS participation_id,
           submission.submit_time_utc,
           team.name AS team_name,
           team.avatar_hash AS team_avatar_hash,
           account.user_name,
           (game.practice_mode OR
              (submission.submit_time_utc >= game.start_time_utc AND
               submission.submit_time_utc < game.end_time_utc)) AND
           (challenge.deadline_utc IS NULL OR
              submission.submit_time_utc <= challenge.deadline_utc) AS is_valid,
           CASE
             WHEN participation.division_id IS NULL THEN $7
             WHEN division.id IS NULL THEN 0
             ELSE COALESCE(permission.permissions, division.default_permissions)
           END AS permissions
      FROM "FirstSolves" first_solve
      JOIN "Submissions" submission
        ON submission.id = first_solve.submission_id
       AND submission.participation_id = first_solve.participation_id
       AND submission.challenge_id = first_solve.challenge_id
       AND submission.game_id = $1
       AND submission.status = $3
      JOIN "Participations" participation
        ON participation.id = first_solve.participation_id
       AND participation.game_id = submission.game_id
       AND participation.team_id = submission.team_id
       AND participation.status = $4
      JOIN "Teams" team ON team.id = participation.team_id
      LEFT JOIN "AspNetUsers" account ON account.id = submission.user_id
      JOIN "Games" game ON game.id = submission.game_id
      JOIN "GameChallenges" challenge
        ON challenge.id = first_solve.challenge_id
       AND challenge.game_id = submission.game_id
       AND challenge.is_enabled
       AND challenge.review_status = $5
      LEFT JOIN "Divisions" division
        ON division.id = participation.division_id
       AND division.game_id = participation.game_id
      LEFT JOIN "DivisionChallengeConfigs" permission
        ON permission.division_id = division.id
       AND permission.challenge_id = challenge.id
     WHERE first_solve.challenge_id = $2
       AND ($6::timestamptz IS NULL OR submission.submit_time_utc < $6)
), numbered AS (
    SELECT solver_base.*,
           COUNT(*) OVER () AS total,
           is_valid AND
             (permissions & $8) <> 0 AND
             (permissions & $9) <> 0 AS blood_eligible,
           COUNT(*) FILTER (
             WHERE is_valid
               AND (permissions & $8) <> 0
               AND (permissions & $9) <> 0
           ) OVER (
             ORDER BY submit_time_utc, participation_id
             ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW
           ) AS blood_position
      FROM solver_base
)
SELECT team_name, team_avatar_hash, user_name, submit_time_utc,
       blood_eligible, blood_position, total
  FROM numbered
 ORDER BY submit_time_utc, participation_id
 LIMIT $10 OFFSET $11
"#;

pub(super) async fn load_challenge_solver_page(
    pool: &sqlx::PgPool,
    game_id: i32,
    challenge_id: i32,
    cutoff: Option<DateTime<Utc>>,
    skip: usize,
    count: usize,
) -> AppResult<Vec<ChallengeSolverPreviewRow>> {
    sqlx::query_as(CHALLENGE_SOLVER_PAGE_SQL)
        .bind(game_id)
        .bind(challenge_id)
        .bind(AnswerResult::Accepted as i16)
        .bind(ParticipationStatus::Accepted as i16)
        .bind(ChallengeReviewStatus::Active as i16)
        .bind(cutoff)
        .bind(GamePermission::ALL)
        .bind(GamePermission::GET_SCORE)
        .bind(GamePermission::GET_BLOOD)
        .bind(count as i64)
        .bind(skip as i64)
        .fetch_all(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

/// Compact, SQL-paged solver view used by the modal. This reads one challenge's
/// canonical FirstSolves rows rather than deserializing and scanning the full
/// event scoreboard. The existing accepted-participant, freeze, and division
/// visibility boundaries remain authoritative.
pub async fn challenge_solver_page(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, challenge_id)): Path<(i32, i32)>,
    Query(query): Query<SolversQuery>,
) -> AppResult<Response> {
    let ctx = context_info(&st, &user, id, false).await?;
    load_playable_challenge(&st, id, challenge_id).await?;
    let permission = effective_permission(&st, &ctx.participation, challenge_id).await?;
    if !permission.contains(GamePermission::VIEW_CHALLENGE) {
        return Err(AppError::not_found("Challenge not found"));
    }
    let (skip, count) = bounded_solver_page(&query)?;
    let cutoff = crate::utils::scoring::public_scoreboard_frozen(
        ctx.game.freeze_time_utc,
        ctx.game.end_time_utc,
        Utc::now(),
        user.is_monitor(),
    )
    .then_some(ctx.game.freeze_time_utc)
    .flatten();
    let cutoff_key = cutoff.map_or_else(
        || "live".to_string(),
        |time| time.timestamp_millis().to_string(),
    );
    let cache_key = format!(
        "solver-page:v1:{id}:{challenge_id}:{}:{cutoff_key}:{skip}:{count}",
        user.is_monitor()
    );
    if let Some(bytes) = st.cache.get(&cache_key).await {
        return Ok(([(header::CONTENT_TYPE, "application/json")], bytes).into_response());
    }

    let (state, key) = (st.clone(), cache_key.clone());
    let bytes = SOLVER_PAGE_SF
        .run(&cache_key, move || async move {
            if let Some(bytes) = state.cache.get(&key).await {
                return Some(bytes);
            }
            let rows =
                match load_challenge_solver_page(state.pg(), id, challenge_id, cutoff, skip, count)
                    .await
                {
                    Ok(rows) => rows,
                    Err(error) => {
                        tracing::error!(
                            game_id = id,
                            challenge_id,
                            skip,
                            count,
                            error = %error,
                            "failed to load compact challenge solver page"
                        );
                        return None;
                    }
                };
            let total = rows.first().map_or(0, |row| row.total);
            let data = rows
                .into_iter()
                .map(|row| ChallengeSolverPreviewModel {
                    team_name: row.team_name,
                    team_avatar: row
                        .team_avatar_hash
                        .map(|hash| format!("/assets/{hash}/avatar")),
                    user_name: row.user_name,
                    submission_type: if row.blood_eligible {
                        match row.blood_position {
                            1 => SubmissionType::FirstBlood,
                            2 => SubmissionType::SecondBlood,
                            3 => SubmissionType::ThirdBlood,
                            _ => SubmissionType::Normal,
                        }
                    } else {
                        SubmissionType::Normal
                    },
                    time: row.submit_time_utc,
                })
                .collect::<Vec<_>>();
            let consumed = skip.saturating_add(data.len()) as i64;
            let next_skip = (consumed < total).then_some(consumed as u64);
            let model = ChallengeSolverPageModel {
                data,
                total,
                next_skip,
            };
            let encoded = match serde_json::to_vec(&model) {
                Ok(encoded) => encoded,
                Err(error) => {
                    tracing::error!(
                        game_id = id,
                        challenge_id,
                        error = %error,
                        "failed to encode compact challenge solver page"
                    );
                    return None;
                }
            };
            state
                .cache
                .set(&key, &encoded, Some(SOLVER_PAGE_CACHE_TTL))
                .await;
            Some(bytes::Bytes::from(encoded))
        })
        .await
        .ok_or_else(|| AppError::internal("solver page cache fill failed"))?;
    Ok(([(header::CONTENT_TYPE, "application/json")], bytes).into_response())
}

/// `GET /api/game/{id}/scoreboardsheet` — Excel export of the scoreboard.
///
/// Mirrors RSCTF `ScoreboardSheet` + `ExcelHelper.GetScoreboardExcel`, trimmed to
/// the columns rsctf surfaces (rank / team / score / solved). Returns the raw
/// `.xlsx` bytes as a file attachment; any spreadsheet build error degrades to a
/// 400 (matching the C# `catch → BadRequest`), never a 500.
pub async fn scoreboard_sheet(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path(id): Path<i32>,
) -> AppResult<Response> {
    let g = load_game(&st, id).await?;
    if Utc::now() < g.start_time_utc {
        return Err(AppError::bad_request("Game has not started"));
    }

    // Monitor-only export: always the live (unfrozen) board.
    let board = build_scoreboard_cached(&st, &g, true).await?;
    let bytes = build_scoreboard_xlsx(&board)
        .map_err(|_| AppError::bad_request("Failed to build scoreboard sheet"))?;

    let filename = format!(
        "{}-Scoreboard-{}.xlsx",
        sanitize_filename(&g.title),
        Utc::now().format("%Y%m%d-%H.%M.%SZ")
    );
    Ok(xlsx_response(bytes, &filename))
}

/// Build the scoreboard `.xlsx` in memory (rank / team / score / solved).
fn build_scoreboard_xlsx(board: &ScoreboardModel) -> Result<Vec<u8>, rust_xlsxwriter::XlsxError> {
    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet();
    sheet.set_name("Scoreboard")?;
    for (col, h) in ["Ranking", "Team", "Score", "Solved"].iter().enumerate() {
        sheet.write_string(0, col as u16, *h)?;
    }
    for (i, item) in board.items.iter().enumerate() {
        let row = (i + 1) as u32;
        sheet.write_number(row, 0, item.rank as f64)?;
        sheet.write_string(row, 1, item.name.clone())?;
        sheet.write_number(row, 2, item.score as f64)?;
        sheet.write_number(row, 3, item.solved_count as f64)?;
    }
    workbook.save_to_buffer()
}

// ---------------------------------------------------------------------------
// Submissions (monitor)
// ---------------------------------------------------------------------------

/// `GET /api/game/{id}/submissions` — requires Monitor.
pub async fn submissions(
    State(st): State<SharedState>,
    MonitorUser(_user): MonitorUser,
    Path(id): Path<i32>,
    Query(q): Query<SubmissionQuery>,
) -> AppResult<RequestResponse<Vec<SubmissionModel>>> {
    let _ = load_game(&st, id).await?;

    let status = q.type_filter.as_deref().and_then(parse_answer_result);
    let data = monitor_history::load_submissions(st.pg(), id, &q, status).await?;
    Ok(RequestResponse::ok(data))
}

/// `GET /api/game/{id}/submissionsheet` — Excel export of every submission.
///
/// Mirrors RSCTF `SubmissionSheet` + `ExcelHelper.GetSubmissionExcel`: one row per
/// submission with time / team / user / challenge / answer / status. Returns the
/// raw `.xlsx` bytes as a file attachment.
pub async fn submission_sheet(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path(id): Path<i32>,
) -> AppResult<Response> {
    let g = load_game(&st, id).await?;
    if Utc::now() < g.start_time_utc {
        return Err(AppError::bad_request("Game has not started"));
    }

    let rows = submission::Entity::find()
        .filter(submission::Column::GameId.eq(id))
        .order_by_desc(submission::Column::SubmitTimeUtc)
        .all(&st.db)
        .await?;

    let team_names = team_name_map(&st, rows.iter().map(|s| s.team_id)).await?;
    let user_names = user_name_map(&st, rows.iter().filter_map(|s| s.user_id)).await?;
    let challenge_titles = challenge_title_map(&st, rows.iter().map(|s| s.challenge_id)).await?;

    let projected: Vec<[String; 6]> = rows
        .iter()
        .map(|s| {
            [
                s.submit_time_utc.format("%Y-%m-%d %H:%M:%SZ").to_string(),
                team_names.get(&s.team_id).cloned().unwrap_or_default(),
                s.user_id
                    .and_then(|u| user_names.get(&u).cloned())
                    .unwrap_or_default(),
                challenge_titles
                    .get(&s.challenge_id)
                    .cloned()
                    .unwrap_or_default(),
                s.answer.clone(),
                answer_result_str(s.status).to_string(),
            ]
        })
        .collect();

    let bytes = build_submission_xlsx(&projected)
        .map_err(|_| AppError::bad_request("Failed to build submission sheet"))?;

    let filename = format!(
        "{}_Submissions_{}.xlsx",
        sanitize_filename(&g.title),
        Utc::now().format("%Y%m%d%H%M%S")
    );
    Ok(xlsx_response(bytes, &filename))
}

/// Build the submissions `.xlsx` in memory (time / team / user / challenge /
/// answer / status), one row per pre-projected submission.
fn build_submission_xlsx(rows: &[[String; 6]]) -> Result<Vec<u8>, rust_xlsxwriter::XlsxError> {
    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet();
    sheet.set_name("Submissions")?;
    for (col, h) in ["Time", "Team", "User", "Challenge", "Answer", "Status"]
        .iter()
        .enumerate()
    {
        sheet.write_string(0, col as u16, *h)?;
    }
    for (i, r) in rows.iter().enumerate() {
        let row = (i + 1) as u32;
        for (col, v) in r.iter().enumerate() {
            sheet.write_string(row, col as u16, v.clone())?;
        }
    }
    workbook.save_to_buffer()
}

/// Human-readable label for an `AnswerResult`, mirroring RSCTF `ToShortString`.
fn answer_result_str(r: AnswerResult) -> &'static str {
    match r {
        AnswerResult::NotFound => "Not Found",
        AnswerResult::FlagSubmitted => "Submitted",
        AnswerResult::Accepted => "Accepted",
        AnswerResult::WrongAnswer => "Wrong Answer",
        AnswerResult::CheatDetected => "Cheat Detected",
    }
}

/// Spreadsheet MIME type shared by both `.xlsx` exports.
const XLSX_MIME: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";

/// Wrap `.xlsx` bytes in an attachment `Response`.
fn xlsx_response(bytes: Vec<u8>, filename: &str) -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, XLSX_MIME.to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}\"", filename),
            ),
        ],
        Body::from(bytes),
    )
        .into_response()
}

/// Strip characters that would break a `Content-Disposition` filename.
fn sanitize_filename(name: &str) -> String {
    name.replace(['"', '\r', '\n', '/', '\\'], "_")
}
