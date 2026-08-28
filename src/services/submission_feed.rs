//! Durable monitor-submission shaping and best-effort real-time publication.
//!
//! PostgreSQL is authoritative. A submission commits before publication, and
//! reconnecting clients recover missed pushes through the reconnect-safe cursor
//! installed by `m0114_submission_feed_cursor`. Cursor assignment never waits
//! behind another same-game submitter: a deferred trigger makes a non-blocking
//! attempt, then this service reconciles any missed rows in bounded batches.

use chrono::{DateTime, Utc};
use sea_orm::ActiveEnum;
use serde::Serialize;
use sqlx::{Acquire, PgConnection};

use crate::app_state::{HubEvent, SharedState};
use crate::services::event_bus::EventBus;
use crate::utils::enums::AnswerResult;

pub const MAX_BACKFILL_SUBMISSIONS: i64 = 100;
const CURSOR_LOCK_NAMESPACE: i32 = 1_398_097_485;
const MAX_ASSIGNMENTS_PER_GAME: i64 = 100;
const MAX_GAMES_PER_PASS: i64 = 16;
const MAX_PUBLISH_BATCH: usize = MAX_ASSIGNMENTS_PER_GAME as usize;
const RECONCILE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
const HOT_PATH_BUDGET: std::time::Duration = std::time::Duration::from_millis(500);
const _: () = assert!(MAX_ASSIGNMENTS_PER_GAME <= 100);
const _: () = assert!(MAX_GAMES_PER_PASS <= 16);

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

const COMMITTED_SUBMISSIONS_SQL: &str = r#"
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
       AND submission.id = ANY($2)
       AND submission.feed_cursor IS NOT NULL
     ORDER BY submission.feed_cursor ASC
"#;

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

const ASSIGN_PENDING_SQL: &str = r#"
    WITH pending AS MATERIALIZED (
      SELECT queue.submission_id
        FROM "SubmissionFeedPending" queue
       WHERE queue.game_id = $1
       ORDER BY queue.submission_id
       LIMIT $2
       FOR UPDATE SKIP LOCKED
    ), assigned AS (
      UPDATE "Submissions" submission
         SET feed_cursor = nextval('rsctf_submission_feed_cursor_seq')
        FROM pending
       WHERE submission.id = pending.submission_id
         AND submission.feed_cursor IS NULL
       RETURNING submission.id, submission.feed_cursor
    ), removed AS (
      DELETE FROM "SubmissionFeedPending" queue
       USING pending
       WHERE queue.submission_id = pending.submission_id
       RETURNING queue.submission_id
    )
    SELECT id FROM assigned ORDER BY feed_cursor ASC
"#;

const FIRST_PENDING_GAME_SQL: &str = r#"
    SELECT game_id
      FROM "SubmissionFeedPending"
     ORDER BY game_id, submission_id
     LIMIT 1
"#;

const NEXT_PENDING_GAME_SQL: &str = r#"
    SELECT game_id
      FROM "SubmissionFeedPending"
     WHERE game_id > $1
     ORDER BY game_id, submission_id
     LIMIT 1
"#;

async fn assign_pending_on(
    connection: &mut PgConnection,
    game_id: i32,
    limit: i64,
) -> anyhow::Result<Vec<i32>> {
    let mut transaction = connection.begin().await?;
    let acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1, $2)")
        .bind(CURSOR_LOCK_NAMESPACE)
        .bind(game_id)
        .fetch_one(&mut *transaction)
        .await?;
    if !acquired {
        transaction.rollback().await?;
        return Ok(Vec::new());
    }
    let assigned = sqlx::query_scalar(ASSIGN_PENDING_SQL)
        .bind(game_id)
        .bind(limit.clamp(1, MAX_ASSIGNMENTS_PER_GAME))
        .fetch_all(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(assigned)
}

async fn pending_game_ids(
    pool: &sqlx::PgPool,
    last_game_id: &mut Option<i32>,
) -> anyhow::Result<Vec<i32>> {
    let mut game_ids = Vec::with_capacity(MAX_GAMES_PER_PASS as usize);
    let mut seen = std::collections::HashSet::with_capacity(MAX_GAMES_PER_PASS as usize);
    let mut wrapped = false;
    while game_ids.len() < MAX_GAMES_PER_PASS as usize {
        let next = match *last_game_id {
            Some(game_id) => {
                sqlx::query_scalar(NEXT_PENDING_GAME_SQL)
                    .bind(game_id)
                    .fetch_optional(pool)
                    .await?
            }
            None => {
                sqlx::query_scalar(FIRST_PENDING_GAME_SQL)
                    .fetch_optional(pool)
                    .await?
            }
        };
        let Some(game_id) = next else {
            if last_game_id.is_some() && !wrapped {
                *last_game_id = None;
                wrapped = true;
                continue;
            }
            break;
        };
        if !seen.insert(game_id) {
            break;
        }
        *last_game_id = Some(game_id);
        game_ids.push(game_id);
    }
    Ok(game_ids)
}

async fn committed_by_ids_on(
    connection: &mut PgConnection,
    game_id: i32,
    submission_ids: &[i32],
) -> anyhow::Result<Vec<SubmissionMessage>> {
    if submission_ids.len() > MAX_PUBLISH_BATCH {
        anyhow::bail!("submission publish batch exceeds {MAX_PUBLISH_BATCH} rows");
    }
    if submission_ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query_as::<_, SubmissionRow>(COMMITTED_SUBMISSIONS_SQL)
        .bind(game_id)
        .bind(submission_ids)
        .fetch_all(connection)
        .await?;
    rows.into_iter()
        .map(SubmissionMessage::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn publish_messages(events: &EventBus, game_id: i32, messages: Vec<SubmissionMessage>) {
    for message in messages {
        match serde_json::to_string(&message) {
            Ok(payload) => events.publish(HubEvent {
                target: "ReceivedSubmissions",
                game_id: Some(game_id),
                payload,
            }),
            Err(error) => tracing::warn!(
                game = game_id,
                submission = message.id,
                %error,
                "submission feed row could not be serialized"
            ),
        }
    }
}

async fn committed_or_pending_on(
    connection: &mut PgConnection,
    game_id: i32,
    submission_id: i32,
) -> anyhow::Result<Option<SubmissionMessage>> {
    if let Some(row) = sqlx::query_as::<_, SubmissionRow>(COMMITTED_SUBMISSION_SQL)
        .bind(game_id)
        .bind(submission_id)
        .fetch_optional(&mut *connection)
        .await?
    {
        return Ok(Some(SubmissionMessage::try_from(row)?));
    }
    let pending: bool = sqlx::query_scalar(
        r#"SELECT EXISTS (
             SELECT 1 FROM "SubmissionFeedPending" WHERE submission_id = $1
           )"#,
    )
    .bind(submission_id)
    .fetch_one(&mut *connection)
    .await?;
    if pending {
        tracing::debug!(
            game = game_id,
            submission = submission_id,
            "submission feed cursor assignment remains pending"
        );
        return Ok(None);
    }
    // Close the race where a reconciler assigned and removed the queue row
    // between the first projection and the pending check.
    if let Some(row) = sqlx::query_as::<_, SubmissionRow>(COMMITTED_SUBMISSION_SQL)
        .bind(game_id)
        .bind(submission_id)
        .fetch_optional(&mut *connection)
        .await?
    {
        return Ok(Some(SubmissionMessage::try_from(row)?));
    }
    anyhow::bail!("committed submission is unavailable for publication")
}

/// Publish the canonical committed row. HTTP backfill remains the correctness
/// path when best-effort publication fails.
pub async fn publish_committed_on(
    pool: &sqlx::PgPool,
    events: &EventBus,
    game_id: i32,
    submission_id: i32,
) -> anyhow::Result<()> {
    // Publication is optional because cursor backfill is authoritative. Never
    // queue a completed player request behind a saturated SQL pool or a
    // same-game cursor assignment already in progress.
    let mut connection = pool
        .try_acquire()
        .ok_or_else(|| anyhow::anyhow!("submission publish skipped while SQL pool is busy"))?;
    let message = tokio::time::timeout(
        HOT_PATH_BUDGET,
        committed_or_pending_on(&mut connection, game_id, submission_id),
    )
    .await
    .map_err(|_| anyhow::anyhow!("submission publish projection timed out"))??;
    let Some(message) = message else {
        return Ok(());
    };
    publish_messages(events, game_id, vec![message]);
    Ok(())
}

pub async fn publish_committed(
    st: &SharedState,
    game_id: i32,
    submission_id: i32,
) -> anyhow::Result<()> {
    publish_committed_on(st.pg(), &st.events, game_id, submission_id).await
}

async fn reconcile_pending_once(
    st: &SharedState,
    last_game_id: &mut Option<i32>,
) -> anyhow::Result<usize> {
    let game_ids = pending_game_ids(st.pg(), last_game_id).await?;
    let mut assigned_count = 0;
    for game_id in game_ids {
        let mut connection = st.pg().acquire().await?;
        let submission_ids =
            assign_pending_on(&mut connection, game_id, MAX_ASSIGNMENTS_PER_GAME).await?;
        assigned_count += submission_ids.len();
        let messages = committed_by_ids_on(&mut connection, game_id, &submission_ids).await?;
        publish_messages(&st.events, game_id, messages);
    }
    Ok(assigned_count)
}

/// Reconcile rows whose commit-time non-blocking cursor attempt lost a race.
/// Every pass and every game batch is bounded; multiple eligible replicas may run
/// this safely because the same non-blocking per-game transaction fence owns
/// assignment order.
pub fn start_reconciler(
    state: SharedState,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(RECONCILE_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last_game_id = None;
        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
                _ = ticker.tick() => {
                    match reconcile_pending_once(&state, &mut last_game_id).await {
                        Ok(count) if count > 0 => tracing::debug!(
                            count,
                            "reconciled pending submission feed cursor(s)"
                        ),
                        Ok(_) => {}
                        Err(error) => tracing::warn!(
                            %error,
                            "submission feed cursor reconciliation failed"
                        ),
                    }
                }
            }
        }
    })
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

    #[test]
    fn pending_reconciliation_is_nonblocking_and_bounded() {
        assert!(ASSIGN_PENDING_SQL.contains("FOR UPDATE SKIP LOCKED"));
        assert!(ASSIGN_PENDING_SQL.contains("LIMIT $2"));
        assert!(ASSIGN_PENDING_SQL.contains("DELETE FROM \"SubmissionFeedPending\""));
        assert!(!NEXT_PENDING_GAME_SQL.contains("IS NULL OR"));
        assert!(NEXT_PENDING_GAME_SQL.contains("game_id > $1"));
        assert!(FIRST_PENDING_GAME_SQL.contains("ORDER BY game_id, submission_id"));
        assert!(NEXT_PENDING_GAME_SQL.contains("LIMIT 1"));
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
        let other_game = insert(8, 10, 80, "other-game".into()).await;

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

        // A same-game cursor owner must never hold a submission commit hostage.
        // The deferred trigger skips the busy fence, leaving a durable NULL for
        // the bounded reconciler to assign after the owner releases it.
        let mut blocker = pool.begin().await.unwrap();
        sqlx::query("SELECT pg_advisory_xact_lock($1, $2)")
            .bind(CURSOR_LOCK_NAMESPACE)
            .bind(7_i32)
            .execute(&mut *blocker)
            .await
            .unwrap();
        let pending_id = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            insert(7, 9, 70, "lost-race".into()),
        )
        .await
        .expect("submission commit waited behind the cursor fence");
        let second_pending_id = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            insert(7, 9, 70, "second-lost-race".into()),
        )
        .await
        .expect("second submission commit waited behind the cursor fence");
        let pending_cursor: Option<i64> =
            sqlx::query_scalar(r#"SELECT feed_cursor FROM "Submissions" WHERE id = $1"#)
                .bind(pending_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(pending_cursor, None);
        assert!(publish_committed_on(&pool, &bus, 7, pending_id)
            .await
            .is_ok());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), received.recv())
                .await
                .is_err()
        );
        blocker.rollback().await.unwrap();

        // A later visible submission may safely advance the monitor checkpoint.
        // Reconciliation must assign the older pending row above that checkpoint
        // so `after = checkpoint` still recovers it.
        let visible_after_pending = insert(7, 9, 70, "visible-after-pending".into()).await;
        let visible_cursor: i64 =
            sqlx::query_scalar(r#"SELECT feed_cursor FROM "Submissions" WHERE id = $1"#)
                .bind(visible_after_pending)
                .fetch_one(&pool)
                .await
                .unwrap();

        let first_worker = {
            let pool = pool.clone();
            async move {
                let mut connection = pool.acquire().await.unwrap();
                assign_pending_on(&mut connection, 7, MAX_ASSIGNMENTS_PER_GAME)
                    .await
                    .unwrap()
            }
        };
        let second_worker = {
            let pool = pool.clone();
            async move {
                let mut connection = pool.acquire().await.unwrap();
                assign_pending_on(&mut connection, 7, MAX_ASSIGNMENTS_PER_GAME)
                    .await
                    .unwrap()
            }
        };
        let (first_assigned, second_assigned) = tokio::join!(first_worker, second_worker);
        let assigned = first_assigned
            .into_iter()
            .chain(second_assigned)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(assigned.len(), 2);
        assert!(assigned.contains(&pending_id));
        assert!(assigned.contains(&second_pending_id));
        let pending_after_assignment: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*)::bigint FROM "SubmissionFeedPending" WHERE game_id = 7"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(pending_after_assignment, 0);
        let reconciled_cursor: i64 =
            sqlx::query_scalar(r#"SELECT feed_cursor FROM "Submissions" WHERE id = $1"#)
                .bind(pending_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(reconciled_cursor > visible_cursor);
        let recovered = backfill_after(&pool, 7, visible_cursor, 100).await.unwrap();
        assert!(recovered
            .submissions
            .iter()
            .any(|submission| submission.id == pending_id));

        // Rotation is by game, not by one global oldest-row window. Even with
        // more game-7 work, a pass resuming after game 7 visits game 8 first.
        sqlx::query(
            r#"INSERT INTO "SubmissionFeedPending" (submission_id, game_id)
               VALUES ($1, 7), ($2, 8)"#,
        )
        .bind(first)
        .bind(other_game)
        .execute(&pool)
        .await
        .unwrap();
        let mut last_game_id = Some(7);
        let fair_games = pending_game_ids(&pool, &mut last_game_id).await.unwrap();
        assert_eq!(fair_games.first(), Some(&8));
        assert!(fair_games.contains(&7));
        sqlx::query(r#"DELETE FROM "SubmissionFeedPending" WHERE submission_id = ANY($1)"#)
            .bind([first, other_game])
            .execute(&pool)
            .await
            .unwrap();

        // Allocate the lower id first, then commit the higher id first. Cursor
        // order must remain reconnect-safe rather than follow sequence-id order.
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
