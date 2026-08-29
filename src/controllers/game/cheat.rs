//! Cheat detection: immutable flag-sharing evidence + collusion (RSI) reporting.
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
    pub(crate) incident_id: i32,
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
}

/// Load immutable flag-sharing incidents. `game_id = None` is the admin-global
/// feed; monitor reads pass a game id. A null SQL limit intentionally means
/// "all rows" for the established per-game response contract.
pub(crate) async fn load_cheat_incident_rows(
    pool: &sqlx::PgPool,
    game_id: Option<i32>,
    limit: Option<i64>,
    offset: i64,
    after: Option<i32>,
) -> AppResult<Vec<CheatIncidentRow>> {
    sqlx::query_as::<_, CheatIncidentRow>(
        r#"SELECT cheat.id AS incident_id,
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
              AND ($4::INTEGER IS NULL OR cheat.id > $4)
            ORDER BY
              CASE WHEN $4::INTEGER IS NULL THEN cheat.id END DESC,
              CASE WHEN $4::INTEGER IS NOT NULL THEN cheat.id END ASC
            LIMIT $2 OFFSET $3"#,
    )
    .bind(game_id)
    .bind(limit)
    .bind(offset.max(0))
    .bind(after)
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

const MAX_CHEAT_INCIDENT_PAGE: i64 = 100;
const MAX_COMPARE_SOLVES: usize = 2_048;
const MAX_REPORT_INCIDENTS: usize = 10_000;
const MAX_REPORT_SOLVES: usize = 50_000;
const MAX_REPORT_SUSPICION_EVENTS: usize = 50_000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheatIncidentPage {
    #[serde(default)]
    skip: i64,
    #[serde(default = "default_cheat_incident_count")]
    count: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheatIncidentFeedQuery {
    #[serde(default)]
    after: Option<i32>,
    #[serde(default = "default_cheat_incident_count")]
    count: i64,
}

const fn default_cheat_incident_count() -> i64 {
    MAX_CHEAT_INCIDENT_PAGE
}

impl CheatIncidentPage {
    fn normalized(self) -> AppResult<(i64, i64)> {
        if !(1..=MAX_CHEAT_INCIDENT_PAGE).contains(&self.count)
            || !(0..=10_000).contains(&self.skip)
        {
            return Err(AppError::bad_request(
                "Cheat incident pages require count 1-100 and skip at most 10000",
            ));
        }
        Ok((self.count, self.skip))
    }
}

/// `GET /api/game/{id}/cheatinfo` — requires Monitor.
///
/// Flag ownership is captured transactionally at submit time in `CheatInfo`; this
/// read never consults mutable instance/flag state.
pub async fn cheat_info(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path(id): Path<i32>,
    Query(page): Query<CheatIncidentPage>,
) -> AppResult<RequestResponse<Vec<CheatInfoModel>>> {
    let _ = load_game(&st, id).await?;
    let (count, skip) = page.normalized()?;
    let results = load_cheat_incident_rows(st.pg(), Some(id), Some(count), skip, None)
        .await?
        .into_iter()
        .map(CheatIncidentRow::into_model)
        .collect::<AppResult<Vec<_>>>()?;
    Ok(RequestResponse::ok(results))
}

/// `GET /api/game/{id}/cheatinfo/page` — latest bounded snapshot or an
/// ascending immutable delta after a stable incident id.
pub async fn cheat_info_page(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path(id): Path<i32>,
    Query(query): Query<CheatIncidentFeedQuery>,
) -> AppResult<RequestResponse<CheatIncidentPageModel>> {
    let _ = load_game(&st, id).await?;
    if query.after.is_some_and(|after| after < 0)
        || !(1..=MAX_CHEAT_INCIDENT_PAGE).contains(&query.count)
    {
        return Err(AppError::bad_request(
            "Cheat incident pages require a non-negative cursor and count 1-100",
        ));
    }
    let mut rows = load_cheat_incident_rows(
        st.pg(),
        Some(id),
        Some(query.count.saturating_add(1)),
        0,
        query.after,
    )
    .await?;
    let has_more = rows.len() > query.count as usize;
    rows.truncate(query.count as usize);
    let next_cursor = rows
        .iter()
        .map(|row| row.incident_id)
        .max()
        .or(query.after)
        .unwrap_or(0);
    let incidents = rows
        .into_iter()
        .map(|row| {
            let cursor = row.incident_id;
            Ok(CheatIncidentFeedItem {
                cursor,
                incident: row.into_model()?,
            })
        })
        .collect::<AppResult<Vec<_>>>()?;
    Ok(RequestResponse::ok(CheatIncidentPageModel {
        incidents,
        next_cursor,
        has_more,
    }))
}

/// Reconstruct flag-sharing incidents for a game — shared by `cheatinfo` (raw
/// list) and `cheatreport` (grouped into collusion groups). Detection strategy is
/// documented on [`cheat_info`].
async fn collect_cheat_incidents(st: &SharedState, id: i32) -> AppResult<Vec<CheatInfoModel>> {
    let rows = load_cheat_incident_rows(
        st.pg(),
        Some(id),
        Some(MAX_REPORT_INCIDENTS as i64 + 1),
        0,
        None,
    )
    .await?;
    if rows.len() > MAX_REPORT_INCIDENTS {
        return Err(AppError::payload_too_large(
            "Cheat report has too many flag-sharing incidents; use the paginated incident log",
        ));
    }
    rows.into_iter().map(CheatIncidentRow::into_model).collect()
}

/// Query for the collusion `compare` endpoint (`?participationA=&participationB=`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompareQuery {
    pub participation_a: i32,
    pub participation_b: i32,
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
pub(super) async fn build_cheat_report(st: &SharedState, id: i32) -> AppResult<CheatReport> {
    let incidents = collect_cheat_incidents(st, id).await?;

    // Canonical one-row-per-participation/challenge solves for RSI. Replayed
    // accepted submissions never inflate similarity or common-solve detail.
    let subs = canonical_solves(st.pg(), id, &[], Some(MAX_REPORT_SOLVES as i64 + 1)).await?;
    if subs.len() > MAX_REPORT_SOLVES {
        return Err(AppError::payload_too_large(
            "Cheat report has too many canonical solves; narrow the evidence review",
        ));
    }
    let titles: HashMap<i32, String> = subs
        .iter()
        .map(|solve| (solve.challenge_id, solve.challenge_title.clone()))
        .collect();
    let mut by_part: HashMap<i32, Vec<CanonicalSolveRow>> = HashMap::new();
    for s in subs {
        by_part.entry(s.participation_id).or_default().push(s);
    }
    // Group incidents by the unordered participation pair; the lower participation
    // id is the first team so the grouping and team order are deterministic.
    type TeamRef = (i32, i32, String); // (participation_id, team_id, team_name)
    let mut pairs: BTreeMap<(i32, i32), (TeamRef, TeamRef)> = BTreeMap::new();
    for inc in &incidents {
        let name_of =
            |p: &ParticipationModel| p.team.name.clone().unwrap_or_else(|| "Unknown".to_string());
        let a: TeamRef = (
            inc.owned_team.id,
            inc.owned_team.team.id,
            name_of(&inc.owned_team),
        );
        let b: TeamRef = (
            inc.submit_team.id,
            inc.submit_team.team.id,
            name_of(&inc.submit_team),
        );
        let (first, second) = if a.0 <= b.0 { (a, b) } else { (b, a) };
        pairs.entry((first.0, second.0)).or_insert((first, second));
    }

    let empty: Vec<CanonicalSolveRow> = Vec::new();
    let mut collusion_groups: Vec<Json> = Vec::new();
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
        collusion_groups.push(serde_json::json!({
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
    // Highest-similarity pairs first.
    collusion_groups.sort_by(|a, b| {
        let ra = a["averageRsi"].as_f64().unwrap_or(0.0);
        let rb = b["averageRsi"].as_f64().unwrap_or(0.0);
        rb.partial_cmp(&ra).unwrap_or(std::cmp::Ordering::Equal)
    });

    let (suspicion_list, abnormal_solves) = build_suspicion_sections(st.pg(), id).await?;
    let (ip_analysis, identity_overlaps) =
        super::cheat_identity::build_identity_analysis(st.pg(), id).await?;
    let reconciliation = load_reconciliation_report_state(st.pg(), id).await?;

    Ok(CheatReport {
        generated_at: Utc::now(),
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
}

#[derive(Debug, sqlx::FromRow)]
struct AcceptedSolveRow {
    participation_id: i32,
    challenge_id: i32,
    challenge_name: String,
    solve_time: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct CanonicalSolveRow {
    participation_id: i32,
    challenge_id: i32,
    challenge_title: String,
    submit_time_utc: DateTime<Utc>,
}

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
        r#"SELECT event.id AS event_id,
                  event.participation_id,
                  event.challenge_id,
                  event.kind,
                  event.evidence_key,
                  event.score_delta,
                  event.created_at,
                  participation.team_id,
                  team.name AS team_name,
                  participation.status AS participation_status
             FROM "SuspicionEvents" event
             JOIN "Participations" participation
               ON participation.id = event.participation_id
              AND participation.game_id = event.game_id
             JOIN "Teams" team ON team.id = participation.team_id
            WHERE event.game_id = $1
              AND event.evidence_key NOT LIKE 'legacy-untrusted:%'
            ORDER BY event.created_at DESC, event.id DESC
            LIMIT $2"#,
    )
    .bind(game_id)
    .bind(MAX_REPORT_SUSPICION_EVENTS as i64 + 1)
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if events.len() > MAX_REPORT_SUSPICION_EVENTS {
        return Err(AppError::payload_too_large(
            "Cheat report has too many suspicion events; use the paginated evidence views",
        ));
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

    let solved = sqlx::query_as::<_, AcceptedSolveRow>(
        r#"SELECT DISTINCT ON (submission.participation_id, submission.challenge_id)
                  submission.participation_id,
                  submission.challenge_id,
                  challenge.title AS challenge_name,
                  submission.submit_time_utc AS solve_time
             FROM "FirstSolves" first_solve
             JOIN "Submissions" submission
               ON submission.id = first_solve.submission_id
              AND submission.participation_id = first_solve.participation_id
              AND submission.challenge_id = first_solve.challenge_id
             JOIN "GameChallenges" challenge
               ON challenge.id = submission.challenge_id
              AND challenge.game_id = submission.game_id
             JOIN "Participations" participation
               ON participation.id = first_solve.participation_id
              AND participation.game_id = submission.game_id
             JOIN "Games" game
               ON game.id = submission.game_id
            WHERE participation.game_id = $1
              AND submission.status = $2
              AND submission.submit_time_utc >= game.start_time_utc
              AND submission.submit_time_utc < game.end_time_utc
            ORDER BY submission.participation_id,
                     submission.challenge_id,
                     submission.submit_time_utc,
                     submission.id"#,
    )
    .bind(game_id)
    .bind(AnswerResult::Accepted as i16)
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let solved: HashMap<(i32, i32), AcceptedSolveRow> = solved
        .into_iter()
        .map(|row| ((row.participation_id, row.challenge_id), row))
        .collect();

    let mut abnormal: Vec<(DateTime<Utc>, i32, Json)> = Vec::new();
    for event in &events {
        let Some(ty) = SuspicionType::from_kind(event.kind).filter(|ty| is_abnormal_solve(*ty))
        else {
            continue;
        };
        let Some(challenge_id) = event.challenge_id else {
            continue;
        };
        let Some(solve) = solved.get(&(event.participation_id, challenge_id)) else {
            continue;
        };
        abnormal.push((
            solve.solve_time,
            event.event_id,
            serde_json::json!({
                "eventId": event.event_id,
                "teamId": event.team_id,
                "teamName": event.team_name,
                "challengeId": challenge_id,
                "challengeName": solve.challenge_name,
                "type": ty.code(),
                "details": ty.default_entry().1,
                "solveTime": solve.solve_time.timestamp_millis(),
            }),
        ));
    }
    abnormal.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));

    Ok((
        suspicion_list,
        abnormal.into_iter().map(|(_, _, row)| row).collect(),
    ))
}

/// `GET /api/game/{id}/cheatreport/compare` — requires Monitor.
///
/// Mirrors RSCTF `CheatReportController.Compare`: for two participations in the
/// game, compute the RSI (`0.7·Jaccard(solved sets) + 0.3·LCS(solve order)`) and
/// the per-common-challenge solve-time detail rows.
pub async fn cheat_report_compare(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path(id): Path<i32>,
    Query(q): Query<CompareQuery>,
) -> AppResult<RequestResponse<CollusionCompareResult>> {
    let _ = load_game(&st, id).await?;

    if q.participation_a == q.participation_b {
        return Err(AppError::bad_request(
            "Cannot compare a participation with itself.",
        ));
    }

    // Validate both ids in one game-scoped query.
    let requested = [q.participation_a, q.participation_b];
    let found = sqlx::query_scalar::<_, i32>(
        r#"SELECT id
             FROM "Participations"
            WHERE game_id = $1 AND id = ANY($2::INTEGER[])"#,
    )
    .bind(id)
    .bind(&requested[..])
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if found.len() != requested.len() {
        return Err(AppError::bad_request(
            "One or both participations were not found in this game.",
        ));
    }

    let solves = canonical_solves(
        st.pg(),
        id,
        &[q.participation_a, q.participation_b],
        Some(MAX_COMPARE_SOLVES as i64 + 1),
    )
    .await?;
    if solves.len() > MAX_COMPARE_SOLVES {
        return Err(AppError::bad_request(
            "The selected pair has too many solves for an interactive comparison",
        ));
    }
    let titles: HashMap<i32, String> = solves
        .iter()
        .map(|solve| (solve.challenge_id, solve.challenge_title.clone()))
        .collect();
    let (sub_a, sub_b): (Vec<_>, Vec<_>) = solves
        .into_iter()
        .partition(|solve| solve.participation_id == q.participation_a);

    let (rsi, _common, details) =
        tokio::task::spawn_blocking(move || collusion_metrics(&sub_a, &sub_b, &titles))
            .await
            .map_err(|error| AppError::internal(format!("comparison task failed: {error}")))?;
    Ok(RequestResponse::ok(CollusionCompareResult { rsi, details }))
}

/// Canonical first solves in deterministic solve order. An empty participation
/// filter selects the whole game.
async fn canonical_solves(
    pool: &sqlx::PgPool,
    game_id: i32,
    participation_ids: &[i32],
    limit: Option<i64>,
) -> AppResult<Vec<CanonicalSolveRow>> {
    sqlx::query_as::<_, CanonicalSolveRow>(
        r#"SELECT first_solve.participation_id,
                  first_solve.challenge_id,
                  challenge.title AS challenge_title,
                  submission.submit_time_utc
             FROM "FirstSolves" first_solve
             JOIN "Submissions" submission
               ON submission.id = first_solve.submission_id
              AND submission.participation_id = first_solve.participation_id
              AND submission.challenge_id = first_solve.challenge_id
             JOIN "Participations" participation
               ON participation.id = first_solve.participation_id
              AND participation.game_id = submission.game_id
             JOIN "Games" game
               ON game.id = submission.game_id
             JOIN "GameChallenges" challenge
               ON challenge.id = first_solve.challenge_id
              AND challenge.game_id = submission.game_id
            WHERE participation.game_id = $1
              AND submission.status = $2
              AND submission.submit_time_utc >= game.start_time_utc
              AND submission.submit_time_utc < game.end_time_utc
              AND (CARDINALITY($3::INTEGER[]) = 0
                   OR first_solve.participation_id = ANY($3))
            ORDER BY submission.submit_time_utc, submission.id
            LIMIT $4"#,
    )
    .bind(game_id)
    .bind(AnswerResult::Accepted as i16)
    .bind(participation_ids)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

/// Length of the longest common subsequence of two challenge-id sequences
/// (mirrors RSCTF `GetLongestCommonSubsequence`, rolling one-row DP).
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

/// RSI + common-solve overlap between two participations' accepted submissions.
///
/// Returns `(rsi, commonSolveTitles, detailedSolves)` where `rsi =
/// 0.7·Jaccard(solved sets) + 0.3·(LCS(solve order)/min(len))`, mirroring RSCTF.
/// `detailedSolves` are `SequenceSuspectDetail`-shaped JSON rows, ordered by
/// solve-time gap (closest first), capped at 50, then by team-A solve time.
fn collusion_metrics(
    sub_a: &[CanonicalSolveRow],
    sub_b: &[CanonicalSolveRow],
    titles: &HashMap<i32, String>,
) -> (f64, Vec<String>, Vec<Json>) {
    let seq_a: Vec<i32> = sub_a.iter().map(|s| s.challenge_id).collect();
    let seq_b: Vec<i32> = sub_b.iter().map(|s| s.challenge_id).collect();
    let set_a: HashSet<i32> = seq_a.iter().copied().collect();
    let set_b: HashSet<i32> = seq_b.iter().copied().collect();

    let mut inter: Vec<i32> = set_a
        .iter()
        .copied()
        .filter(|c| set_b.contains(c))
        .collect();
    inter.sort_unstable();
    let union = set_a.union(&set_b).count();
    let jaccard = if union == 0 {
        0.0
    } else {
        inter.len() as f64 / union as f64
    };
    let lcs = lcs_len(&seq_a, &seq_b);
    let min_len = seq_a.len().min(seq_b.len());
    let lcs_score = if min_len == 0 {
        0.0
    } else {
        lcs as f64 / min_len as f64
    };
    let rsi = jaccard * 0.7 + lcs_score * 0.3;

    // Earliest accepted solve time per side (submissions are ascending, so the
    // first match is the earliest solve of that challenge).
    let mut rows: Vec<(String, DateTime<Utc>, DateTime<Utc>, f64)> = Vec::new();
    let mut common_solves: Vec<String> = Vec::new();
    for cid in &inter {
        let name = titles
            .get(cid)
            .cloned()
            .unwrap_or_else(|| "Unknown".to_string());
        common_solves.push(name.clone());
        let ta = sub_a
            .iter()
            .find(|s| s.challenge_id == *cid)
            .map(|s| s.submit_time_utc);
        let tb = sub_b
            .iter()
            .find(|s| s.challenge_id == *cid)
            .map(|s| s.submit_time_utc);
        if let (Some(ta), Some(tb)) = (ta, tb) {
            let diff = ((ta - tb).num_milliseconds().abs() as f64) / 1000.0;
            rows.push((name, ta, tb, diff));
        }
    }
    rows.sort_by(|a, b| a.3.partial_cmp(&b.3).unwrap_or(std::cmp::Ordering::Equal));
    rows.truncate(50);
    rows.sort_by_key(|row| row.1);

    let detailed: Vec<Json> = rows
        .into_iter()
        .map(|(name, ta, tb, diff)| {
            serde_json::json!({
                "challengeName": name,
                "timeA": ta.timestamp_millis(),
                "timeB": tb.timestamp_millis(),
                "timeDiff": diff,
            })
        })
        .collect();
    (rsi, common_solves, detailed)
}

#[cfg(test)]
#[path = "cheat_tests.rs"]
mod tests;
