//! Scoreboard, notices, events, participations, and the monitor submission feed + Excel exports.
use super::*;
use crate::services::monitor_export::{
    load_submission_export_snapshot, MonitorExportAdmissionError, MonitorExportPermit,
    SubmissionExportRow, SubmissionSnapshotError,
};
use axum::http::HeaderMap;

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
        .clamp(1, MAX_SOLVER_PAGE_SIZE);
    let skip = query.skip.unwrap_or(0);
    if skip > MAX_SOLVER_SKIP {
        return Err(AppError::bad_request("Solver page offset is too large"));
    }
    Ok((skip as usize, count as usize))
}

/// Preserve the original compatibility-route contract. New clients use the
/// separately bounded `/solvers/page` endpoint; omission or zero here means
/// every solver after `skip`, just as it did before that endpoint existed.
pub(super) fn legacy_solver_window(query: &SolversQuery) -> (usize, usize) {
    let skip = usize::try_from(query.skip.unwrap_or(0)).unwrap_or(usize::MAX);
    let count = query
        .count
        .filter(|count| *count > 0)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(usize::MAX);
    (skip, count)
}

const MAX_SCOREBOARD_EXPORT_ROWS: usize = 10_000;
const MAX_SCOREBOARD_EXPORT_SNAPSHOT_BYTES: usize = 32 * 1024 * 1024;
const EXPORT_RETRY_AFTER_SECONDS: u64 = 3;

#[derive(Default)]
struct ScoreboardExportSizeWriter {
    bytes: usize,
    exceeded: bool,
}

impl std::io::Write for ScoreboardExportSizeWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        match self.bytes.checked_add(buffer.len()) {
            Some(bytes) if bytes <= MAX_SCOREBOARD_EXPORT_SNAPSHOT_BYTES => {
                self.bytes = bytes;
                Ok(buffer.len())
            }
            _ => {
                self.exceeded = true;
                Err(std::io::Error::other(
                    "scoreboard export snapshot is too large",
                ))
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn scoreboard_export_snapshot_size(board: &ScoreboardModel) -> AppResult<usize> {
    if board.items.len() > MAX_SCOREBOARD_EXPORT_ROWS {
        return Err(AppError::payload_too_large(format!(
            "Scoreboard export is limited to {MAX_SCOREBOARD_EXPORT_ROWS} teams"
        )));
    }
    let mut writer = ScoreboardExportSizeWriter::default();
    let result = serde_json::to_writer(&mut writer, board);
    if writer.exceeded {
        return Err(AppError::payload_too_large(format!(
            "Scoreboard export snapshot is limited to {} MiB",
            MAX_SCOREBOARD_EXPORT_SNAPSHOT_BYTES / 1024 / 1024
        )));
    }
    result.map_err(|error| AppError::internal(error.to_string()))?;
    Ok(writer.bytes)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportOverloadBody {
    title: &'static str,
    status: u16,
    retry_after: u64,
}

fn export_overload_response(error: MonitorExportAdmissionError) -> Response {
    let (status, title) = match error {
        MonitorExportAdmissionError::Busy => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Spreadsheet export workers are busy; retry shortly",
        ),
        MonitorExportAdmissionError::WeightedCapacity => (
            StatusCode::TOO_MANY_REQUESTS,
            "Spreadsheet export capacity is in use; retry shortly",
        ),
    };
    let mut response = (
        status,
        axum::Json(ExportOverloadBody {
            title,
            status: status.as_u16(),
            retry_after: EXPORT_RETRY_AFTER_SECONDS,
        }),
    )
        .into_response();
    response.headers_mut().insert(
        header::RETRY_AFTER,
        axum::http::HeaderValue::from(EXPORT_RETRY_AFTER_SECONDS),
    );
    response
}

fn begin_monitor_export(
    st: &SharedState,
) -> Result<MonitorExportPermit, MonitorExportAdmissionError> {
    st.monitor_export_admission.try_begin()
}

fn reserve_monitor_export_work(
    permit: &mut MonitorExportPermit,
    rows: usize,
    bytes: usize,
) -> Result<(), MonitorExportAdmissionError> {
    permit.try_reserve_work(rows, bytes)
}

/// Reconnect backfill. Omitting `after` returns a cursor-only checkpoint; a
/// supplied cursor returns the next commit-ordered bounded page.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventBackfillQuery {
    #[serde(default)]
    pub after: Option<i64>,
    #[serde(default = "event_backfill_default_limit")]
    pub limit: i64,
}

fn event_backfill_default_limit() -> i64 {
    crate::services::game_event_feed::MAX_BACKFILL_EVENTS
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
/// event-scoped team/user/value projection before pagination. This compatibility
/// route preserves `TakeAllIfZero`: an explicit zero returns all retained rows.
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

    let data = monitor_history::load_events_legacy(st.pg(), id, &q).await?;
    Ok(RequestResponse::ok(data))
}

/// `GET /api/game/{id}/events/backfill` — monitor-only reconnect recovery.
///
/// With no `after`, this returns a cursor-only checkpoint. With `after`, it
/// returns at most 100 committed events in ascending cursor order and reports
/// whether another bounded page remains.
pub async fn event_backfill(
    State(st): State<SharedState>,
    MonitorUser(_user): MonitorUser,
    Path(id): Path<i32>,
    Query(q): Query<EventBackfillQuery>,
) -> AppResult<RequestResponse<crate::services::game_event_feed::GameEventBackfill>> {
    let start: Option<DateTime<Utc>> = sqlx::query_scalar(
        r#"SELECT start_time_utc FROM "Games" WHERE id = $1 AND deletion_pending = FALSE"#,
    )
    .bind(id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let start = start.ok_or_else(|| AppError::not_found("Game not found"))?;
    if Utc::now() < start {
        return Err(AppError::game_not_started());
    }

    let data = match q.after {
        Some(after) if after < 0 => {
            return Err(AppError::bad_request("Event cursor must not be negative"));
        }
        Some(after) => {
            crate::services::game_event_feed::backfill_after(st.pg(), id, after, q.limit)
                .await
                .map_err(|error| AppError::internal(error.to_string()))?
        }
        None => crate::services::game_event_feed::GameEventBackfill {
            events: Vec::new(),
            next_cursor: crate::services::game_event_feed::latest_cursor(st.pg(), id)
                .await
                .map_err(|error| AppError::internal(error.to_string()))?,
            has_more: false,
        },
    };
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
    headers: HeaderMap,
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

    let bundle = build_scoreboard_bundle(&st, &g, is_monitor).await?;
    let validator_scope = if is_monitor {
        "standard-monitor"
    } else {
        "standard-public"
    };
    scoreboard_encoding::scoped_response(bundle, &headers, validator_scope)
}

/// `GET /api/game/{id}/challenges/{challengeId}/solvers` — teams that solved one
/// challenge, ordered by solve time. Mirrors RSCTF `GameController.GetChallengeSolvers`:
/// a projection of the (freeze-aware) scoreboard. For each team whose `solvedChallenges`
/// holds `challengeId`, emit a `ChallengeSolverModel` (rank/team/avatar from the item,
/// userName/type/time/score from the solved cell). `RequireUser` + Accepted-participant
/// gate remains in force after closeout so the read-only challenge archive can show
/// final solvers. A non-monitor
/// inside `[FreezeTimeUtc, EndTimeUtc)` gets the FROZEN board, keeping post-freeze solves
/// hidden. For compatibility, an omitted or zero `count` returns every solver after
/// `skip`; clients that need a bounded response use `/solvers/page`.
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
    let (skip, count) = legacy_solver_window(&q);
    let board = build_scoreboard_cached(&st, &ctx.game, user.is_monitor()).await?;

    // Retain only compact board indexes until the final projection. The legacy
    // all-solvers contract necessarily scales with the matching roster, but it
    // does not clone every compatibility DTO before applying `skip`/`count`.
    let mut solver_positions = Vec::new();
    for (item_index, item) in board.items.iter().enumerate() {
        let Some(solve_index) = item
            .solved_challenges
            .iter()
            .position(|solve| solve.id == challenge_id)
        else {
            continue;
        };
        solver_positions.push((
            item.solved_challenges[solve_index].time,
            item_index,
            solve_index,
        ));
    }
    // Stable ordering preserves the historical scoreboard order for exact-time ties.
    solver_positions.sort_by_key(|position| position.0);

    let paged = solver_positions
        .into_iter()
        .skip(skip)
        .take(count)
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

pub(super) struct ChallengeSolverPreviewRow {
    pub(super) team_name: String,
    team_avatar_hash: Option<String>,
    user_name: Option<String>,
    submit_time_utc: DateTime<Utc>,
    blood_eligible: bool,
    blood_position: i64,
}

#[derive(sqlx::FromRow)]
struct ChallengeSolverPreviewQueryRow {
    has_solver: bool,
    team_name: Option<String>,
    team_avatar_hash: Option<String>,
    user_name: Option<String>,
    submit_time_utc: Option<DateTime<Utc>>,
    blood_eligible: Option<bool>,
    blood_position: Option<i64>,
    total: i64,
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
), paged AS (
    SELECT *
      FROM numbered
     ORDER BY submit_time_utc, participation_id
     LIMIT $10 OFFSET $11
), totals AS (
    SELECT COUNT(*)::bigint AS total FROM solver_base
)
SELECT paged.participation_id IS NOT NULL AS has_solver,
       paged.team_name, paged.team_avatar_hash, paged.user_name,
       paged.submit_time_utc, paged.blood_eligible, paged.blood_position,
       totals.total
  FROM totals
  LEFT JOIN paged ON TRUE
 ORDER BY paged.submit_time_utc, paged.participation_id
"#;

pub(super) async fn load_challenge_solver_page(
    pool: &sqlx::PgPool,
    game_id: i32,
    challenge_id: i32,
    cutoff: Option<DateTime<Utc>>,
    skip: usize,
    count: usize,
) -> AppResult<(i64, Vec<ChallengeSolverPreviewRow>)> {
    let query_rows: Vec<ChallengeSolverPreviewQueryRow> = sqlx::query_as(CHALLENGE_SOLVER_PAGE_SQL)
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
        .map_err(|error| AppError::internal(error.to_string()))?;
    let total = query_rows
        .first()
        .ok_or_else(|| AppError::internal("solver page query returned no aggregate row"))?
        .total;
    let mut rows = Vec::with_capacity(query_rows.len().min(count));
    for row in query_rows {
        if !row.has_solver {
            continue;
        }
        rows.push(ChallengeSolverPreviewRow {
            team_name: row
                .team_name
                .ok_or_else(|| AppError::internal("solver page row has no team name"))?,
            team_avatar_hash: row.team_avatar_hash,
            user_name: row.user_name,
            submit_time_utc: row
                .submit_time_utc
                .ok_or_else(|| AppError::internal("solver page row has no submit time"))?,
            blood_eligible: row
                .blood_eligible
                .ok_or_else(|| AppError::internal("solver page row has no blood eligibility"))?,
            blood_position: row
                .blood_position
                .ok_or_else(|| AppError::internal("solver page row has no blood position"))?,
        });
    }
    Ok((total, rows))
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
            let (total, rows) =
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

    let mut export_permit = match begin_monitor_export(&st) {
        Ok(permit) => permit,
        Err(error) => return Ok(export_overload_response(error)),
    };
    let bounded_count: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)::bigint
             FROM (
               SELECT participation.id
                 FROM "Participations" participation
                WHERE participation.game_id = $1 AND participation.status = $2
                ORDER BY participation.id
                LIMIT $3
             ) bounded"#,
    )
    .bind(id)
    .bind(ParticipationStatus::Accepted as i16)
    .bind(i64::try_from(MAX_SCOREBOARD_EXPORT_ROWS + 1).unwrap_or(i64::MAX))
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if bounded_count > MAX_SCOREBOARD_EXPORT_ROWS as i64 {
        return Err(AppError::payload_too_large(format!(
            "Scoreboard export is limited to {MAX_SCOREBOARD_EXPORT_ROWS} teams"
        )));
    }
    // Charge the maximum bounded scoreboard before loading the model. This
    // remains safe if a participation is accepted between the count and the
    // cache fill, and two small scoreboards may still use both task slots.
    if let Err(error) = reserve_monitor_export_work(
        &mut export_permit,
        MAX_SCOREBOARD_EXPORT_ROWS,
        MAX_SCOREBOARD_EXPORT_SNAPSHOT_BYTES,
    ) {
        return Ok(export_overload_response(error));
    }

    // Monitor-only export: always build the live (unfrozen) model directly.
    // The public scoreboard's 8 MiB wire/cache limit must not override this
    // endpoint's separately admitted 32 MiB snapshot contract.
    let board = build_scoreboard(&st, &g, true).await?;
    let bytes = build_scoreboard_xlsx_off_thread(board, export_permit).await?;

    let filename = format!(
        "{}-Scoreboard-{}.xlsx",
        sanitize_filename(&g.title),
        Utc::now().format("%Y%m%d-%H.%M.%SZ")
    );
    Ok(xlsx_response(bytes, &filename))
}

/// Build the scoreboard `.xlsx` in memory (rank / team / score / solved).
fn build_scoreboard_xlsx(board: ScoreboardModel) -> Result<Vec<u8>, rust_xlsxwriter::XlsxError> {
    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet();
    sheet.set_name("Scoreboard")?;
    for (col, h) in ["Ranking", "Team", "Score", "Solved"].iter().enumerate() {
        sheet.write_string(0, col as u16, *h)?;
    }
    for (i, item) in board.items.into_iter().enumerate() {
        let row = (i + 1) as u32;
        sheet.write_number(row, 0, item.rank as f64)?;
        sheet.write_string(row, 1, item.name)?;
        sheet.write_number(row, 2, item.score as f64)?;
        sheet.write_number(row, 3, item.solved_count as f64)?;
    }
    workbook.save_to_buffer()
}

async fn build_scoreboard_xlsx_off_thread(
    board: ScoreboardModel,
    permit: MonitorExportPermit,
) -> AppResult<Vec<u8>> {
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        scoreboard_export_snapshot_size(&board)?;
        build_scoreboard_xlsx(board)
            .map_err(|_| AppError::bad_request("Failed to build scoreboard sheet"))
    })
    .await
    .map_err(|error| AppError::internal(format!("spreadsheet task failed: {error}")))?
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
) -> AppResult<RequestResponse<Vec<MonitorSubmissionModel>>> {
    let _ = load_game(&st, id).await?;

    let status = q.type_filter.as_deref().and_then(parse_answer_result);
    let data = monitor_history::load_submissions_legacy(st.pg(), id, &q, status).await?;
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

    let mut export_permit = match begin_monitor_export(&st) {
        Ok(permit) => permit,
        Err(error) => return Ok(export_overload_response(error)),
    };
    let rows = match load_submission_export_snapshot(st.pg(), id, &mut export_permit).await {
        Ok(rows) => rows,
        Err(SubmissionSnapshotError::Application(error)) => return Err(error),
        Err(SubmissionSnapshotError::Overloaded(error)) => {
            return Ok(export_overload_response(error));
        }
    };
    let bytes = build_xlsx_off_thread(
        rows,
        export_permit,
        build_submission_xlsx,
        "Failed to build submission sheet",
    )
    .await?;

    let filename = format!(
        "{}_Submissions_{}.xlsx",
        sanitize_filename(&g.title),
        Utc::now().format("%Y%m%d%H%M%S")
    );
    Ok(xlsx_response(bytes, &filename))
}

/// Build the submissions `.xlsx` in memory (time / team / user / challenge /
/// answer / status), one row per pre-projected submission.
fn build_submission_xlsx(
    rows: Vec<SubmissionExportRow>,
) -> Result<Vec<u8>, rust_xlsxwriter::XlsxError> {
    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet();
    sheet.set_name("Submissions")?;
    for (col, h) in ["Time", "Team", "User", "Challenge", "Answer", "Status"]
        .iter()
        .enumerate()
    {
        sheet.write_string(0, col as u16, *h)?;
    }
    for (i, submission) in rows.into_iter().enumerate() {
        let row = (i + 1) as u32;
        sheet.write_string(
            row,
            0,
            submission
                .submit_time_utc
                .format("%Y-%m-%d %H:%M:%SZ")
                .to_string(),
        )?;
        sheet.write_string(row, 1, submission.team_name.unwrap_or_default())?;
        sheet.write_string(row, 2, submission.user_name.unwrap_or_default())?;
        sheet.write_string(row, 3, submission.challenge_title.unwrap_or_default())?;
        sheet.write_string(row, 4, submission.answer)?;
        sheet.write_string(row, 5, answer_result_str(submission.status))?;
    }
    workbook.save_to_buffer()
}

/// Human-readable label for an `AnswerResult`, mirroring RSCTF `ToShortString`.
fn answer_result_str(r: i16) -> &'static str {
    match r {
        value if value == AnswerResult::NotFound as i16 => "Not Found",
        value if value == AnswerResult::FlagSubmitted as i16 => "Submitted",
        value if value == AnswerResult::Accepted as i16 => "Accepted",
        value if value == AnswerResult::WrongAnswer as i16 => "Wrong Answer",
        value if value == AnswerResult::CheatDetected as i16 => "Cheat Detected",
        _ => "Unknown",
    }
}

/// Keep all XLSX serialization off Tokio request workers. The snapshot and its
/// admission permit remain owned until the blocking task has fully completed.
async fn build_xlsx_off_thread<T, F>(
    snapshot: T,
    permit: MonitorExportPermit,
    builder: F,
    public_error: &'static str,
) -> AppResult<Vec<u8>>
where
    T: Send + 'static,
    F: FnOnce(T) -> Result<Vec<u8>, rust_xlsxwriter::XlsxError> + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        // The task, rather than the request future, owns admission. If a client
        // disconnects and Axum drops the handler while rust_xlsxwriter is still
        // running, the detached blocking task remains counted until completion.
        let _permit = permit;
        builder(snapshot)
    })
    .await
    .map_err(|error| AppError::internal(format!("spreadsheet task failed: {error}")))?
    .map_err(|_| AppError::bad_request(public_error))
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

#[cfg(test)]
mod export_tests;

#[cfg(test)]
#[path = "event_feed_tests.rs"]
mod event_feed_tests;
