//! Scoreboard, notices, events, participations, and the monitor submission feed + Excel exports.
use super::*;
use sea_orm::sea_query::{Alias, Expr, Func};

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
    if let Some(freeze) = g.freeze_time_utc {
        if !is_monitor && now >= freeze && now < g.end_time_utc {
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
/// Mirrors RSCTF `GameController.Events` + `GameEventRepository.GetEvents`:
/// `hideContainer` drops the container-lifecycle events, `search` matches team
/// name / user name (applied at SQL level, i.e. BEFORE pagination so page counts
/// stay correct), and `count`/`skip` follow `TakeAllIfZero` (count 0 ⇒ all rows).
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

    let mut query = game_event::Entity::find().filter(game_event::Column::GameId.eq(id));

    // `hideContainer`: exclude ContainerStart / ContainerDestroy lifecycle events.
    if q.hide_container {
        query = query.filter(
            Condition::all()
                .add(game_event::Column::EventType.ne(EventType::ContainerStart))
                .add(game_event::Column::EventType.ne(EventType::ContainerDestroy)),
        );
    }

    // `search`: RSCTF matches team name / user name / the `Values` array (raw SQL:
    // `array_to_string(Values, ' ') ILIKE '%term%'`). `Values` is a Postgres `text[]`
    // modeled here as `Json`; casting the column to text and matching a lower-cased
    // `LIKE '%term%'` reproduces the containment on the array's contents. Team/user-name
    // matches are resolved to ids up front; all predicates are OR-ed at SQL level so
    // they land BEFORE skip/take (mirrors RSCTF `GameEventRepository.GetEvents`).
    if let Some(term) = q.search.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let team_ids: Vec<i32> = team::Entity::find()
            .filter(team::Column::Name.contains(term))
            .all(&st.db)
            .await?
            .into_iter()
            .map(|t| t.id)
            .collect();
        let user_ids: Vec<Uuid> = user::Entity::find()
            .filter(user::Column::UserName.contains(term))
            .all(&st.db)
            .await?
            .into_iter()
            .map(|u| u.id)
            .collect();
        let values_pat = format!("%{}%", term.to_lowercase());
        query = query.filter(
            Condition::any()
                .add(game_event::Column::TeamId.is_in(team_ids))
                .add(game_event::Column::UserId.is_in(user_ids))
                .add(
                    Expr::expr(Func::lower(
                        game_event::Column::Values
                            .into_expr()
                            .cast_as(Alias::new("text")),
                    ))
                    .like(values_pat.as_str()),
                ),
        );
    }

    query = query.order_by_desc(game_event::Column::PublishTimeUtc);
    // `TakeAllIfZero`: count 0 ⇒ every row; otherwise the requested page (RSCTF caps
    // count at 100 via `[Range(0,100)]`).
    let count = q.count.unwrap_or(100);
    if count > 0 {
        query = query.offset(q.skip.unwrap_or(0)).limit(count.min(100));
    }
    let rows = query.all(&st.db).await?;

    let team_names = team_name_map(&st, rows.iter().map(|e| e.team_id)).await?;
    let user_ids: Vec<Uuid> = rows.iter().filter_map(|e| e.user_id).collect();
    let user_names = user_name_map(&st, user_ids.into_iter()).await?;

    let data = rows
        .into_iter()
        .map(|e| GameEventModel {
            event_type: e.event_type,
            values: e.values,
            time: e.publish_time_utc,
            user: e.user_id.and_then(|u| user_names.get(&u).cloned()),
            team: team_names.get(&e.team_id).cloned(),
        })
        .collect();
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
/// gate via `context_info` (`denyAfterEnded = true`, RSCTF's default); a non-monitor
/// inside `[FreezeTimeUtc, EndTimeUtc)` gets the FROZEN board, keeping post-freeze solves
/// hidden. `count`/`skip` page the ordered list (count omitted or 0 ⇒ every solver).
pub async fn challenge_solvers(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, challenge_id)): Path<(i32, i32)>,
    Query(q): Query<SolversQuery>,
) -> AppResult<RequestResponse<Vec<ChallengeSolverModel>>> {
    let ctx = context_info(&st, &user, id, true).await?;
    let board = build_scoreboard_cached(&st, &ctx.game, user.is_monitor()).await?;

    let mut solvers: Vec<ChallengeSolverModel> = board
        .items
        .iter()
        .filter_map(|item| {
            item.solved_challenges
                .iter()
                .find(|c| c.id == challenge_id)
                .map(|solve| ChallengeSolverModel {
                    rank: item.rank,
                    team_name: item.name.clone(),
                    team_avatar: item.avatar.clone(),
                    user_name: solve.user_name.clone(),
                    submission_type: solve.submission_type,
                    time: solve.time,
                    score: solve.score,
                })
        })
        .collect();
    // RSCTF `.OrderBy(s => s.Time)`.
    solvers.sort_by_key(|solver| solver.time);

    let skip = q.skip.unwrap_or(0) as usize;
    let paged: Vec<ChallengeSolverModel> = match q.count {
        Some(c) if c > 0 => solvers.into_iter().skip(skip).take(c as usize).collect(),
        _ => solvers.into_iter().skip(skip).collect(),
    };
    Ok(RequestResponse::ok(paged))
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

    let mut query = submission::Entity::find().filter(submission::Column::GameId.eq(id));
    if let Some(status) = q.type_filter.as_deref().and_then(parse_answer_result) {
        query = query.filter(submission::Column::Status.eq(status));
    }

    // `search`: RSCTF `SubmissionRepository.GetSubmissions` matches team name / user
    // name / challenge title / answer, applied BEFORE `TakeAllIfZero` (skip/take) so
    // pagination pages over the filtered set. Resolve the name matches to ids up
    // front, then OR them with the answer `LIKE` at SQL level.
    if let Some(term) = q.search.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let team_ids: Vec<i32> = team::Entity::find()
            .filter(team::Column::Name.contains(term))
            .all(&st.db)
            .await?
            .into_iter()
            .map(|t| t.id)
            .collect();
        let user_ids: Vec<Uuid> = user::Entity::find()
            .filter(user::Column::UserName.contains(term))
            .all(&st.db)
            .await?
            .into_iter()
            .map(|u| u.id)
            .collect();
        let chal_ids: Vec<i32> = game_challenge::Entity::find()
            .filter(game_challenge::Column::GameId.eq(id))
            .filter(game_challenge::Column::Title.contains(term))
            .all(&st.db)
            .await?
            .into_iter()
            .map(|c| c.id)
            .collect();
        query = query.filter(
            Condition::any()
                .add(submission::Column::Answer.contains(term))
                .add(submission::Column::TeamId.is_in(team_ids))
                .add(submission::Column::UserId.is_in(user_ids))
                .add(submission::Column::ChallengeId.is_in(chal_ids)),
        );
    }

    query = query.order_by_desc(submission::Column::SubmitTimeUtc);
    // `TakeAllIfZero`: count 0 ⇒ every row; otherwise the requested page (RSCTF caps
    // count at 100 via `[Range(0,100)]`).
    let count = q.count.unwrap_or(100);
    if count > 0 {
        query = query.offset(q.skip.unwrap_or(0)).limit(count.min(100));
    }
    let rows = query.all(&st.db).await?;

    let team_names = team_name_map(&st, rows.iter().map(|s| s.team_id)).await?;
    let user_ids: Vec<Uuid> = rows.iter().filter_map(|s| s.user_id).collect();
    let user_names = user_name_map(&st, user_ids.into_iter()).await?;
    let challenge_titles = challenge_title_map(&st, rows.iter().map(|s| s.challenge_id)).await?;

    let data = rows
        .into_iter()
        .map(|s| SubmissionModel {
            user: s.user_id.and_then(|u| user_names.get(&u).cloned()),
            team: team_names.get(&s.team_id).cloned(),
            challenge: challenge_titles.get(&s.challenge_id).cloned(),
            answer: s.answer,
            status: s.status,
            time: s.submit_time_utc,
        })
        .collect();
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
