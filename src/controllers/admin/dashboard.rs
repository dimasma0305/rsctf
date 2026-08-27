//! Bounded admin-dashboard reads.
//!
//! These endpoints are polled by the dashboard, so every read keeps a constant
//! statement count and lets PostgreSQL aggregate/filter before returning rows.

use super::*;

use axum::http::header;
use bytes::Bytes;
use serde::Serialize;
use sqlx::PgPool;
use std::future::Future;
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tokio::sync::Semaphore;

const TOP_GAME_LIMIT: i64 = 5;
const MAX_ACTIVITY_PAGE_SIZE: u64 = 1_000;
const MAX_ACTIVITY_OFFSET: u64 = 1_000_000;
const AGGREGATE_CACHE_TTL: Duration = Duration::from_secs(15);
const AGGREGATE_QUERY_DEADLINE: Duration = Duration::from_secs(5);
const DASHBOARD_CACHE_KEY: &str = "admin-dashboard:v2";

static AGGREGATE_QUERY_SLOTS: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(2)));
static AGGREGATE_FLIGHTS: LazyLock<crate::utils::single_flight::SingleFlight<AggregateFill>> =
    LazyLock::new(crate::utils::single_flight::SingleFlight::new);

#[derive(Clone, Default)]
enum AggregateFill {
    Ready(Bytes),
    Busy,
    #[default]
    Failed,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemStatsModel {
    pub user_count: i64,
    pub team_count: i64,
    pub active_container_count: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BasicGameInfoModel {
    pub id: i32,
    pub title: String,
    pub summary: String,
    pub poster: Option<String>,
    pub limit: i32,
    pub team_count: i64,
    pub user_count: i64,
    pub average_rating: Option<f64>,
    pub review_count: i32,
    #[serde(with = "crate::utils::datetime::millis")]
    pub start: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub end: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub server_time: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminDashboardModel {
    pub system_stats: SystemStatsModel,
    pub top_games: Vec<BasicGameInfoModel>,
}

#[derive(sqlx::FromRow)]
struct SystemStatsRow {
    user_count: i64,
    team_count: i64,
    active_container_count: i64,
}

#[derive(sqlx::FromRow)]
struct TopGameRow {
    id: i32,
    title: String,
    summary: String,
    poster_hash: Option<String>,
    member_limit: i32,
    team_count: i64,
    user_count: i64,
    average_rating: Option<f64>,
    review_count: i32,
    start_time_utc: DateTime<Utc>,
    end_time_utc: DateTime<Utc>,
}

const SYSTEM_STATS_SQL: &str = r#"
/* rsctf_admin_dashboard_stats_v2 */
SELECT (SELECT count(*)::bigint FROM "AspNetUsers") AS user_count,
       (SELECT count(*)::bigint FROM "Teams") AS team_count,
       (SELECT count(*)::bigint FROM "Containers") AS active_container_count
"#;

const TOP_GAMES_SQL: &str = r#"
/* rsctf_admin_dashboard_top_games_v2 */
WITH participation_counts AS MATERIALIZED (
    SELECT game_id, count(*)::bigint AS team_count
      FROM "Participations"
     GROUP BY game_id
), ranked_games AS MATERIALIZED (
    SELECT g.id,
           g.title,
           g.summary,
           g.poster_hash,
           g.team_member_count_limit AS member_limit,
           COALESCE(p.team_count, 0)::bigint AS team_count,
           g.start_time_utc,
           g.end_time_utc
      FROM "Games" g
      LEFT JOIN participation_counts p ON p.game_id = g.id
     ORDER BY COALESCE(p.team_count, 0) DESC, g.id DESC
     LIMIT $1
), user_counts AS (
    SELECT u.game_id, count(*)::bigint AS user_count
      FROM "UserParticipations" u
      JOIN ranked_games ranked ON ranked.id = u.game_id
     GROUP BY u.game_id
), review_counts AS (
    SELECT r.game_id,
           count(*)::bigint AS review_count,
           count(*) FILTER (WHERE r.rating IN (1, 2))::bigint AS decisive_count,
           count(*) FILTER (WHERE r.rating = 2)::bigint AS positive_count
      FROM "ChallengeReviews" r
      JOIN ranked_games ranked ON ranked.id = r.game_id
     GROUP BY r.game_id
)
SELECT ranked.id,
       ranked.title,
       ranked.summary,
       ranked.poster_hash,
       ranked.member_limit,
       ranked.team_count,
       COALESCE(u.user_count, 0)::bigint AS user_count,
       CASE WHEN COALESCE(r.decisive_count, 0) > 0
            THEN r.positive_count::float8 / r.decisive_count::float8
            ELSE NULL
       END AS average_rating,
       LEAST(COALESCE(r.review_count, 0), 2147483647)::int4 AS review_count,
       ranked.start_time_utc,
       ranked.end_time_utc
  FROM ranked_games ranked
  LEFT JOIN user_counts u ON u.game_id = ranked.id
  LEFT JOIN review_counts r ON r.game_id = ranked.id
 ORDER BY ranked.team_count DESC, ranked.id DESC
"#;

async fn load_dashboard_model(pool: &PgPool) -> AppResult<AdminDashboardModel> {
    let stats = sqlx::query_as::<_, SystemStatsRow>(SYSTEM_STATS_SQL)
        .fetch_one(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let top_rows = sqlx::query_as::<_, TopGameRow>(TOP_GAMES_SQL)
        .bind(TOP_GAME_LIMIT)
        .fetch_all(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let server_time = Utc::now();
    let top_games = top_rows
        .into_iter()
        .map(|row| BasicGameInfoModel {
            id: row.id,
            title: row.title,
            summary: row.summary,
            poster: row.poster_hash.map(|hash| format!("/assets/{hash}/poster")),
            limit: row.member_limit,
            team_count: row.team_count,
            user_count: row.user_count,
            average_rating: row.average_rating,
            review_count: row.review_count,
            start: row.start_time_utc,
            end: row.end_time_utc,
            server_time,
        })
        .collect();

    Ok(AdminDashboardModel {
        system_stats: SystemStatsModel {
            user_count: stats.user_count,
            team_count: stats.team_count,
            active_container_count: stats.active_container_count,
        },
        top_games,
    })
}

async fn cached_aggregate<T, F, Fut>(
    st: &SharedState,
    cache_key: String,
    load: F,
) -> AppResult<Bytes>
where
    T: Serialize + Send + 'static,
    F: FnOnce(PgPool) -> Fut + Send + 'static,
    Fut: Future<Output = AppResult<T>> + Send + 'static,
{
    if let Some(bytes) = st.cache.get(&cache_key).await {
        return Ok(bytes);
    }

    let state = st.clone();
    let flight_key = cache_key.clone();
    let fill = AGGREGATE_FLIGHTS
        .run(&cache_key, move || async move {
            if let Some(bytes) = state.cache.get(&flight_key).await {
                return AggregateFill::Ready(bytes);
            }
            let Ok(permit) = AGGREGATE_QUERY_SLOTS.clone().try_acquire_owned() else {
                return AggregateFill::Busy;
            };
            let pool = state.pg().clone();
            let loaded = tokio::time::timeout(AGGREGATE_QUERY_DEADLINE, load(pool)).await;
            drop(permit);
            let model = match loaded {
                Ok(Ok(model)) => model,
                Ok(Err(error)) => {
                    tracing::warn!(%error, cache_key = %flight_key, "admin dashboard aggregate failed");
                    return AggregateFill::Failed;
                }
                Err(_) => {
                    tracing::warn!(cache_key = %flight_key, "admin dashboard aggregate timed out");
                    return AggregateFill::Failed;
                }
            };
            let bytes = match serde_json::to_vec(&model) {
                Ok(json) => Bytes::from(json),
                Err(error) => {
                    tracing::warn!(%error, cache_key = %flight_key, "admin dashboard serialization failed");
                    return AggregateFill::Failed;
                }
            };
            state
                .cache
                .set(&flight_key, &bytes, Some(AGGREGATE_CACHE_TTL))
                .await;
            AggregateFill::Ready(bytes)
        })
        .await;

    match fill {
        AggregateFill::Ready(bytes) => Ok(bytes),
        AggregateFill::Busy => Err(AppError::unavailable(
            "Dashboard refresh capacity is busy; retry shortly",
        )),
        AggregateFill::Failed => Err(AppError::internal("dashboard aggregate failed")),
    }
}

fn json_response(bytes: Bytes) -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "private, no-store"),
        ],
        bytes,
    )
        .into_response()
}

/// `GET /api/admin/dashboard` — platform-wide stats + top games by team count.
pub async fn dashboard(State(st): State<SharedState>, _admin: AdminUser) -> AppResult<Response> {
    let bytes = cached_aggregate(&st, DASHBOARD_CACHE_KEY.to_string(), |pool| async move {
        load_dashboard_model(&pool).await
    })
    .await?;
    Ok(json_response(bytes))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeReviewDetailModel {
    pub id: i32,
    pub challenge_id: i32,
    pub challenge_name: String,
    pub game_title: String,
    pub user_id: Uuid,
    pub user_name: String,
    pub rating: ReviewRating,
    pub comment: Option<String>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub submit_time_utc: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct ReviewRow {
    id: i32,
    challenge_id: i32,
    challenge_name: String,
    game_title: String,
    user_id: Uuid,
    user_name: String,
    rating: i16,
    comment: Option<String>,
    submit_time_utc: DateTime<Utc>,
}

const REVIEWS_SQL: &str = r#"
/* rsctf_admin_dashboard_reviews_v2 */
SELECT r.id,
       r.challenge_id,
       COALESCE(c.title, '') AS challenge_name,
       COALESCE(g.title, '') AS game_title,
       r.user_id,
       COALESCE(u.user_name, '') AS user_name,
       r.rating,
       r.comment,
       r.submit_time_utc
  FROM "ChallengeReviews" r
  LEFT JOIN "GameChallenges" c
    ON c.id = r.challenge_id AND c.game_id = r.game_id
  LEFT JOIN "Games" g ON g.id = c.game_id
  LEFT JOIN "AspNetUsers" u ON u.id = r.user_id
 ORDER BY r.submit_time_utc DESC, r.id DESC
 LIMIT $1 OFFSET $2
"#;

fn review_rating(value: i16) -> AppResult<ReviewRating> {
    match value {
        0 => Ok(ReviewRating::None),
        1 => Ok(ReviewRating::Poor),
        2 => Ok(ReviewRating::Fair),
        3 => Ok(ReviewRating::Good),
        4 => Ok(ReviewRating::Excellent),
        _ => Err(AppError::internal("invalid stored review rating")),
    }
}

async fn load_reviews(
    pool: &PgPool,
    count: u64,
    skip: u64,
) -> AppResult<Vec<ChallengeReviewDetailModel>> {
    let rows = sqlx::query_as::<_, ReviewRow>(REVIEWS_SQL)
        .bind(count.min(MAX_ACTIVITY_PAGE_SIZE) as i64)
        .bind(skip.min(MAX_ACTIVITY_OFFSET) as i64)
        .fetch_all(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    rows.into_iter()
        .map(|row| {
            Ok(ChallengeReviewDetailModel {
                id: row.id,
                challenge_id: row.challenge_id,
                challenge_name: row.challenge_name,
                game_title: row.game_title,
                user_id: row.user_id,
                user_name: row.user_name,
                rating: review_rating(row.rating)?,
                comment: row.comment,
                submit_time_utc: row.submit_time_utc,
            })
        })
        .collect()
}

pub async fn reviews(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Query(q): Query<ListQuery>,
) -> AppResult<RequestResponse<Vec<ChallengeReviewDetailModel>>> {
    Ok(RequestResponse::ok(
        load_reviews(st.pg(), q.count, q.skip).await?,
    ))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmissionTrendModel {
    #[serde(with = "crate::utils::datetime::millis")]
    pub time: DateTime<Utc>,
    pub count: i64,
}

#[derive(Debug, Default, Deserialize)]
pub struct SubmissionTrendQuery {
    pub range: Option<String>,
}

#[derive(Clone, Copy)]
enum TrendRange {
    Day,
    Week,
    Month,
    Year,
}

impl TrendRange {
    fn parse(value: Option<&str>) -> Self {
        match value.unwrap_or_default().to_ascii_lowercase().as_str() {
            "week" => Self::Week,
            "month" => Self::Month,
            "year" => Self::Year,
            _ => Self::Day,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::Year => "year",
        }
    }
}

#[derive(sqlx::FromRow)]
struct TrendRow {
    time: DateTime<Utc>,
    count: i64,
}

const TREND_SQL: &str = r#"
/* rsctf_admin_dashboard_trend_v2 */
WITH range_settings AS (
    SELECT CASE $1::text
             WHEN 'day' THEN 'hour'
             WHEN 'year' THEN 'month'
             ELSE 'day'
           END AS granularity,
           CASE $1::text
             WHEN 'day' THEN interval '1 hour'
             WHEN 'year' THEN interval '1 month'
             ELSE interval '1 day'
           END AS step,
           CASE $1::text
             WHEN 'day' THEN 24
             WHEN 'week' THEN 7
             WHEN 'month' THEN 30
             WHEN 'year' THEN 12
             ELSE 24
           END AS bucket_count
), bounds AS (
    SELECT date_trunc(s.granularity, $2::timestamptz, 'UTC') AS last_bucket,
           s.granularity,
           s.step,
           s.bucket_count
      FROM range_settings s
), buckets AS (
    SELECT generate_series(
               b.last_bucket - b.step * (b.bucket_count - 1),
               b.last_bucket,
               b.step
           ) AS time
      FROM bounds b
), counts AS (
    SELECT date_trunc(b.granularity, s.submit_time_utc, 'UTC') AS time,
           count(*)::bigint AS count
      FROM "Submissions" s
      CROSS JOIN bounds b
     WHERE s.submit_time_utc >= b.last_bucket - b.step * (b.bucket_count - 1)
       AND s.submit_time_utc < b.last_bucket + b.step
     GROUP BY 1
)
SELECT b.time, COALESCE(c.count, 0)::bigint AS count
  FROM buckets b
  LEFT JOIN counts c USING (time)
 ORDER BY b.time ASC
"#;

async fn load_submission_trend(
    pool: &PgPool,
    range: TrendRange,
    now: DateTime<Utc>,
) -> AppResult<Vec<SubmissionTrendModel>> {
    let rows = sqlx::query_as::<_, TrendRow>(TREND_SQL)
        .bind(range.key())
        .bind(now)
        .fetch_all(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(rows
        .into_iter()
        .map(|row| SubmissionTrendModel {
            time: row.time,
            count: row.count,
        })
        .collect())
}

pub async fn submission_trend(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Query(q): Query<SubmissionTrendQuery>,
) -> AppResult<Response> {
    let range = TrendRange::parse(q.range.as_deref());
    let cache_key = format!("admin-dashboard:trend:{}:v2", range.key());
    let bytes = cached_aggregate(&st, cache_key, move |pool| async move {
        load_submission_trend(&pool, range, Utc::now()).await
    })
    .await?;
    Ok(json_response(bytes))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteupInfo {
    pub id: i32,
    pub team: TeamInfoModel,
    pub game_title: String,
    pub url: String,
    #[serde(with = "crate::utils::datetime::millis")]
    pub upload_time_utc: DateTime<Utc>,
    pub division_id: Option<i32>,
}

#[derive(sqlx::FromRow)]
struct WriteupRow {
    id: i32,
    team_id: i32,
    team_name: Option<String>,
    team_bio: Option<String>,
    team_avatar_hash: Option<String>,
    team_locked: Option<bool>,
    game_title: Option<String>,
    file_hash: String,
    file_name: String,
    upload_time_utc: DateTime<Utc>,
    division_id: Option<i32>,
}

const WRITEUPS_SQL: &str = r#"
/* rsctf_admin_dashboard_writeups_v2 */
SELECT p.id,
       p.team_id,
       t.name AS team_name,
       t.bio AS team_bio,
       t.avatar_hash AS team_avatar_hash,
       t.locked AS team_locked,
       g.title AS game_title,
       f.hash AS file_hash,
       f.name AS file_name,
       f.upload_time_utc,
       p.division_id
  FROM "Participations" p
  JOIN "Files" f ON f.id = p.writeup_id
  LEFT JOIN "Teams" t ON t.id = p.team_id
  LEFT JOIN "Games" g ON g.id = p.game_id
 WHERE p.writeup_id IS NOT NULL
 ORDER BY p.id DESC
 LIMIT $1 OFFSET $2
"#;

async fn load_writeups(pool: &PgPool, count: u64, skip: u64) -> AppResult<Vec<WriteupInfo>> {
    let rows = sqlx::query_as::<_, WriteupRow>(WRITEUPS_SQL)
        .bind(count.min(MAX_ACTIVITY_PAGE_SIZE) as i64)
        .bind(skip.min(MAX_ACTIVITY_OFFSET) as i64)
        .fetch_all(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(rows
        .into_iter()
        .map(|row| WriteupInfo {
            id: row.id,
            team: TeamInfoModel {
                id: row.team_id,
                name: row.team_name.unwrap_or_default(),
                bio: row.team_bio,
                avatar: row
                    .team_avatar_hash
                    .map(|hash| format!("/assets/{hash}/avatar")),
                locked: row.team_locked.unwrap_or(false),
                members: Vec::new(),
            },
            game_title: row.game_title.unwrap_or_default(),
            url: format!("/assets/{}/{}", row.file_hash, row.file_name),
            upload_time_utc: row.upload_time_utc,
            division_id: row.division_id,
        })
        .collect())
}

pub async fn all_writeups(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Query(q): Query<ListQuery>,
) -> AppResult<RequestResponse<Vec<WriteupInfo>>> {
    Ok(RequestResponse::ok(
        load_writeups(st.pg(), q.count, q.skip).await?,
    ))
}

#[cfg(test)]
#[path = "dashboard_tests.rs"]
mod tests;
