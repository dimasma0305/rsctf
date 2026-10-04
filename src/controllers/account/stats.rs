//! Bounded, mutation-driven account statistics.
//!
//! The player only needs compact aggregates. Keep flag answers and the unbounded
//! submission ledger out of both the query result and the wire response.

use std::collections::BTreeMap;

use axum::extract::State;
use chrono::{DateTime, Utc};
use sea_orm::ActiveEnum;
use serde::Serialize;
use sqlx::FromRow;

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::CurrentUser;
use crate::utils::enums::{AnswerResult, ChallengeCategory};
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::RequestResponse;

const MAX_RECENT_GAMES: i64 = 100;

/// RSCTF `GameStatItem` — one recent game in which the user has solves.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameStatItem {
    pub game_id: i32,
    pub game_title: String,
    #[serde(with = "crate::utils::datetime::millis")]
    pub end_time_utc: DateTime<Utc>,
    pub solves: i32,
}

/// RSCTF `UserStatsModel` — the bounded "My Stats" payload.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserStatsModel {
    pub total_solves: i32,
    pub total_first_bloods: i32,
    pub games_participated: i32,
    pub solves_by_category: BTreeMap<String, i32>,
    pub games: Vec<GameStatItem>,
}

#[derive(Debug, FromRow)]
struct StatsProjection {
    total_solves: i64,
    total_first_bloods: i64,
    games_participated: i64,
    category_ids: Vec<i16>,
    category_solves: Vec<i64>,
    game_ids: Vec<i32>,
    game_titles: Vec<String>,
    game_ends: Vec<DateTime<Utc>>,
    game_solves: Vec<i64>,
}

const STATS_SQL: &str = r#"
WITH accepted_submissions AS MATERIALIZED (
    SELECT submission.id, submission.game_id, submission.challenge_id
      FROM "Submissions" submission
     WHERE submission.user_id = $1
       AND submission.status = $2
), accepted AS MATERIALIZED (
    SELECT DISTINCT submission.game_id, submission.challenge_id
      FROM accepted_submissions submission
), solved AS MATERIALIZED (
    SELECT accepted.game_id, accepted.challenge_id, challenge.category,
           game.title AS game_title, game.end_time_utc
      FROM accepted
      JOIN "GameChallenges" challenge ON challenge.id = accepted.challenge_id
      JOIN "Games" game ON game.id = accepted.game_id
), category_summary AS (
    SELECT category, COUNT(*)::bigint AS solves
      FROM solved
     GROUP BY category
), game_summary AS (
    SELECT game_id, game_title, end_time_utc, COUNT(*)::bigint AS solves
      FROM solved
     GROUP BY game_id, game_title, end_time_utc
), recent_games AS MATERIALIZED (
    SELECT game_id, game_title, end_time_utc, solves
      FROM game_summary
     ORDER BY end_time_utc DESC, game_id DESC
     LIMIT $3
)
SELECT
    (SELECT COUNT(*)::bigint FROM solved) AS total_solves,
    (SELECT COUNT(*)::bigint
       FROM accepted_submissions submission
       JOIN LATERAL (
           SELECT 1
             FROM "FirstSolves" first_solve
            WHERE first_solve.submission_id = submission.id
            LIMIT 1
       ) first_solve ON TRUE) AS total_first_bloods,
    (SELECT COUNT(*)::bigint FROM game_summary) AS games_participated,
    COALESCE((SELECT ARRAY_AGG(category ORDER BY category) FROM category_summary), ARRAY[]::smallint[]) AS category_ids,
    COALESCE((SELECT ARRAY_AGG(solves ORDER BY category) FROM category_summary), ARRAY[]::bigint[]) AS category_solves,
    COALESCE((SELECT ARRAY_AGG(game_id ORDER BY end_time_utc DESC, game_id DESC) FROM recent_games), ARRAY[]::integer[]) AS game_ids,
    COALESCE((SELECT ARRAY_AGG(game_title ORDER BY end_time_utc DESC, game_id DESC) FROM recent_games), ARRAY[]::text[]) AS game_titles,
    COALESCE((SELECT ARRAY_AGG(end_time_utc ORDER BY end_time_utc DESC, game_id DESC) FROM recent_games), ARRAY[]::timestamptz[]) AS game_ends,
    COALESCE((SELECT ARRAY_AGG(solves ORDER BY end_time_utc DESC, game_id DESC) FROM recent_games), ARRAY[]::bigint[]) AS game_solves
"#;

fn bounded_i32(value: i64) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

fn category_name(value: i16) -> AppResult<String> {
    let category = ChallengeCategory::try_from_value(&value)
        .map_err(|error| AppError::internal(error.to_string()))?;
    serde_json::to_value(category)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| AppError::internal("Could not encode challenge category"))
}

async fn load_stats(pool: &sqlx::PgPool, user_id: uuid::Uuid) -> AppResult<UserStatsModel> {
    let row = sqlx::query_as::<_, StatsProjection>(STATS_SQL)
        .bind(user_id)
        .bind(AnswerResult::Accepted as i16)
        .bind(MAX_RECENT_GAMES)
        .fetch_one(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    if row.category_ids.len() != row.category_solves.len()
        || row.game_ids.len() != row.game_titles.len()
        || row.game_ids.len() != row.game_ends.len()
        || row.game_ids.len() != row.game_solves.len()
    {
        return Err(AppError::internal("Invalid account statistics projection"));
    }

    let solves_by_category = row
        .category_ids
        .into_iter()
        .zip(row.category_solves)
        .map(|(category, solves)| Ok((category_name(category)?, bounded_i32(solves))))
        .collect::<AppResult<BTreeMap<_, _>>>()?;
    let games = row
        .game_ids
        .into_iter()
        .zip(row.game_titles)
        .zip(row.game_ends)
        .zip(row.game_solves)
        .map(
            |(((game_id, game_title), end_time_utc), solves)| GameStatItem {
                game_id,
                game_title,
                end_time_utc,
                solves: bounded_i32(solves),
            },
        )
        .collect();

    Ok(UserStatsModel {
        total_solves: bounded_i32(row.total_solves),
        total_first_bloods: bounded_i32(row.total_first_bloods),
        games_participated: bounded_i32(row.games_participated),
        solves_by_category,
        games,
    })
}

/// `GET /api/account/stats` — compact lifetime aggregates and at most 100
/// recent games. The client revalidates this read after an accepted solve or an
/// explicit user refresh; it is never an idle poller.
pub async fn stats(
    State(st): State<SharedState>,
    user: CurrentUser,
) -> AppResult<RequestResponse<UserStatsModel>> {
    Ok(RequestResponse::ok(load_stats(st.pg(), user.id).await?))
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::*;

    #[test]
    fn stats_projection_is_bounded_and_never_selects_flag_answers() {
        assert!(STATS_SQL.contains("LIMIT $3"));
        assert!(STATS_SQL.contains("submission.user_id = $1"));
        assert_eq!(STATS_SQL.matches("submission.status = $2").count(), 1);
        assert!(!STATS_SQL.contains("submission.answer"));
        assert!(!STATS_SQL.contains("SELECT submission.*"));
        assert_eq!(MAX_RECENT_GAMES, 100);
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn large_submission_history_returns_compact_bounded_aggregates() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("connect test database");
        let schema = format!("account_stats_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .expect("create isolated schema");
        let options = PgConnectOptions::from_str(&database_url)
            .expect("parse database URL")
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .expect("connect isolated schema");

        sqlx::raw_sql(
            r#"
            CREATE TABLE "Games" (
              id INTEGER PRIMARY KEY, title TEXT NOT NULL,
              end_time_utc TIMESTAMPTZ NOT NULL
            );
            CREATE TABLE "GameChallenges" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, category SMALLINT NOT NULL
            );
            CREATE TABLE "Submissions" (
              id INTEGER PRIMARY KEY, user_id UUID, status SMALLINT NOT NULL,
              game_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL
            );
            CREATE TABLE "FirstSolves" (
              participation_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
              submission_id INTEGER NOT NULL
            );
            "#,
        )
        .execute(&pool)
        .await
        .expect("create stats fixture tables");
        let player = uuid::Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO "Games" (id, title, end_time_utc)
               SELECT n, 'Event ' || n, clock_timestamp() - make_interval(days => n)
                 FROM generate_series(1, 150) n"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "GameChallenges" (id, game_id, category)
               SELECT n, n, 3 FROM generate_series(1, 150) n"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let other = uuid::Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO "Submissions" (id, user_id, status, game_id, challenge_id)
               SELECT n, $1, 1, n, n FROM generate_series(1, 150) n
               UNION ALL
               SELECT n, $1, 2, ((n - 1) % 150) + 1, ((n - 1) % 150) + 1
                 FROM generate_series(151, 10000) n
               UNION ALL
               SELECT n, $2, 1, ((n - 1) % 150) + 1, ((n - 1) % 150) + 1
                 FROM generate_series(10001, 110000) n"#,
        )
        .bind(player)
        .bind(other)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "FirstSolves" (participation_id, challenge_id, submission_id)
               SELECT n, n, n FROM generate_series(1, 10) n
               UNION ALL SELECT 151, 1, 151
               UNION ALL
               SELECT n, ((n - 1) % 150) + 1, n FROM generate_series(10001, 110000) n"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE INDEX ix_submissions_user_accepted_stats
                ON "Submissions" (user_id, game_id, challenge_id)
                WHERE user_id IS NOT NULL AND status = 1;
            CREATE INDEX ix_firstsolves_submission_stats
                ON "FirstSolves" (submission_id);
            ANALYZE "Submissions";
            ANALYZE "FirstSolves";
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let stats = load_stats(&pool, player).await.unwrap();
        assert_eq!(stats.total_solves, 150);
        assert_eq!(stats.total_first_bloods, 10);
        assert_eq!(stats.games_participated, 150);
        assert_eq!(stats.games.len(), MAX_RECENT_GAMES as usize);
        assert_eq!(stats.solves_by_category.values().sum::<i32>(), 150);

        let explain_sql = format!("EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) {STATS_SQL}");
        let plan: serde_json::Value = sqlx::query_scalar(&explain_sql)
            .bind(player)
            .bind(AnswerResult::Accepted as i16)
            .bind(MAX_RECENT_GAMES)
            .fetch_one(&pool)
            .await
            .unwrap();
        let plan = plan.to_string();
        assert!(
            plan.contains("ix_submissions_user_accepted_stats"),
            "{plan}"
        );
        assert!(plan.contains("ix_firstsolves_submission_stats"), "{plan}");

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .expect("drop isolated schema");
        admin.close().await;
    }
}
