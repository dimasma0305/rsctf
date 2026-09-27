//! Monitor review of AI chat disclosures: every team's disclosure status per
//! competitive solve (links, "no AI used", or missing), solve-relative timing,
//! edit counts, and the full append-only edit history for cheat review.
//! History stays readable after the event switch is turned off.

use std::collections::HashSet;

use axum::extract::Query;
use sea_orm::ActiveEnum;

use super::ai_chats::{private_json, StoredLink, JEOPARDY_TYPES};
use super::*;
use crate::services::ai_chat_links;

const MAX_MONITOR_PAGE: i64 = 100;
const MAX_EVENT_ROWS: i64 = 200;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiChatMonitorQuery {
    pub count: Option<i64>,
    pub skip: Option<i64>,
    pub challenge_id: Option<i32>,
    /// `Links`, `NoAi`, or `Missing`; omitted means every status.
    pub status: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorAiChatLink {
    pub url: String,
    pub provider_key: String,
    pub provider_label: String,
    pub provider_active: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiChatLinkRecord {
    pub participation_id: i32,
    pub team_id: i32,
    pub team_name: String,
    pub challenge_id: i32,
    pub challenge_title: String,
    pub category: ChallengeCategory,
    /// `Links`, `NoAi`, or `Missing`.
    pub status: String,
    pub declared_no_ai: bool,
    pub links: Vec<MonitorAiChatLink>,
    pub submitted_by: Option<String>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub solved_at: Option<DateTime<Utc>>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub first_disclosed_at: Option<DateTime<Utc>>,
    /// Seconds from the solve to the first disclosure.
    pub delay_seconds: Option<i64>,
    pub edit_count: i64,
    pub event_count: i64,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub updated_at: Option<DateTime<Utc>>,
    pub revision: i32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiChatLinkPage {
    pub total: i64,
    pub required: bool,
    pub items: Vec<AiChatLinkRecord>,
}

#[derive(sqlx::FromRow)]
struct MonitorRow {
    participation_id: i32,
    team_id: i32,
    team_name: String,
    challenge_id: i32,
    challenge_title: String,
    category: i16,
    status: String,
    declared_no_ai: Option<bool>,
    links: Option<sqlx::types::Json<Vec<StoredLink>>>,
    submitted_by: Option<String>,
    solved_at: Option<DateTime<Utc>>,
    first_disclosed_at: Option<DateTime<Utc>>,
    edit_count: i64,
    event_count: i64,
    updated_at: Option<DateTime<Utc>>,
    revision: Option<i32>,
}

/// Every competitive Jeopardy solve of the game merged with every saved
/// disclosure. A solve without a disclosure is `Missing`.
const MERGED_CTE: &str = r#"
WITH window_bounds AS (
    SELECT start_time_utc, end_time_utc FROM "Games" WHERE id = $1
), solves AS (
    SELECT first_solve.participation_id, first_solve.challenge_id,
           submission.submit_time_utc AS solved_at
      FROM "FirstSolves" first_solve
      JOIN "Submissions" submission
        ON submission.id = first_solve.submission_id AND submission.game_id = $1
      JOIN "GameChallenges" challenge
        ON challenge.id = first_solve.challenge_id AND challenge.game_id = $1
       AND challenge."Type" = ANY($4)
      CROSS JOIN window_bounds
     WHERE submission.submit_time_utc >= window_bounds.start_time_utc
       AND submission.submit_time_utc < window_bounds.end_time_utc
), saved AS (
    SELECT * FROM "AiChatLinks" WHERE game_id = $1
), merged AS (
    SELECT COALESCE(saved.participation_id, solves.participation_id) AS participation_id,
           COALESCE(saved.challenge_id, solves.challenge_id) AS challenge_id,
           solves.solved_at, saved.links, saved.declared_no_ai, saved.revision,
           saved.updated_at, saved.submitted_by,
           CASE WHEN saved.participation_id IS NULL THEN 'Missing'
                WHEN saved.declared_no_ai THEN 'NoAi'
                ELSE 'Links' END AS status
      FROM saved
      FULL JOIN solves
        ON solves.participation_id = saved.participation_id
       AND solves.challenge_id = saved.challenge_id
)
"#;

fn parse_status(status: Option<&str>) -> AppResult<Option<&'static str>> {
    match status {
        None | Some("") | Some("All") => Ok(None),
        Some("Links") => Ok(Some("Links")),
        Some("NoAi") => Ok(Some("NoAi")),
        Some("Missing") => Ok(Some("Missing")),
        Some(_) => Err(AppError::bad_request(
            "status must be Links, NoAi, or Missing",
        )),
    }
}

/// `GET /api/game/{id}/ai-chats` — disclosure status per solve, newest first.
pub async fn list_ai_chat_links(
    State(st): State<SharedState>,
    MonitorUser(_user): MonitorUser,
    Path(id): Path<i32>,
    Query(query): Query<AiChatMonitorQuery>,
) -> AppResult<Response> {
    let game = load_game(&st, id).await?;
    let status = parse_status(query.status.as_deref())?;
    let count = query.count.unwrap_or(50).clamp(1, MAX_MONITOR_PAGE);
    let skip = query.skip.unwrap_or(0).max(0);
    let types = JEOPARDY_TYPES.map(|kind| kind as i16).to_vec();
    let total: i64 = sqlx::query_scalar(&format!(
        "{MERGED_CTE} SELECT COUNT(*) FROM merged
          WHERE ($2::integer IS NULL OR merged.challenge_id = $2)
            AND ($3::text IS NULL OR merged.status = $3)"
    ))
    .bind(id)
    .bind(query.challenge_id)
    .bind(status)
    .bind(&types)
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let rows = sqlx::query_as::<_, MonitorRow>(&format!(
        r#"{MERGED_CTE}
        SELECT merged.participation_id, participation.team_id, team.name AS team_name,
               merged.challenge_id, challenge.title AS challenge_title, challenge.category,
               merged.status, merged.declared_no_ai, merged.links,
               account.user_name AS submitted_by, merged.solved_at,
               history.first_disclosed_at,
               COALESCE(history.edit_count, 0) AS edit_count,
               COALESCE(history.event_count, 0) AS event_count,
               merged.updated_at, merged.revision
          FROM merged
          JOIN "Participations" participation ON participation.id = merged.participation_id
          JOIN "Teams" team ON team.id = participation.team_id
          JOIN "GameChallenges" challenge ON challenge.id = merged.challenge_id
          LEFT JOIN "AspNetUsers" account ON account.id = merged.submitted_by
          LEFT JOIN LATERAL (
              SELECT MIN(event.occurred_at) FILTER (WHERE event.action <> 'Cleared')
                         AS first_disclosed_at,
                     COUNT(*) FILTER (WHERE event.action = 'Edited') AS edit_count,
                     COUNT(*) AS event_count
                FROM "AiChatLinkEvents" event
               WHERE event.game_id = $1
                 AND event.participation_id = merged.participation_id
                 AND event.challenge_id = merged.challenge_id
          ) history ON TRUE
         WHERE ($2::integer IS NULL OR merged.challenge_id = $2)
           AND ($3::text IS NULL OR merged.status = $3)
         ORDER BY COALESCE(merged.updated_at, merged.solved_at) DESC,
                  merged.participation_id, merged.challenge_id
         LIMIT $5 OFFSET $6"#
    ))
    .bind(id)
    .bind(query.challenge_id)
    .bind(status)
    .bind(&types)
    .bind(count)
    .bind(skip)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let active: HashSet<String> =
        ai_chat_links::enabled_providers(&ai_chat_links::load_rows(st.pg()).await?)
            .into_iter()
            .map(|provider| provider.key)
            .collect();
    let items = rows
        .into_iter()
        .map(|row| {
            let category = ChallengeCategory::try_from_value(&row.category)
                .map_err(|error| AppError::internal(error.to_string()))?;
            let delay_seconds = match (row.solved_at, row.first_disclosed_at) {
                (Some(solved), Some(disclosed)) => Some((disclosed - solved).num_seconds()),
                _ => None,
            };
            Ok(AiChatLinkRecord {
                participation_id: row.participation_id,
                team_id: row.team_id,
                team_name: row.team_name,
                challenge_id: row.challenge_id,
                challenge_title: row.challenge_title,
                category,
                status: row.status,
                declared_no_ai: row.declared_no_ai.unwrap_or(false),
                links: row
                    .links
                    .map(|links| links.0)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|link| MonitorAiChatLink {
                        provider_active: active.contains(&link.provider_key),
                        url: link.url,
                        provider_key: link.provider_key,
                        provider_label: link.provider_label,
                    })
                    .collect(),
                submitted_by: row.submitted_by,
                solved_at: row.solved_at,
                first_disclosed_at: row.first_disclosed_at,
                delay_seconds,
                edit_count: row.edit_count,
                event_count: row.event_count,
                updated_at: row.updated_at,
                revision: row.revision.unwrap_or(0),
            })
        })
        .collect::<AppResult<Vec<_>>>()?;
    Ok(private_json(AiChatLinkPage {
        total,
        required: super::ai_chats::required(&game),
        items,
    }))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiChatLinkEvent {
    pub id: i64,
    /// `Created`, `Edited`, or `Cleared`.
    pub action: String,
    pub user_name: Option<String>,
    pub revision: i32,
    pub previous_links: Vec<String>,
    pub links: Vec<String>,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub previous_declared_no_ai: bool,
    pub declared_no_ai: bool,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub solved_at: Option<DateTime<Utc>>,
    pub seconds_since_solve: Option<i64>,
    /// First 12 hex digits of the keyed network hash, for correlation only.
    pub network_hint: Option<String>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub occurred_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiChatLinkEventPage {
    pub items: Vec<AiChatLinkEvent>,
    pub truncated: bool,
}

#[derive(sqlx::FromRow)]
struct EventRow {
    id: i64,
    action: String,
    user_name: Option<String>,
    revision: i32,
    previous_links: sqlx::types::Json<Vec<StoredLink>>,
    links: sqlx::types::Json<Vec<StoredLink>>,
    added_urls: sqlx::types::Json<Vec<String>>,
    removed_urls: sqlx::types::Json<Vec<String>>,
    previous_declared_no_ai: bool,
    declared_no_ai: bool,
    solved_at: Option<DateTime<Utc>>,
    seconds_since_solve: Option<i64>,
    remote_ip_hash: Option<Vec<u8>>,
    occurred_at: DateTime<Utc>,
}

fn network_hint(hash: &[u8]) -> String {
    hash.iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// `GET /api/game/{id}/ai-chats/{participationId}/{challengeId}/events` — the
/// append-only disclosure history of one team and challenge, oldest first.
pub async fn list_ai_chat_link_events(
    State(st): State<SharedState>,
    MonitorUser(_user): MonitorUser,
    Path((id, participation_id, challenge_id)): Path<(i32, i32, i32)>,
) -> AppResult<Response> {
    load_game(&st, id).await?;
    let mut rows = sqlx::query_as::<_, EventRow>(
        r#"SELECT event.id, event.action, account.user_name, event.revision,
                  event.previous_links, event.links, event.added_urls, event.removed_urls,
                  event.previous_declared_no_ai, event.declared_no_ai, event.solved_at,
                  event.seconds_since_solve, event.remote_ip_hash, event.occurred_at
             FROM "AiChatLinkEvents" event
             LEFT JOIN "AspNetUsers" account ON account.id = event.user_id
            WHERE event.game_id = $1 AND event.participation_id = $2
              AND event.challenge_id = $3
            ORDER BY event.occurred_at, event.id
            LIMIT $4"#,
    )
    .bind(id)
    .bind(participation_id)
    .bind(challenge_id)
    .bind(MAX_EVENT_ROWS + 1)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let truncated = rows.len() as i64 > MAX_EVENT_ROWS;
    rows.truncate(MAX_EVENT_ROWS as usize);
    let urls = |links: Vec<StoredLink>| links.into_iter().map(|link| link.url).collect();
    let items = rows
        .into_iter()
        .map(|row| AiChatLinkEvent {
            id: row.id,
            action: row.action,
            user_name: row.user_name,
            revision: row.revision,
            previous_links: urls(row.previous_links.0),
            links: urls(row.links.0),
            added: row.added_urls.0,
            removed: row.removed_urls.0,
            previous_declared_no_ai: row.previous_declared_no_ai,
            declared_no_ai: row.declared_no_ai,
            solved_at: row.solved_at,
            seconds_since_solve: row.seconds_since_solve,
            network_hint: row.remote_ip_hash.as_deref().map(network_hint),
            occurred_at: row.occurred_at,
        })
        .collect();
    Ok(private_json(AiChatLinkEventPage { items, truncated }))
}
