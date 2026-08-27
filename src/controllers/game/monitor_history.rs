//! Bounded, event-scoped projections for the live monitor history panels.
//!
//! These feeds are polled and searched while an event is active. Keep them as
//! one SQL statement per page: resolving platform-wide identity lists before
//! querying the event makes both memory and query count grow with the platform.

use chrono::{DateTime, Utc};
use serde_json::Value as Json;

use super::{
    AnswerResult, AppError, AppResult, EventQuery, EventType, GameEventModel, SubmissionModel,
    SubmissionQuery,
};

pub(super) const MONITOR_PAGE_DEFAULT: u64 = 100;
pub(super) const MONITOR_PAGE_MAX: u64 = 100;
pub(super) const MONITOR_SEARCH_MAX_CHARS: usize = 128;
const MONITOR_SEARCH_INSPECT_MAX_CHARS: usize = 512;
/// Offset pagination is retained for wire compatibility. Beyond this bounded
/// interactive window, operators use the separately admitted export endpoint.
pub(super) const MONITOR_MAX_SKIP: u64 = 10_000;

/// The comments give `pg_stat_statements` and load-test diagnostics stable
/// names without exposing any user input.
pub(super) const MONITOR_EVENTS_SQL: &str = r#"
/* rsctf_monitor_events */
SELECT event."Type"::smallint AS event_type,
       event.id,
       event.feed_cursor,
       event.values,
       event.publish_time_utc,
       account.user_name,
       team.name AS team_name
  FROM "GameEvents" event
  LEFT JOIN "Teams" team ON team.id = event.team_id
  LEFT JOIN "AspNetUsers" account ON account.id = event.user_id
 WHERE event.game_id = $1
   AND (NOT $2::boolean OR event."Type" NOT IN (1, 2))
 ORDER BY event.publish_time_utc DESC, event.id DESC
 OFFSET $3
 LIMIT $4
"#;

pub(super) const MONITOR_EVENTS_SEARCH_SQL: &str = r#"
/* rsctf_monitor_events */
WITH matching AS MATERIALIZED (
    (SELECT candidate.id, candidate.publish_time_utc
       FROM "GameEvents" candidate
       JOIN "Teams" searched_team ON searched_team.id = candidate.team_id
      WHERE candidate.game_id = $1
        AND (NOT $2::boolean OR candidate."Type" NOT IN (1, 2))
        AND LOWER(searched_team.name) LIKE $3 ESCAPE '\'
      ORDER BY candidate.publish_time_utc DESC, candidate.id DESC
      LIMIT $6)
    UNION
    (SELECT candidate.id, candidate.publish_time_utc
       FROM "GameEvents" candidate
       JOIN "AspNetUsers" searched_account ON searched_account.id = candidate.user_id
      WHERE candidate.game_id = $1
        AND (NOT $2::boolean OR candidate."Type" NOT IN (1, 2))
        AND LOWER(searched_account.user_name) LIKE $3 ESCAPE '\'
      ORDER BY candidate.publish_time_utc DESC, candidate.id DESC
      LIMIT $6)
    UNION
    (SELECT candidate.id, candidate.publish_time_utc
       FROM "GameEvents" candidate
      WHERE candidate.game_id = $1
        AND (NOT $2::boolean OR candidate."Type" NOT IN (1, 2))
        AND LOWER(candidate.values::text) LIKE $3 ESCAPE '\'
      ORDER BY candidate.publish_time_utc DESC, candidate.id DESC
      LIMIT $6)
)
SELECT event."Type"::smallint AS event_type,
       event.id,
       event.feed_cursor,
       event.values,
       event.publish_time_utc,
       account.user_name,
       team.name AS team_name
  FROM matching
  JOIN "GameEvents" event ON event.id = matching.id
  LEFT JOIN "Teams" team ON team.id = event.team_id
  LEFT JOIN "AspNetUsers" account ON account.id = event.user_id
 ORDER BY matching.publish_time_utc DESC, matching.id DESC
 OFFSET $4
 LIMIT $5
"#;

pub(super) const MONITOR_SUBMISSIONS_SQL: &str = r#"
/* rsctf_monitor_submissions */
SELECT submission.answer,
       submission.status::smallint AS status,
       submission.submit_time_utc,
       account.user_name,
       team.name AS team_name,
       challenge.title AS challenge_title
  FROM "Submissions" submission
  LEFT JOIN "Teams" team ON team.id = submission.team_id
  LEFT JOIN "AspNetUsers" account ON account.id = submission.user_id
  LEFT JOIN "GameChallenges" challenge
    ON challenge.id = submission.challenge_id
   AND challenge.game_id = submission.game_id
 WHERE submission.game_id = $1
   AND ($2::smallint IS NULL OR submission.status = $2)
 ORDER BY submission.submit_time_utc DESC, submission.id DESC
 OFFSET $3
 LIMIT $4
"#;

pub(super) const MONITOR_SUBMISSIONS_SEARCH_SQL: &str = r#"
/* rsctf_monitor_submissions */
WITH matching AS MATERIALIZED (
    (SELECT candidate.id, candidate.submit_time_utc
       FROM "Submissions" candidate
       JOIN "Teams" searched_team ON searched_team.id = candidate.team_id
      WHERE candidate.game_id = $1
        AND ($2::smallint IS NULL OR candidate.status = $2)
        AND LOWER(searched_team.name) LIKE $3 ESCAPE '\'
      ORDER BY candidate.submit_time_utc DESC, candidate.id DESC
      LIMIT $6)
    UNION
    (SELECT candidate.id, candidate.submit_time_utc
       FROM "Submissions" candidate
       JOIN "AspNetUsers" searched_account ON searched_account.id = candidate.user_id
      WHERE candidate.game_id = $1
        AND ($2::smallint IS NULL OR candidate.status = $2)
        AND LOWER(searched_account.user_name) LIKE $3 ESCAPE '\'
      ORDER BY candidate.submit_time_utc DESC, candidate.id DESC
      LIMIT $6)
    UNION
    (SELECT candidate.id, candidate.submit_time_utc
       FROM "Submissions" candidate
       JOIN "GameChallenges" searched_challenge
         ON searched_challenge.id = candidate.challenge_id
        AND searched_challenge.game_id = candidate.game_id
      WHERE candidate.game_id = $1
        AND ($2::smallint IS NULL OR candidate.status = $2)
        AND LOWER(searched_challenge.title) LIKE $3 ESCAPE '\'
      ORDER BY candidate.submit_time_utc DESC, candidate.id DESC
      LIMIT $6)
    UNION
    (SELECT candidate.id, candidate.submit_time_utc
       FROM "Submissions" candidate
      WHERE candidate.game_id = $1
        AND ($2::smallint IS NULL OR candidate.status = $2)
        AND LOWER(candidate.answer) LIKE $3 ESCAPE '\'
      ORDER BY candidate.submit_time_utc DESC, candidate.id DESC
      LIMIT $6)
)
SELECT submission.answer,
       submission.status::smallint AS status,
       submission.submit_time_utc,
       account.user_name,
       team.name AS team_name,
       challenge.title AS challenge_title
  FROM matching
  JOIN "Submissions" submission ON submission.id = matching.id
  LEFT JOIN "Teams" team ON team.id = submission.team_id
  LEFT JOIN "AspNetUsers" account ON account.id = submission.user_id
  LEFT JOIN "GameChallenges" challenge
    ON challenge.id = submission.challenge_id
   AND challenge.game_id = submission.game_id
 ORDER BY matching.submit_time_utc DESC, matching.id DESC
 OFFSET $4
 LIMIT $5
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MonitorPage {
    pub(super) limit: i64,
    pub(super) offset: i64,
    pub(super) beyond_interactive_history: bool,
}

impl MonitorPage {
    pub(super) fn from_parts(count: Option<u64>, skip: Option<u64>) -> Self {
        let requested = count.unwrap_or(MONITOR_PAGE_DEFAULT);
        let limit = if requested == 0 {
            MONITOR_PAGE_DEFAULT
        } else {
            requested.clamp(1, MONITOR_PAGE_MAX)
        };
        let skip = skip.unwrap_or(0);
        Self {
            limit: limit as i64,
            offset: skip.min(MONITOR_MAX_SKIP) as i64,
            beyond_interactive_history: skip > MONITOR_MAX_SKIP,
        }
    }
}

/// Collapse whitespace, cap by Unicode scalar count, case-fold, then escape
/// SQL LIKE metacharacters. User `%`/`_` therefore remain literal characters
/// instead of turning a tiny query into a whole-history wildcard scan.
pub(super) fn normalized_search_pattern(search: Option<&str>) -> Option<String> {
    let mut normalized = String::with_capacity(MONITOR_SEARCH_MAX_CHARS);
    let mut scalar_count = 0;
    let mut pending_space = false;
    'input: for character in search?.chars().take(MONITOR_SEARCH_INSPECT_MAX_CHARS) {
        if character.is_whitespace() {
            pending_space = scalar_count > 0;
            continue;
        }
        if pending_space {
            if scalar_count == MONITOR_SEARCH_MAX_CHARS {
                break;
            }
            normalized.push(' ');
            scalar_count += 1;
            pending_space = false;
        }
        for lower in character.to_lowercase() {
            if scalar_count == MONITOR_SEARCH_MAX_CHARS {
                break 'input;
            }
            normalized.push(lower);
            scalar_count += 1;
        }
    }
    if normalized.is_empty() {
        return None;
    }

    let escaped = normalized
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    Some(format!("%{escaped}%"))
}

#[derive(sqlx::FromRow)]
struct EventRow {
    id: i32,
    feed_cursor: i64,
    event_type: i16,
    values: Json,
    publish_time_utc: DateTime<Utc>,
    user_name: Option<String>,
    team_name: Option<String>,
}

#[derive(sqlx::FromRow)]
struct SubmissionRow {
    answer: String,
    status: i16,
    submit_time_utc: DateTime<Utc>,
    user_name: Option<String>,
    team_name: Option<String>,
    challenge_title: Option<String>,
}

fn event_type_from_db(value: i16) -> AppResult<EventType> {
    match value {
        0 => Ok(EventType::Normal),
        1 => Ok(EventType::ContainerStart),
        2 => Ok(EventType::ContainerDestroy),
        3 => Ok(EventType::FlagSubmit),
        4 => Ok(EventType::CheatDetected),
        5 => Ok(EventType::Download),
        6 => Ok(EventType::ChallengeOpened),
        _ => Err(AppError::internal("invalid game event type in database")),
    }
}

fn answer_result_from_db(value: i16) -> AppResult<AnswerResult> {
    match value {
        -1 => Ok(AnswerResult::NotFound),
        0 => Ok(AnswerResult::FlagSubmitted),
        1 => Ok(AnswerResult::Accepted),
        2 => Ok(AnswerResult::WrongAnswer),
        3 => Ok(AnswerResult::CheatDetected),
        _ => Err(AppError::internal("invalid submission result in database")),
    }
}

pub(super) async fn load_events(
    pool: &sqlx::PgPool,
    game_id: i32,
    query: &EventQuery,
) -> AppResult<Vec<GameEventModel>> {
    let page = MonitorPage::from_parts(query.count, query.skip);
    if page.beyond_interactive_history {
        return Ok(Vec::new());
    }
    let search = normalized_search_pattern(query.search.as_deref());
    let rows = if let Some(search) = search.as_deref() {
        sqlx::query_as::<_, EventRow>(MONITOR_EVENTS_SEARCH_SQL)
            .bind(game_id)
            .bind(query.hide_container)
            .bind(search)
            .bind(page.offset)
            .bind(page.limit)
            .bind(page.offset + page.limit)
            .fetch_all(pool)
            .await
    } else {
        sqlx::query_as::<_, EventRow>(MONITOR_EVENTS_SQL)
            .bind(game_id)
            .bind(query.hide_container)
            .bind(page.offset)
            .bind(page.limit)
            .fetch_all(pool)
            .await
    }
    .map_err(|error| AppError::internal(error.to_string()))?;

    rows.into_iter()
        .map(|row| {
            Ok(GameEventModel {
                id: row.id,
                cursor: row.feed_cursor,
                event_type: event_type_from_db(row.event_type)?,
                values: row.values,
                time: row.publish_time_utc,
                user: row.user_name,
                team: row.team_name,
            })
        })
        .collect()
}

pub(super) async fn load_submissions(
    pool: &sqlx::PgPool,
    game_id: i32,
    query: &SubmissionQuery,
    status: Option<AnswerResult>,
) -> AppResult<Vec<SubmissionModel>> {
    let page = MonitorPage::from_parts(query.count, query.skip);
    if page.beyond_interactive_history {
        return Ok(Vec::new());
    }
    let search = normalized_search_pattern(query.search.as_deref());
    let status = status.map(|value| value as i16);
    let rows = if let Some(search) = search.as_deref() {
        sqlx::query_as::<_, SubmissionRow>(MONITOR_SUBMISSIONS_SEARCH_SQL)
            .bind(game_id)
            .bind(status)
            .bind(search)
            .bind(page.offset)
            .bind(page.limit)
            .bind(page.offset + page.limit)
            .fetch_all(pool)
            .await
    } else {
        sqlx::query_as::<_, SubmissionRow>(MONITOR_SUBMISSIONS_SQL)
            .bind(game_id)
            .bind(status)
            .bind(page.offset)
            .bind(page.limit)
            .fetch_all(pool)
            .await
    }
    .map_err(|error| AppError::internal(error.to_string()))?;

    rows.into_iter()
        .map(|row| {
            Ok(SubmissionModel {
                answer: row.answer,
                status: answer_result_from_db(row.status)?,
                time: row.submit_time_utc,
                user: row.user_name,
                team: row.team_name,
                challenge: row.challenge_title,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_and_oversized_counts_are_bounded() {
        assert_eq!(MonitorPage::from_parts(None, None).limit, 100);
        assert_eq!(MonitorPage::from_parts(Some(0), None).limit, 100);
        assert_eq!(MonitorPage::from_parts(Some(1), None).limit, 1);
        assert_eq!(MonitorPage::from_parts(Some(10_000), None).limit, 100);
    }

    #[test]
    fn excessive_offset_ends_the_interactive_feed_without_querying() {
        let last = MonitorPage::from_parts(Some(100), Some(MONITOR_MAX_SKIP));
        assert_eq!(last.offset, MONITOR_MAX_SKIP as i64);
        assert!(!last.beyond_interactive_history);

        let beyond = MonitorPage::from_parts(Some(100), Some(MONITOR_MAX_SKIP + 1));
        assert_eq!(beyond.offset, MONITOR_MAX_SKIP as i64);
        assert!(beyond.beyond_interactive_history);
    }

    #[test]
    fn search_is_normalized_capped_and_wildcards_are_literal() {
        assert_eq!(
            normalized_search_pattern(Some("  ReD   Team  ")),
            Some("%red team%".into())
        );
        assert_eq!(normalized_search_pattern(Some("  \n\t")), None);
        assert_eq!(
            normalized_search_pattern(Some("100%_\\")),
            Some(r"%100\%\_\\%".into())
        );

        let long = "é".repeat(MONITOR_SEARCH_MAX_CHARS + 40);
        let pattern = normalized_search_pattern(Some(&long)).unwrap();
        assert_eq!(
            pattern.trim_matches('%').chars().count(),
            MONITOR_SEARCH_MAX_CHARS
        );
    }

    #[test]
    fn feed_queries_are_single_event_scoped_bounded_projections() {
        for (sql, marker) in [
            (MONITOR_EVENTS_SQL, "rsctf_monitor_events"),
            (MONITOR_EVENTS_SEARCH_SQL, "rsctf_monitor_events"),
            (MONITOR_SUBMISSIONS_SQL, "rsctf_monitor_submissions"),
            (MONITOR_SUBMISSIONS_SEARCH_SQL, "rsctf_monitor_submissions"),
        ] {
            assert!(sql.contains(marker));
            assert!(sql.contains("game_id = $1"));
            assert!(sql.contains("LIMIT $4") || sql.contains("LIMIT $5"));
            assert!(!sql.contains(" IN ($"));
            assert!(!sql.contains("ANY($"));
            if sql.contains("WITH matching") {
                assert!(sql.contains("LIMIT $6"));
                assert!(sql.contains("MATERIALIZED"));
            }
        }
    }
}
