//! Durable monitor-submission shaping and best-effort real-time publication.
//!
//! PostgreSQL is authoritative. A submission commits before publication, and
//! reconnecting clients recover missed pushes through the commit-ordered cursor
//! installed by `m0114_submission_feed_cursor`.

use chrono::{DateTime, Utc};
use sea_orm::ActiveEnum;
use serde::Serialize;

use crate::app_state::{HubEvent, SharedState};
use crate::services::event_bus::EventBus;
use crate::utils::enums::AnswerResult;

pub const MAX_BACKFILL_SUBMISSIONS: i64 = 100;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmissionMessage {
    pub id: i32,
    pub cursor: i64,
    pub answer: String,
    pub status: AnswerResult,
    #[serde(with = "crate::utils::datetime::millis")]
    pub time: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub challenge: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmissionBackfill {
    pub submissions: Vec<SubmissionMessage>,
    pub next_cursor: i64,
    pub has_more: bool,
}

#[derive(Debug, sqlx::FromRow)]
struct SubmissionRow {
    id: i32,
    feed_cursor: i64,
    answer: String,
    status: i16,
    submit_time_utc: DateTime<Utc>,
    user_name: Option<String>,
    team_name: Option<String>,
    challenge_title: Option<String>,
}

impl TryFrom<SubmissionRow> for SubmissionMessage {
    type Error = sea_orm::DbErr;

    fn try_from(row: SubmissionRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            cursor: row.feed_cursor,
            answer: row.answer,
            status: AnswerResult::try_from_value(&row.status)?,
            time: row.submit_time_utc,
            user: row.user_name,
            team: row.team_name,
            challenge: row.challenge_title,
        })
    }
}

const COMMITTED_SUBMISSION_SQL: &str = r#"
    SELECT submission.id,
           submission.feed_cursor,
           submission.answer,
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
       AND submission.id = $2
       AND submission.feed_cursor IS NOT NULL
"#;

const BACKFILL_SQL: &str = r#"
    SELECT submission.id,
           submission.feed_cursor,
           submission.answer,
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
       AND submission.feed_cursor > $2
     ORDER BY submission.feed_cursor ASC
     LIMIT $3
"#;

/// Publish the canonical committed row. HTTP backfill remains the correctness
/// path when best-effort publication fails.
pub async fn publish_committed_on(
    pool: &sqlx::PgPool,
    events: &EventBus,
    game_id: i32,
    submission_id: i32,
) -> anyhow::Result<()> {
    // Publication is optional because cursor backfill is authoritative. Never
    // queue a completed player request behind a saturated SQL pool.
    let mut connection = pool
        .try_acquire()
        .ok_or_else(|| anyhow::anyhow!("submission publish skipped while SQL pool is busy"))?;
    let row = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        sqlx::query_as::<_, SubmissionRow>(COMMITTED_SUBMISSION_SQL)
            .bind(game_id)
            .bind(submission_id)
            .fetch_optional(&mut *connection),
    )
    .await
    .map_err(|_| anyhow::anyhow!("submission publish projection timed out"))??
    .ok_or_else(|| anyhow::anyhow!("committed submission is unavailable for publication"))?;
    let message = SubmissionMessage::try_from(row)?;
    events.publish(HubEvent {
        target: "ReceivedSubmissions",
        game_id: Some(game_id),
        payload: serde_json::to_string(&message)?,
    });
    Ok(())
}

pub async fn publish_committed(
    st: &SharedState,
    game_id: i32,
    submission_id: i32,
) -> anyhow::Result<()> {
    publish_committed_on(st.pg(), &st.events, game_id, submission_id).await
}

pub async fn backfill_after(
    pool: &sqlx::PgPool,
    game_id: i32,
    after: i64,
    requested_limit: i64,
) -> anyhow::Result<SubmissionBackfill> {
    let limit = requested_limit.clamp(1, MAX_BACKFILL_SUBMISSIONS);
    let mut rows = sqlx::query_as::<_, SubmissionRow>(BACKFILL_SQL)
        .bind(game_id)
        .bind(after)
        .bind(limit + 1)
        .fetch_all(pool)
        .await?;
    let has_more = rows.len() > limit as usize;
    if has_more {
        rows.pop();
    }
    let submissions = rows
        .into_iter()
        .map(SubmissionMessage::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    let next_cursor = submissions
        .last()
        .map_or(after, |submission| submission.cursor);
    Ok(SubmissionBackfill {
        submissions,
        next_cursor,
        has_more,
    })
}

pub async fn latest_cursor(pool: &sqlx::PgPool, game_id: i32) -> anyhow::Result<i64> {
    Ok(sqlx::query_scalar(
        r#"SELECT COALESCE(MAX(feed_cursor), 0)::bigint
             FROM "Submissions"
            WHERE game_id = $1
              AND feed_cursor IS NOT NULL"#,
    )
    .bind(game_id)
    .fetch_one(pool)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use std::str::FromStr;
    use uuid::Uuid;

    #[test]
    fn monitor_submission_wire_has_stable_identity_cursor_and_millis() {
        let at = DateTime::parse_from_rfc3339("2026-08-28T09:00:00.123Z")
            .unwrap()
            .with_timezone(&Utc);
        let value = serde_json::to_value(SubmissionMessage {
            id: 17,
            cursor: 29,
            answer: "flag{fixture}".into(),
            status: AnswerResult::Accepted,
            time: at,
            user: Some("player".into()),
            team: Some("team".into()),
            challenge: Some("challenge".into()),
        })
        .unwrap();
        assert_eq!(value["id"], 17);
        assert_eq!(value["cursor"], 29);
        assert_eq!(value["time"], at.timestamp_millis());
        assert_eq!(value["status"], "Accepted");
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn committed_submissions_backfill_without_gaps_or_cross_game_rows() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("submission_feed_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(6)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "AspNetUsers" (id UUID PRIMARY KEY, user_name TEXT);
            CREATE TABLE "Teams" (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
            CREATE TABLE "GameChallenges" (
                id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, title TEXT NOT NULL
            );
            CREATE TABLE "Submissions" (
                id SERIAL PRIMARY KEY,
                answer TEXT NOT NULL,
                status SMALLINT NOT NULL,
                submit_time_utc TIMESTAMPTZ NOT NULL,
                user_id UUID,
                team_id INTEGER NOT NULL,
                game_id INTEGER NOT NULL,
                challenge_id INTEGER NOT NULL
            );
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(crate::migrations::SUBMISSION_FEED_CURSOR_SQL)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(crate::migrations::SUBMISSION_FEED_CURSOR_SQL)
            .execute(&pool)
            .await
            .unwrap();

        let user_id = Uuid::new_v4();
        sqlx::query(r#"INSERT INTO "AspNetUsers" (id, user_name) VALUES ($1, 'player')"#)
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "Teams" (id, name) VALUES (9, 'alpha'), (10, 'other')"#)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO "GameChallenges" (id, game_id, title)
               VALUES (70, 7, 'seven'), (80, 8, 'eight')"#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let insert = |game_id: i32, team_id: i32, challenge_id: i32, answer: String| {
            let pool = pool.clone();
            async move {
                let mut transaction = pool.begin().await.unwrap();
                let id: i32 = sqlx::query_scalar(
                    r#"INSERT INTO "Submissions"
                         (answer, status, submit_time_utc, user_id, team_id, game_id, challenge_id)
                       VALUES ($1, 1, clock_timestamp(), $2, $3, $4, $5)
                       RETURNING id"#,
                )
                .bind(answer)
                .bind(user_id)
                .bind(team_id)
                .bind(game_id)
                .bind(challenge_id)
                .fetch_one(&mut *transaction)
                .await
                .unwrap();
                transaction.commit().await.unwrap();
                id
            }
        };

        let first = insert(7, 9, 70, "first".into()).await;
        let second = insert(7, 9, 70, "second".into()).await;
        let third = insert(7, 9, 70, "third".into()).await;
        insert(8, 10, 80, "other-game".into()).await;

        let bus = EventBus::local();
        let mut received = bus.subscribe();
        publish_committed_on(&pool, &bus, 7, second).await.unwrap();
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), received.recv())
            .await
            .unwrap()
            .unwrap();
        let payload: serde_json::Value = serde_json::from_str(&event.payload).unwrap();
        assert_eq!(event.target, "ReceivedSubmissions");
        assert_eq!(event.game_id, Some(7));
        assert_eq!(payload["id"], second);
        assert!(payload["cursor"].as_i64().is_some_and(|cursor| cursor > 0));
        assert_eq!(payload["user"], "player");
        assert_eq!(payload["team"], "alpha");
        assert_eq!(payload["challenge"], "seven");

        let first_page = backfill_after(&pool, 7, 0, 2).await.unwrap();
        assert_eq!(first_page.submissions.len(), 2);
        assert!(first_page.has_more);
        assert!(first_page.submissions[0].cursor < first_page.submissions[1].cursor);
        let second_page = backfill_after(&pool, 7, first_page.next_cursor, 100)
            .await
            .unwrap();
        assert_eq!(second_page.submissions.len(), 1);
        assert!(!second_page.has_more);
        let mut observed = first_page
            .submissions
            .into_iter()
            .chain(second_page.submissions)
            .map(|submission| submission.id)
            .collect::<Vec<_>>();
        observed.sort_unstable();
        assert_eq!(observed, vec![first, second, third]);
        assert!(backfill_after(&pool, 7, second_page.next_cursor, 100)
            .await
            .unwrap()
            .submissions
            .is_empty());

        // Allocate the lower id first, then commit the higher id first. Cursor
        // order must follow commit order rather than sequence-id order.
        let mut lower = pool.begin().await.unwrap();
        let lower_id: i32 = sqlx::query_scalar(
            r#"INSERT INTO "Submissions"
                 (answer, status, submit_time_utc, user_id, team_id, game_id, challenge_id)
               VALUES ('lower-late', 1, clock_timestamp(), $1, 9, 7, 70)
               RETURNING id"#,
        )
        .bind(user_id)
        .fetch_one(&mut *lower)
        .await
        .unwrap();
        let mut higher = pool.begin().await.unwrap();
        let higher_id: i32 = sqlx::query_scalar(
            r#"INSERT INTO "Submissions"
                 (answer, status, submit_time_utc, user_id, team_id, game_id, challenge_id)
               VALUES ('higher-first', 1, clock_timestamp(), $1, 9, 7, 70)
               RETURNING id"#,
        )
        .bind(user_id)
        .fetch_one(&mut *higher)
        .await
        .unwrap();
        assert!(lower_id < higher_id);
        higher.commit().await.unwrap();
        lower.commit().await.unwrap();
        let commit_order: Vec<(i32, i64)> = sqlx::query_as(
            r#"SELECT id, feed_cursor FROM "Submissions"
                WHERE id = ANY($1) ORDER BY feed_cursor"#,
        )
        .bind([lower_id, higher_id])
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(commit_order[0].0, higher_id);
        assert_eq!(commit_order[1].0, lower_id);

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
