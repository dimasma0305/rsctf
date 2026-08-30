//! Cheat detection: immutable flag-sharing evidence + collusion (RSI) reporting.
#[cfg(test)]
use super::cheat_compare::canonical_solves_bounded;
use super::cheat_compare::{canonical_report_solves, collusion_metrics, CanonicalSolveRow};
use super::*;

#[derive(Debug, Default, sqlx::FromRow)]
struct ReconciliationReportState {
    evidence_closed_at: Option<DateTime<Utc>>,
    last_reconciled_at: Option<DateTime<Utc>>,
    sealed_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
    pending_jobs: i64,
    oldest_pending_at: Option<DateTime<Utc>>,
}

/// Load the persisted evaluator watermark and only the jobs that can still
/// affect this game's competitive snapshot. Response generation is not an
/// evaluation, so the monitor must never use its wall clock as freshness.
async fn load_reconciliation_report_state(
    pool: &sqlx::PgPool,
    game_id: i32,
) -> AppResult<ReconciliationReportState> {
    sqlx::query_as::<_, ReconciliationReportState>(
        r#"SELECT reconciliation.evidence_closed_at_utc AS evidence_closed_at,
                  reconciliation.last_reconciled_at_utc AS last_reconciled_at,
                  reconciliation.sealed_at_utc AS sealed_at,
                  COALESCE(reconciliation.last_error, pending.pending_error) AS last_error,
                  COALESCE(pending.pending_jobs, 0)::bigint AS pending_jobs,
                  pending.oldest_pending_at
             FROM "Games" game
             LEFT JOIN "SuspicionReconciliationState" reconciliation
               ON reconciliation.game_id = game.id
             LEFT JOIN LATERAL (
               SELECT COUNT(*)::bigint AS pending_jobs,
                      MIN(job.observed_at_utc) AS oldest_pending_at,
                      (ARRAY_AGG(job.last_error ORDER BY job.observed_at_utc, job.id)
                         FILTER (WHERE job.last_error IS NOT NULL))[1] AS pending_error
                 FROM "SuspicionEvaluationOutbox" job
                WHERE job.game_id = game.id
                  AND job.completed_at_utc IS NULL
                  AND job.observed_at_utc >= game.start_time_utc
                  AND job.observed_at_utc < game.end_time_utc
             ) pending ON TRUE
            WHERE game.id = $1"#,
    )
    .bind(game_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
    .map(|state| state.unwrap_or_default())
}

// ---------------------------------------------------------------------------
// Canonical flag-sharing evidence.
// ---------------------------------------------------------------------------

/// One immutable `CheatInfo` incident joined to its presentation data. The
/// identifiers and observation time come from the submit-time audit row. Names
/// come from its versioned JSON snapshot (with a legacy fallback); avatars and
/// participation status remain presentation-only.
#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct CheatIncidentRow {
    /// Game-wide incident-id high-water mark from the same query snapshot.
    /// Legacy loaders populate this with zero because they never expose a
    /// reconnect checkpoint.
    pub(crate) checkpoint_id: i32,
    pub(crate) incident_id: i32,
    pub(crate) observed_at_utc: DateTime<Utc>,
    pub(crate) source_participation_id: i32,
    pub(crate) source_team_id: i32,
    pub(crate) source_team_name: String,
    pub(crate) source_avatar_hash: Option<String>,
    pub(crate) source_status: i16,
    pub(crate) source_division_id: Option<i32>,
    pub(crate) source_division_name: Option<String>,
    pub(crate) submit_participation_id: i32,
    pub(crate) submit_team_id: i32,
    pub(crate) submit_team_name: String,
    pub(crate) submit_avatar_hash: Option<String>,
    pub(crate) submit_status: i16,
    pub(crate) submit_division_id: Option<i32>,
    pub(crate) submit_division_name: Option<String>,
    pub(crate) answer: String,
    pub(crate) answer_status: i16,
    pub(crate) submit_time_utc: DateTime<Utc>,
    pub(crate) user_name: Option<String>,
    pub(crate) challenge_title: String,
}

pub(crate) fn cheat_participation_status(value: i16) -> AppResult<ParticipationStatus> {
    match value {
        value if value == ParticipationStatus::Pending as i16 => Ok(ParticipationStatus::Pending),
        value if value == ParticipationStatus::Accepted as i16 => Ok(ParticipationStatus::Accepted),
        value if value == ParticipationStatus::Rejected as i16 => Ok(ParticipationStatus::Rejected),
        value if value == ParticipationStatus::Suspended as i16 => {
            Ok(ParticipationStatus::Suspended)
        }
        value if value == ParticipationStatus::Unsubmitted as i16 => {
            Ok(ParticipationStatus::Unsubmitted)
        }
        _ => Err(AppError::internal(
            "invalid participation status in cheat evidence",
        )),
    }
}

pub(crate) fn cheat_answer_result(value: i16) -> AppResult<AnswerResult> {
    match value {
        value if value == AnswerResult::NotFound as i16 => Ok(AnswerResult::NotFound),
        value if value == AnswerResult::FlagSubmitted as i16 => Ok(AnswerResult::FlagSubmitted),
        value if value == AnswerResult::Accepted as i16 => Ok(AnswerResult::Accepted),
        value if value == AnswerResult::WrongAnswer as i16 => Ok(AnswerResult::WrongAnswer),
        value if value == AnswerResult::CheatDetected as i16 => Ok(AnswerResult::CheatDetected),
        _ => Err(AppError::internal(
            "invalid answer status in cheat evidence",
        )),
    }
}

pub(crate) fn cheat_avatar_url(hash: &Option<String>) -> Option<String> {
    hash.as_ref().map(|hash| format!("/assets/{hash}/avatar"))
}

impl CheatIncidentRow {
    fn into_model(self) -> AppResult<CheatInfoModel> {
        Ok(CheatInfoModel {
            owned_team: ParticipationModel {
                id: self.source_participation_id,
                team: TeamModel {
                    id: self.source_team_id,
                    name: Some(self.source_team_name),
                    avatar: cheat_avatar_url(&self.source_avatar_hash),
                },
                status: cheat_participation_status(self.source_status)?,
                division: self.source_division_name,
                division_id: self.source_division_id,
            },
            submit_team: ParticipationModel {
                id: self.submit_participation_id,
                team: TeamModel {
                    id: self.submit_team_id,
                    name: Some(self.submit_team_name.clone()),
                    avatar: cheat_avatar_url(&self.submit_avatar_hash),
                },
                status: cheat_participation_status(self.submit_status)?,
                division: self.submit_division_name,
                division_id: self.submit_division_id,
            },
            submission: SubmissionModel {
                answer: self.answer,
                status: cheat_answer_result(self.answer_status)?,
                time: self.submit_time_utc,
                user: self.user_name,
                team: Some(self.submit_team_name),
                challenge: Some(self.challenge_title),
            },
        })
    }

    fn into_page_item(self) -> AppResult<CheatIncidentPageItem> {
        let id = self.incident_id;
        let observed_at = self.observed_at_utc;
        Ok(CheatIncidentPageItem {
            id,
            observed_at,
            incident: self.into_model()?,
        })
    }
}

/// Load immutable flag-sharing incidents. `game_id = None` is the admin-global
/// feed; monitor reads pass a game id. Every request handler supplies a finite
/// limit; a null limit remains only for focused internal/fixture inspection.
pub(crate) async fn load_cheat_incident_rows(
    pool: &sqlx::PgPool,
    game_id: Option<i32>,
    limit: Option<i64>,
    offset: i64,
) -> AppResult<Vec<CheatIncidentRow>> {
    sqlx::query_as::<_, CheatIncidentRow>(
        r#"SELECT 0::INTEGER AS checkpoint_id,
                  cheat.id AS incident_id,
                  cheat.observed_at_utc,
                  cheat.source_participation_id,
                  source_part.team_id AS source_team_id,
                  COALESCE(
                      NULLIF(cheat.evidence_payload->>'sourceTeamName', ''),
                      source_team.name
                  ) AS source_team_name,
                  source_team.avatar_hash AS source_avatar_hash,
                  source_part.status AS source_status,
                  source_part.division_id AS source_division_id,
                  source_division.name AS source_division_name,
                  cheat.submit_participation_id,
                  submit_part.team_id AS submit_team_id,
                  COALESCE(
                      NULLIF(cheat.evidence_payload->>'submitTeamName', ''),
                      submit_team.name
                  ) AS submit_team_name,
                  submit_team.avatar_hash AS submit_avatar_hash,
                  submit_part.status AS submit_status,
                  submit_part.division_id AS submit_division_id,
                  submit_division.name AS submit_division_name,
                  submission.answer,
                  submission.status AS answer_status,
                  submission.submit_time_utc,
                  COALESCE(
                      NULLIF(cheat.evidence_payload->>'submitUserName', ''),
                      account.user_name
                  ) AS user_name,
                  COALESCE(
                      NULLIF(cheat.evidence_payload->>'challengeTitle', ''),
                      challenge.title
                  ) AS challenge_title
             FROM "CheatInfo" cheat
             JOIN "Games" game ON game.id = cheat.game_id
             JOIN "Submissions" submission
               ON submission.id = cheat.submission_id
              AND submission.game_id = cheat.game_id
              AND submission.participation_id = cheat.submit_participation_id
              AND submission.challenge_id = cheat.challenge_id
             JOIN "Participations" source_part
               ON source_part.id = cheat.source_participation_id
              AND source_part.game_id = cheat.game_id
             JOIN "Teams" source_team ON source_team.id = source_part.team_id
        LEFT JOIN "Divisions" source_division
               ON source_division.id = source_part.division_id
              AND source_division.game_id = cheat.game_id
             JOIN "Participations" submit_part
               ON submit_part.id = cheat.submit_participation_id
              AND submit_part.game_id = cheat.game_id
             JOIN "Teams" submit_team ON submit_team.id = submit_part.team_id
        LEFT JOIN "Divisions" submit_division
               ON submit_division.id = submit_part.division_id
              AND submit_division.game_id = cheat.game_id
             JOIN "GameChallenges" challenge
               ON challenge.id = cheat.challenge_id
              AND challenge.game_id = cheat.game_id
        LEFT JOIN "AspNetUsers" account ON account.id = submission.user_id
            WHERE ($1::INTEGER IS NULL OR cheat.game_id = $1)
              AND cheat.observed_at_utc = submission.submit_time_utc
              AND cheat.observed_at_utc >= game.start_time_utc
              AND cheat.observed_at_utc < game.end_time_utc
            ORDER BY cheat.observed_at_utc DESC, cheat.id DESC
            LIMIT $2 OFFSET $3"#,
    )
    .bind(game_id)
    .bind(limit)
    .bind(offset.max(0))
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

const CHEAT_INCIDENT_PAGE_DEFAULT: u64 = 50;
const CHEAT_INCIDENT_PAGE_MAX: u64 = 100;

/// Stable cursor for the descending immutable incident history.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheatIncidentCursor {
    #[serde(with = "crate::utils::datetime::millis")]
    pub observed_at: DateTime<Utc>,
    pub id: i32,
}

/// One bounded incident-feed row. `flatten` preserves the legacy incident
/// fields while adding the immutable identity required for reconnects.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheatIncidentPageItem {
    pub id: i32,
    #[serde(with = "crate::utils::datetime::millis")]
    pub observed_at: DateTime<Utc>,
    #[serde(flatten)]
    pub incident: CheatInfoModel,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheatIncidentPage {
    pub data: Vec<CheatIncidentPageItem>,
    pub next_before: Option<CheatIncidentCursor>,
    pub checkpoint_id: i32,
    pub has_more: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheatInfoPageQuery {
    #[serde(default)]
    pub limit: Option<u64>,
    #[serde(default)]
    pub before_observed_at: Option<i64>,
    #[serde(default)]
    pub before_id: Option<i32>,
    #[serde(default)]
    pub after_id: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheatIncidentWindow {
    Descending {
        before_observed_at: Option<i64>,
        before_id: Option<i32>,
    },
    Delta {
        after_id: i32,
    },
}

fn bounded_cheat_incident_window(
    query: &CheatInfoPageQuery,
) -> AppResult<(i64, CheatIncidentWindow)> {
    let limit = query
        .limit
        .unwrap_or(CHEAT_INCIDENT_PAGE_DEFAULT)
        .clamp(1, CHEAT_INCIDENT_PAGE_MAX) as i64;
    if query.after_id.is_some() && (query.before_observed_at.is_some() || query.before_id.is_some())
    {
        return Err(AppError::bad_request(
            "afterId cannot be combined with a before cursor",
        ));
    }
    if query.before_observed_at.is_some() != query.before_id.is_some() {
        return Err(AppError::bad_request(
            "beforeObservedAt and beforeId must be supplied together",
        ));
    }
    if let Some(after_id) = query.after_id {
        if after_id < 0 {
            return Err(AppError::bad_request("afterId must not be negative"));
        }
        return Ok((limit, CheatIncidentWindow::Delta { after_id }));
    }
    if query
        .before_observed_at
        .is_some_and(|millis| DateTime::from_timestamp_millis(millis).is_none())
    {
        return Err(AppError::bad_request("beforeObservedAt is out of range"));
    }
    if query.before_id.is_some_and(|id| id < 0) {
        return Err(AppError::bad_request("beforeId must not be negative"));
    }
    Ok((
        limit,
        CheatIncidentWindow::Descending {
            before_observed_at: query.before_observed_at,
            before_id: query.before_id,
        },
    ))
}

async fn load_cheat_incident_page_rows(
    pool: &sqlx::PgPool,
    game_id: i32,
    window: CheatIncidentWindow,
    limit: i64,
) -> AppResult<Vec<CheatIncidentRow>> {
    let (after_id, before_observed_at, before_id) = match window {
        CheatIncidentWindow::Descending {
            before_observed_at,
            before_id,
        } => (None, before_observed_at, before_id),
        CheatIncidentWindow::Delta { after_id } => (Some(after_id), None, None),
    };
    sqlx::query_as::<_, CheatIncidentRow>(
        r#"WITH checkpoint AS (
               SELECT COALESCE(MAX(candidate.id), 0)::INTEGER AS checkpoint_id
                 FROM "CheatInfo" candidate
                WHERE candidate.game_id = $1
           )
           SELECT checkpoint.checkpoint_id,
                  cheat.id AS incident_id,
                  cheat.observed_at_utc,
                  cheat.source_participation_id,
                  source_part.team_id AS source_team_id,
                  COALESCE(NULLIF(cheat.evidence_payload->>'sourceTeamName', ''),
                           source_team.name) AS source_team_name,
                  source_team.avatar_hash AS source_avatar_hash,
                  source_part.status AS source_status,
                  source_part.division_id AS source_division_id,
                  source_division.name AS source_division_name,
                  cheat.submit_participation_id,
                  submit_part.team_id AS submit_team_id,
                  COALESCE(NULLIF(cheat.evidence_payload->>'submitTeamName', ''),
                           submit_team.name) AS submit_team_name,
                  submit_team.avatar_hash AS submit_avatar_hash,
                  submit_part.status AS submit_status,
                  submit_part.division_id AS submit_division_id,
                  submit_division.name AS submit_division_name,
                  submission.answer,
                  submission.status AS answer_status,
                  submission.submit_time_utc,
                  COALESCE(NULLIF(cheat.evidence_payload->>'submitUserName', ''),
                           account.user_name) AS user_name,
                  COALESCE(NULLIF(cheat.evidence_payload->>'challengeTitle', ''),
                           challenge.title) AS challenge_title
             FROM "CheatInfo" cheat
       CROSS JOIN checkpoint
             JOIN "Games" game ON game.id = cheat.game_id
             JOIN "Submissions" submission
               ON submission.id = cheat.submission_id
              AND submission.game_id = cheat.game_id
              AND submission.participation_id = cheat.submit_participation_id
              AND submission.challenge_id = cheat.challenge_id
             JOIN "Participations" source_part
               ON source_part.id = cheat.source_participation_id
              AND source_part.game_id = cheat.game_id
             JOIN "Teams" source_team ON source_team.id = source_part.team_id
        LEFT JOIN "Divisions" source_division
               ON source_division.id = source_part.division_id
              AND source_division.game_id = cheat.game_id
             JOIN "Participations" submit_part
               ON submit_part.id = cheat.submit_participation_id
              AND submit_part.game_id = cheat.game_id
             JOIN "Teams" submit_team ON submit_team.id = submit_part.team_id
        LEFT JOIN "Divisions" submit_division
               ON submit_division.id = submit_part.division_id
              AND submit_division.game_id = cheat.game_id
             JOIN "GameChallenges" challenge
               ON challenge.id = cheat.challenge_id
              AND challenge.game_id = cheat.game_id
        LEFT JOIN "AspNetUsers" account ON account.id = submission.user_id
            WHERE cheat.game_id = $1
              AND cheat.observed_at_utc = submission.submit_time_utc
              AND cheat.observed_at_utc >= game.start_time_utc
              AND cheat.observed_at_utc < game.end_time_utc
              AND (($2::INTEGER IS NOT NULL AND cheat.id > $2)
                   OR ($2::INTEGER IS NULL
                       AND ($4::INTEGER IS NULL
                            OR (cheat.observed_at_utc, cheat.id) < (
                                SELECT cursor.observed_at_utc, cursor.id
                                  FROM "CheatInfo" cursor
                                 WHERE cursor.game_id = $1
                                   AND cursor.id = $4
                                   AND FLOOR(EXTRACT(EPOCH FROM cursor.observed_at_utc) * 1000)::BIGINT = $3
                            ))))
            ORDER BY CASE WHEN $2::INTEGER IS NOT NULL THEN cheat.id END ASC,
                     CASE WHEN $2::INTEGER IS NULL THEN cheat.observed_at_utc END DESC,
                     CASE WHEN $2::INTEGER IS NULL THEN cheat.id END DESC
            LIMIT $5"#,
    )
    .bind(game_id)
    .bind(after_id)
    .bind(before_observed_at)
    .bind(before_id)
    .bind(limit.saturating_add(1))
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

/// Bounded immutable incident history and reconnect delta feed.
pub async fn cheat_info_page(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path(id): Path<i32>,
    Query(query): Query<CheatInfoPageQuery>,
) -> AppResult<RequestResponse<CheatIncidentPage>> {
    let _ = load_game(&st, id).await?;
    let (limit, window) = bounded_cheat_incident_window(&query)?;
    let mut rows = load_cheat_incident_page_rows(st.pg(), id, window, limit).await?;
    let has_more = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    let snapshot_checkpoint_id = rows
        .first()
        .map(|row| row.checkpoint_id)
        .or(query.after_id)
        .unwrap_or(0);
    // A full page of ascending deltas may stop before the query snapshot's
    // high-water mark. Advance only through the emitted rows so the next delta
    // cannot skip the remainder; every other response publishes the whole-game
    // snapshot checkpoint selected above.
    let checkpoint_id = if has_more && matches!(window, CheatIncidentWindow::Delta { .. }) {
        rows.last()
            .map(|row| row.incident_id)
            .unwrap_or(snapshot_checkpoint_id)
    } else {
        snapshot_checkpoint_id
    };
    let next_before = if has_more && matches!(window, CheatIncidentWindow::Descending { .. }) {
        rows.last().map(|row| CheatIncidentCursor {
            observed_at: row.observed_at_utc,
            id: row.incident_id,
        })
    } else {
        None
    };
    let data = rows
        .into_iter()
        .map(CheatIncidentRow::into_page_item)
        .collect::<AppResult<Vec<_>>>()?;
    Ok(RequestResponse::ok(CheatIncidentPage {
        data,
        next_before,
        checkpoint_id,
        has_more,
    }))
}

/// `GET /api/game/{id}/cheatinfo` — requires Monitor.
///
/// Flag ownership is captured transactionally at submit time in `CheatInfo`; this
/// read never consults mutable instance/flag state. The compatibility array is
/// capped to the newest 100 incidents; new clients use [`cheat_info_page`].
pub async fn cheat_info(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<Vec<CheatInfoModel>>> {
    let _ = load_game(&st, id).await?;
    let results = collect_cheat_incidents(&st, id).await?;
    Ok(RequestResponse::ok(results))
}

/// Reconstruct flag-sharing incidents for a game — shared by `cheatinfo` (raw
/// list) and `cheatreport` (grouped into collusion groups). Detection strategy is
/// documented on [`cheat_info`].
async fn collect_cheat_incidents(st: &SharedState, id: i32) -> AppResult<Vec<CheatInfoModel>> {
    load_cheat_incident_rows(st.pg(), Some(id), Some(CHEAT_INCIDENT_PAGE_MAX as i64), 0)
        .await?
        .into_iter()
        .map(CheatIncidentRow::into_model)
        .collect()
}

/// `GET /api/game/{id}/cheatreport` — requires Monitor.
///
/// Aggregates immutable flag-sharing incidents (see `cheat_info`) into
/// the report shape: incidents are grouped by the unordered {ownerTeam,
/// submitTeam} pair, and each group is scored with the same RSI-style
/// solved-challenge overlap the `compare` endpoint uses.
///
/// Ported from RSCTF `CheatReportController.Get`, adapted to rsctf's data model:
/// - `suspicionList` — the persisted `suspicion_event` rows for the game,
///   grouped by participation and passed through the tiered fair-scoring
///   [`compute_breakdown`] (total score + risk band + per-event tier/counted).
/// - `identityOverlaps` / `ipAnalysis` — cross-team fingerprint/IP correlation
///   from game-scoped, append-only login observations.
/// - `abnormalSolves` — a projection of already-persisted abnormal-solve events.
///
/// This GET is deliberately read-only. Detector sweeps run in the background
/// reconciler; refreshing the report cannot create evidence or change scores.
pub async fn cheat_report(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path(id): Path<i32>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let _ = load_game(&st, id).await?;
    super::cheat_report_cache::serve_report(&st, id, &headers).await
}

const MAX_REPORT_INCIDENTS: i64 = 2_000;
const MAX_REPORT_PAIRS: usize = 100;
const MAX_REPORT_LCS_CELLS: usize = 8_000_000;

type TeamRef = (i32, i32, String); // (participation_id, team_id, team_name)

fn incident_pairs(incidents: &[CheatInfoModel]) -> BTreeMap<(i32, i32), (TeamRef, TeamRef)> {
    let mut pairs = BTreeMap::new();
    for incident in incidents {
        let name_of = |participation: &ParticipationModel| {
            participation
                .team
                .name
                .clone()
                .unwrap_or_else(|| "Unknown".to_string())
        };
        let left = (
            incident.owned_team.id,
            incident.owned_team.team.id,
            name_of(&incident.owned_team),
        );
        let right = (
            incident.submit_team.id,
            incident.submit_team.team.id,
            name_of(&incident.submit_team),
        );
        let (first, second) = if left.0 <= right.0 {
            (left, right)
        } else {
            (right, left)
        };
        pairs.entry((first.0, second.0)).or_insert((first, second));
    }
    pairs
}

fn validate_report_pair_count(pairs: &BTreeMap<(i32, i32), (TeamRef, TeamRef)>) -> AppResult<()> {
    if pairs.len() > MAX_REPORT_PAIRS {
        return Err(AppError::payload_too_large(format!(
            "Anti-cheat report is limited to {MAX_REPORT_PAIRS} collusion pairs"
        )));
    }
    Ok(())
}

fn build_collusion_groups(
    pairs: BTreeMap<(i32, i32), (TeamRef, TeamRef)>,
    solves: Vec<CanonicalSolveRow>,
) -> AppResult<Vec<Json>> {
    let titles: HashMap<i32, String> = solves
        .iter()
        .map(|solve| (solve.challenge_id, solve.challenge_title.clone()))
        .collect();
    let mut by_part = HashMap::<i32, Vec<CanonicalSolveRow>>::new();
    for solve in solves {
        by_part
            .entry(solve.participation_id)
            .or_default()
            .push(solve);
    }
    let lcs_cells = pairs.values().try_fold(0_usize, |total, (first, second)| {
        let left = by_part.get(&first.0).map_or(0, Vec::len);
        let right = by_part.get(&second.0).map_or(0, Vec::len);
        total.checked_add(left.saturating_mul(right))
    });
    if !matches!(lcs_cells, Some(cells) if cells <= MAX_REPORT_LCS_CELLS) {
        return Err(AppError::payload_too_large(format!(
            "Anti-cheat report pair comparison is limited to {MAX_REPORT_LCS_CELLS} LCS cells"
        )));
    }
    let empty = Vec::new();
    let mut groups = Vec::with_capacity(pairs.len());
    for (first, second) in pairs.into_values() {
        let sub_a = by_part.get(&first.0).unwrap_or(&empty);
        let sub_b = by_part.get(&second.0).unwrap_or(&empty);
        let (rsi, common, detailed) = collusion_metrics(sub_a, sub_b, &titles);
        let details = format!(
            "Flag-sharing detected between team '{}' and team '{}': {} common solved challenge(s), {:.1}% solve-sequence similarity.",
            first.2,
            second.2,
            common.len(),
            rsi * 100.0
        );
        groups.push(serde_json::json!({
            "teams": [
                { "id": first.1, "name": first.2, "participationId": first.0 },
                { "id": second.1, "name": second.2, "participationId": second.0 },
            ],
            "averageRsi": rsi,
            "commonSolves": common,
            "details": details,
            "detailedSolves": detailed,
        }));
    }
    groups.sort_by(|left, right| {
        let left_rsi = left["averageRsi"].as_f64().unwrap_or(0.0);
        let right_rsi = right["averageRsi"].as_f64().unwrap_or(0.0);
        right_rsi
            .partial_cmp(&left_rsi)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(groups)
}

pub(super) async fn build_cheat_report(st: &SharedState, id: i32) -> AppResult<CheatReport> {
    let mut incident_rows = load_cheat_incident_rows(
        st.pg(),
        Some(id),
        Some(MAX_REPORT_INCIDENTS.saturating_add(1)),
        0,
    )
    .await?;
    if incident_rows.len() > MAX_REPORT_INCIDENTS as usize {
        return Err(AppError::payload_too_large(format!(
            "Anti-cheat report is limited to {MAX_REPORT_INCIDENTS} flag-sharing incidents"
        )));
    }
    let incidents = incident_rows
        .drain(..)
        .map(CheatIncidentRow::into_model)
        .collect::<AppResult<Vec<_>>>()?;
    let pairs = incident_pairs(&incidents);
    validate_report_pair_count(&pairs)?;
    let participation_ids = pairs
        .values()
        .flat_map(|(first, second)| [first.0, second.0])
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();

    // Canonical one-row-per-participation/challenge solves for RSI. Replayed
    // accepted submissions never inflate similarity or common-solve detail.
    let solves = canonical_report_solves(st.pg(), id, &participation_ids).await?;
    let collusion_groups =
        tokio::task::spawn_blocking(move || build_collusion_groups(pairs, solves))
            .await
            .map_err(|error| {
                AppError::internal(format!("anti-cheat report task failed: {error}"))
            })??;

    let (suspicion_list, abnormal_solves) = build_suspicion_sections(st.pg(), id).await?;
    let (ip_analysis, identity_overlaps) =
        super::cheat_identity::build_identity_analysis(st.pg(), id).await?;
    let reconciliation = load_reconciliation_report_state(st.pg(), id).await?;

    Ok(CheatReport {
        // Once sealed, this timestamp is deterministic across cache expiry and
        // replicas; live versions retain their actual build time.
        generated_at: reconciliation.sealed_at.unwrap_or_else(Utc::now),
        evidence_closed_at: reconciliation.evidence_closed_at,
        last_reconciled_at: reconciliation.last_reconciled_at,
        sealed_at: reconciliation.sealed_at,
        pending_jobs: reconciliation.pending_jobs,
        oldest_pending_at: reconciliation.oldest_pending_at,
        last_error: reconciliation.last_error,
        collusion_groups,
        suspicion_list,
        ip_analysis,
        identity_overlaps,
        abnormal_solves,
        detector_capabilities: super::cheat_capabilities::detector_capabilities(),
    })
}

// ---------------------------------------------------------------------------
// Suspicion list — tiered fair-scoring aggregation of persisted events.
// ---------------------------------------------------------------------------

/// The `SuspicionEvents.tier` string the React client keys on
/// (`TIER_META` in `CheatInfo.tsx`): `hard | strong | behavioral | context`.
fn tier_key(tier: crate::services::suspicion::SuspicionTier) -> &'static str {
    use crate::services::suspicion::SuspicionTier::*;
    match tier {
        Hard => "hard",
        Strong => "strong",
        Context => "context",
        Behavioral => "behavioral",
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct ReportSuspicionRow {
    event_id: i32,
    participation_id: i32,
    challenge_id: Option<i32>,
    kind: i16,
    evidence_key: String,
    score_delta: Option<i32>,
    created_at: DateTime<Utc>,
    team_id: i32,
    team_name: String,
    participation_status: i16,
    challenge_name: Option<String>,
    solve_time: Option<DateTime<Utc>>,
}

const MAX_REPORT_SUSPICION_EVENTS: i64 = 10_000;

fn is_abnormal_solve(ty: crate::services::suspicion::SuspicionType) -> bool {
    use crate::services::suspicion::SuspicionType::*;
    matches!(
        ty,
        NoDownload
            | NoContainer
            | Hoarding
            | FastSolveOpen
            | FastSolveDownload
            | FastSolveContainer
            | ZeroWrongAttempts
            | HighWrongRate
            | AutomatedPattern
            | FirstBloodAnomaly
    )
}

/// Build the behavioral suspicion roster and the abnormal-solve projection from
/// the same persisted event snapshot. This function never runs a detector or
/// mutates report state.
async fn build_suspicion_sections(
    pool: &sqlx::PgPool,
    game_id: i32,
) -> AppResult<(Vec<Json>, Vec<Json>)> {
    use crate::services::suspicion::{
        compute_breakdown, default_weight, RiskBand, SuspicionEventRow, SuspicionType,
    };

    // One set-based read supplies event, participation and display-team data.
    let events = sqlx::query_as::<_, ReportSuspicionRow>(
        r#"WITH bounded_events AS MATERIALIZED (
               SELECT event.*
                 FROM "SuspicionEvents" event
                WHERE event.game_id = $1
                  AND event.evidence_key NOT LIKE 'legacy-untrusted:%'
                ORDER BY event.created_at DESC, event.id DESC
                LIMIT $2
             )
           SELECT event.id AS event_id,
                  event.participation_id,
                  event.challenge_id,
                  event.kind,
                  event.evidence_key,
                  event.score_delta,
                  event.created_at,
                  participation.team_id,
                  team.name AS team_name,
                  participation.status AS participation_status,
                  challenge.title AS challenge_name,
                  solve_submission.submit_time_utc AS solve_time
             FROM bounded_events event
             JOIN "Participations" participation
               ON participation.id = event.participation_id
              AND participation.game_id = event.game_id
             JOIN "Teams" team ON team.id = participation.team_id
        LEFT JOIN "FirstSolves" first_solve
               ON first_solve.participation_id = event.participation_id
              AND first_solve.challenge_id = event.challenge_id
        LEFT JOIN "Submissions" solve_submission
               ON solve_submission.id = first_solve.submission_id
              AND solve_submission.game_id = event.game_id
              AND solve_submission.participation_id = event.participation_id
              AND solve_submission.challenge_id = event.challenge_id
              AND solve_submission.status = $3
        LEFT JOIN "Games" game ON game.id = event.game_id
        LEFT JOIN "GameChallenges" challenge
               ON challenge.id = event.challenge_id
              AND challenge.game_id = event.game_id
              AND solve_submission.submit_time_utc >= game.start_time_utc
              AND solve_submission.submit_time_utc < game.end_time_utc
            ORDER BY event.created_at DESC, event.id DESC"#,
    )
    .bind(game_id)
    .bind(MAX_REPORT_SUSPICION_EVENTS.saturating_add(1))
    .bind(AnswerResult::Accepted as i16)
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if events.len() > MAX_REPORT_SUSPICION_EVENTS as usize {
        return Err(AppError::payload_too_large(format!(
            "Anti-cheat report is limited to {MAX_REPORT_SUSPICION_EVENTS} suspicion events"
        )));
    }
    if events.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    // m0091 freezes every event delta. During a rolling migration, query live
    // weights only when a legacy nullable row is actually present.
    let weights: HashMap<String, i32> = if events.iter().any(|event| event.score_delta.is_none()) {
        sqlx::query_as::<_, (String, i32)>(r#"SELECT rule_code, weight FROM "SuspicionRules""#)
            .fetch_all(pool)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?
            .into_iter()
            .collect()
    } else {
        HashMap::new()
    };
    let mut by_part: HashMap<i32, Vec<ReportSuspicionRow>> = HashMap::new();
    for e in events.iter().cloned() {
        by_part.entry(e.participation_id).or_default().push(e);
    }

    // (band, score, counted-incidents, teamId, row) — sort keys travel alongside.
    let mut rows: Vec<(RiskBand, i64, usize, i32, Json)> = Vec::new();
    for (pid, evs) in &by_part {
        let display = &evs[0];

        // Each persisted event is one immutable incident. New events retain the
        // weight resolved at write time; legacy events fall back to the current
        // rule weight because they predate score-delta persistence.
        let event_rows: Vec<SuspicionEventRow> = evs
            .iter()
            .map(|e| {
                let ty = SuspicionType::from_kind(e.kind).ok_or_else(|| {
                    AppError::internal(format!(
                        "unsupported suspicion event kind {} (event {})",
                        e.kind, e.event_id
                    ))
                })?;
                let code = ty.code();
                Ok(SuspicionEventRow {
                    rule_code: code.to_string(),
                    evidence_key: e.evidence_key.clone(),
                    details: ty.default_entry().1.to_string(),
                    time: e.created_at,
                    score_delta: e.score_delta,
                })
            })
            .collect::<AppResult<Vec<_>>>()?;

        let weight_of = |code: &str| {
            weights
                .get(code)
                .copied()
                .unwrap_or_else(|| default_weight(code))
        };
        let bd = compute_breakdown(&event_rows, |code| weight_of(code));

        // Events newest-first (matches RSCTF's `OrderByDescending(e => e.Time)`).
        let mut scored = bd.events.clone();
        scored.sort_by_key(|event| std::cmp::Reverse(event.time));
        let mut used_event_ids = HashSet::new();
        let events_json: Vec<Json> = scored
            .iter()
            .map(|e| {
                let event_id = evs
                    .iter()
                    .filter(|raw| !used_event_ids.contains(&raw.event_id))
                    .filter(|raw| raw.created_at == e.time)
                    .filter(|raw| {
                        SuspicionType::from_kind(raw.kind)
                            .is_some_and(|ty| ty.code() == e.rule_code)
                    })
                    .filter(|raw| {
                        raw.score_delta.unwrap_or_else(|| weight_of(&e.rule_code)) == e.score_delta
                    })
                    .map(|raw| raw.event_id)
                    .max();
                if let Some(event_id) = event_id {
                    used_event_ids.insert(event_id);
                }
                serde_json::json!({
                    "eventId": event_id,
                    "type": e.rule_code,
                    "scoreDelta": e.score_delta,
                    "appliedDelta": e.applied_delta,
                    "details": e.details,
                    "time": e.time.timestamp_millis(),
                    "tier": tier_key(e.tier),
                    "counted": e.counted,
                })
            })
            .collect();

        let counted = bd.events.iter().filter(|e| e.counted).count();
        let record = serde_json::json!({
            "teamId": display.team_id,
            "participationId": pid,
            "teamName": display.team_name,
            "score": bd.total,
            "band": bd.band.band_key(),
            "hard": bd.hard,
            "strong": bd.strong,
            "behavioral": bd.behavioral,
            "corroboration": bd.corroboration,
            "status": cheat_participation_status(display.participation_status)?,
            "events": events_json,
        });
        rows.push((bd.band, bd.total, counted, display.team_id, record));
    }

    // Rank: band desc (hard evidence on top), score desc, counted incidents desc,
    // teamId asc — the deterministic order RSCTF applies.
    rows.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(b.1.cmp(&a.1))
            .then(b.2.cmp(&a.2))
            .then(a.3.cmp(&b.3))
    });
    let suspicion_list = rows.into_iter().map(|r| r.4).collect();

    let mut abnormal: Vec<(DateTime<Utc>, i32, Json)> = Vec::new();
    for event in &events {
        let Some(ty) = SuspicionType::from_kind(event.kind).filter(|ty| is_abnormal_solve(*ty))
        else {
            continue;
        };
        let Some(challenge_id) = event.challenge_id else {
            continue;
        };
        let (Some(challenge_name), Some(solve_time)) =
            (event.challenge_name.as_deref(), event.solve_time)
        else {
            continue;
        };
        abnormal.push((
            solve_time,
            event.event_id,
            serde_json::json!({
                "eventId": event.event_id,
                "teamId": event.team_id,
                "teamName": event.team_name,
                "challengeId": challenge_id,
                "challengeName": challenge_name,
                "type": ty.code(),
                "details": ty.default_entry().1,
                "solveTime": solve_time.timestamp_millis(),
            }),
        ));
    }
    abnormal.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));

    Ok((
        suspicion_list,
        abnormal.into_iter().map(|(_, _, row)| row).collect(),
    ))
}

#[cfg(test)]
#[path = "cheat_tests.rs"]
mod tests;
