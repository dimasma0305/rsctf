//! Durable monitor-feed shaping and best-effort real-time publication.
//!
//! PostgreSQL is authoritative. Writers persist a `GameEvents` row inside the
//! operation's transaction, commit it, and only then call [`publish_committed`].
//! Redis/WebSocket delivery may be lost, so reconnecting clients recover from
//! [`backfill_after`] using the commit-ordered cursor assigned by migration
//! `m0111_game_event_feed_cursor`.

use chrono::{DateTime, Utc};
use sea_orm::ActiveEnum;
use serde::Serialize;
use serde_json::Value as Json;
use uuid::Uuid;

use crate::app_state::{HubEvent, SharedState};
use crate::services::event_bus::EventBus;
use crate::utils::enums::{AnswerResult, EventType};

pub const MAX_BACKFILL_EVENTS: i64 = 100;
const MAX_PUBLISH_BATCH: usize = 8;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameEventMessage {
    /// Stable database identity used for client-side deduplication.
    pub id: i32,
    /// Commit-ordered cursor used only for reconnect backfill.
    pub cursor: i64,
    #[serde(rename = "type")]
    pub event_type: EventType,
    pub values: Json,
    #[serde(with = "crate::utils::datetime::millis")]
    pub time: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameEventBackfill {
    pub events: Vec<GameEventMessage>,
    pub next_cursor: i64,
    pub has_more: bool,
}

#[derive(Debug, sqlx::FromRow)]
struct GameEventRow {
    id: i32,
    game_id: i32,
    feed_cursor: i64,
    event_type: i16,
    values: Json,
    publish_time_utc: DateTime<Utc>,
    user_name: Option<String>,
    team_name: Option<String>,
}

impl TryFrom<GameEventRow> for GameEventMessage {
    type Error = sea_orm::DbErr;

    fn try_from(row: GameEventRow) -> Result<Self, Self::Error> {
        let event_type = EventType::try_from_value(&row.event_type)?;
        Ok(Self {
            id: row.id,
            cursor: row.feed_cursor,
            event_type,
            values: row.values,
            time: row.publish_time_utc,
            user: row.user_name,
            team: row.team_name,
        })
    }
}

pub struct NewGameEvent<'a> {
    pub game_id: i32,
    pub event_type: EventType,
    pub values: &'a Json,
    pub publish_time: DateTime<Utc>,
    pub user_id: Option<Uuid>,
    pub team_id: i32,
}

/// Insert into a caller-owned transaction. The caller must publish only after
/// that transaction commits; the deferred cursor trigger has not fired yet when
/// this function returns.
pub async fn insert_on(
    connection: &mut sqlx::PgConnection,
    event: NewGameEvent<'_>,
) -> Result<i32, sqlx::Error> {
    sqlx::query_scalar(
        r#"INSERT INTO "GameEvents"
             (game_id, "Type", "values", publish_time_utc, user_id, team_id)
           VALUES ($1, $2, $3, $4, $5, $6)
           RETURNING id"#,
    )
    .bind(event.game_id)
    .bind(event.event_type as i16)
    .bind(sqlx::types::Json(event.values))
    .bind(event.publish_time)
    .bind(event.user_id)
    .bind(event.team_id)
    .fetch_one(connection)
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn insert_flag_submission_on(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    result: AnswerResult,
    answer: &str,
    challenge_title: &str,
    submission_id: i32,
    publish_time: DateTime<Utc>,
    user_id: Uuid,
    team_id: i32,
) -> Result<i32, sqlx::Error> {
    let values = serde_json::json!([result, answer, challenge_title, submission_id.to_string(),]);
    insert_on(
        connection,
        NewGameEvent {
            game_id,
            event_type: EventType::FlagSubmit,
            values: &values,
            publish_time,
            user_id: Some(user_id),
            team_id,
        },
    )
    .await
}

pub async fn insert_cheat_detected_on(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    values: &Json,
    publish_time: DateTime<Utc>,
    user_id: Uuid,
    team_id: i32,
) -> Result<i32, sqlx::Error> {
    insert_on(
        connection,
        NewGameEvent {
            game_id,
            event_type: EventType::CheatDetected,
            values,
            publish_time,
            user_id: Some(user_id),
            team_id,
        },
    )
    .await
}

/// Persist one standalone event and publish it only after its transaction (and
/// therefore the deferred feed-cursor assignment) has committed.
pub async fn persist_and_publish(st: &SharedState, event: NewGameEvent<'_>) -> anyhow::Result<i32> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg()).await?;
    let event_id = insert_on(&mut transaction, event).await?;
    transaction.commit().await?;
    if let Err(error) = publish_committed(st, &[event_id]).await {
        tracing::warn!(event_id, %error, "committed game event could not be published");
    }
    Ok(event_id)
}

const COMMITTED_BY_IDS_SQL: &str = r#"
    SELECT event.id,
           event.game_id,
           event.feed_cursor,
           event."Type" AS event_type,
           event."values" AS values,
           event.publish_time_utc,
           account.user_name,
           team.name AS team_name
      FROM "GameEvents" event
      LEFT JOIN "AspNetUsers" account ON account.id = event.user_id
      LEFT JOIN "Teams" team ON team.id = event.team_id
     WHERE event.id = ANY($1)
       AND event.feed_cursor IS NOT NULL
     ORDER BY event.feed_cursor ASC
"#;

const BACKFILL_SQL: &str = r#"
    SELECT event.id,
           event.game_id,
           event.feed_cursor,
           event."Type" AS event_type,
           event."values" AS values,
           event.publish_time_utc,
           account.user_name,
           team.name AS team_name
      FROM "GameEvents" event
      LEFT JOIN "AspNetUsers" account ON account.id = event.user_id
      LEFT JOIN "Teams" team ON team.id = event.team_id
     WHERE event.game_id = $1
       AND event.feed_cursor > $2
     ORDER BY event.feed_cursor ASC
     LIMIT $3
"#;

const EVENT_PAGE_SQL: &str = r#"
    SELECT event.id,
           event.game_id,
           event.feed_cursor,
           event."Type" AS event_type,
           event."values" AS values,
           event.publish_time_utc,
           account.user_name,
           team.name AS team_name
      FROM "GameEvents" event
      LEFT JOIN "AspNetUsers" account ON account.id = event.user_id
      LEFT JOIN "Teams" team ON team.id = event.team_id
     WHERE event.game_id = $1
       AND event.feed_cursor IS NOT NULL
       AND (
            NOT $2
            OR event."Type" NOT IN ($3, $4)
       )
       AND (
            $5::text IS NULL
            OR LOWER(COALESCE(team.name, '')) LIKE $5
            OR LOWER(COALESCE(account.user_name, '')) LIKE $5
            OR LOWER(event."values"::text) LIKE $5
       )
     ORDER BY event.publish_time_utc DESC, event.feed_cursor DESC
     OFFSET $6
     LIMIT $7
"#;

async fn committed_by_ids(
    pool: &sqlx::PgPool,
    event_ids: &[i32],
) -> anyhow::Result<Vec<(i32, GameEventMessage)>> {
    if event_ids.len() > MAX_PUBLISH_BATCH {
        anyhow::bail!("game-event publish batch exceeds {MAX_PUBLISH_BATCH} rows");
    }
    if event_ids.is_empty() {
        return Ok(Vec::new());
    }

    // Real-time publication is optional because cursor backfill is authoritative.
    // Never queue a player response behind a saturated SQL pool after its write
    // already committed.
    let mut connection = pool
        .try_acquire()
        .ok_or_else(|| anyhow::anyhow!("game-event publish skipped while SQL pool is busy"))?;
    let rows = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        sqlx::query_as::<_, GameEventRow>(COMMITTED_BY_IDS_SQL)
            .bind(event_ids)
            .fetch_all(&mut *connection),
    )
    .await
    .map_err(|_| anyhow::anyhow!("game-event publish projection timed out"))??;
    if rows.len()
        != event_ids
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>()
            .len()
    {
        anyhow::bail!("committed game-event projection is incomplete");
    }
    rows.into_iter()
        .map(|row| {
            let game_id = row.game_id;
            Ok((game_id, GameEventMessage::try_from(row)?))
        })
        .collect()
}

/// Publish already-committed rows to local subscribers and Redis peers. A
/// publication failure never changes database correctness; callers log and
/// continue, while reconnecting clients recover from the HTTP cursor.
pub async fn publish_committed_on(
    pool: &sqlx::PgPool,
    events: &EventBus,
    event_ids: &[i32],
) -> anyhow::Result<()> {
    for (game_id, message) in committed_by_ids(pool, event_ids).await? {
        events.publish(HubEvent {
            target: "ReceivedGameEvent",
            game_id: Some(game_id),
            payload: serde_json::to_string(&message)?,
        });
    }
    Ok(())
}

pub async fn publish_committed(st: &SharedState, event_ids: &[i32]) -> anyhow::Result<()> {
    publish_committed_on(st.pg(), &st.events, event_ids).await
}

/// Return a reconnect page in commit order. Every database read is bounded by
/// `MAX_BACKFILL_EVENTS + 1`; the extra row determines `hasMore` without a
/// separate count over the growing event table.
pub async fn backfill_after(
    pool: &sqlx::PgPool,
    game_id: i32,
    after: i64,
    requested_limit: i64,
) -> anyhow::Result<GameEventBackfill> {
    let limit = requested_limit.clamp(1, MAX_BACKFILL_EVENTS);
    let mut rows = sqlx::query_as::<_, GameEventRow>(BACKFILL_SQL)
        .bind(game_id)
        .bind(after)
        .bind(limit + 1)
        .fetch_all(pool)
        .await?;
    let has_more = rows.len() > limit as usize;
    if has_more {
        rows.pop();
    }
    let events = rows
        .into_iter()
        .map(GameEventMessage::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    let next_cursor = events.last().map_or(after, |event| event.cursor);
    Ok(GameEventBackfill {
        events,
        next_cursor,
        has_more,
    })
}

/// Bounded newest-first page used by the existing monitor table. This shares
/// the exact serializer and identity with pushed/backfilled messages.
pub async fn event_page(
    pool: &sqlx::PgPool,
    game_id: i32,
    hide_container: bool,
    search: Option<&str>,
    skip: u64,
    requested_count: u64,
) -> anyhow::Result<Vec<GameEventMessage>> {
    let search = search
        .map(str::trim)
        .filter(|term| !term.is_empty())
        .map(|term| format!("%{}%", term.to_lowercase()));
    let limit = if requested_count == 0 {
        MAX_BACKFILL_EVENTS
    } else {
        requested_count.min(MAX_BACKFILL_EVENTS as u64) as i64
    };
    let offset = i64::try_from(skip).unwrap_or(i64::MAX);
    sqlx::query_as::<_, GameEventRow>(EVENT_PAGE_SQL)
        .bind(game_id)
        .bind(hide_container)
        .bind(EventType::ContainerStart as i16)
        .bind(EventType::ContainerDestroy as i16)
        .bind(search)
        .bind(offset)
        .bind(limit)
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(GameEventMessage::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Capture a no-gap checkpoint after a SignalR connection is established and
/// the authoritative filtered snapshot has completed. Newer commits are then
/// either pushed to that live listener or recoverable with `backfill_after`.
pub async fn latest_cursor(pool: &sqlx::PgPool, game_id: i32) -> anyhow::Result<i64> {
    Ok(sqlx::query_scalar(
        r#"SELECT COALESCE(MAX(feed_cursor), 0)::bigint
             FROM "GameEvents"
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
    use std::time::{Duration, Instant};

    #[test]
    fn every_monitor_event_type_has_a_stable_identity_and_cursor_wire_shape() {
        let at = DateTime::parse_from_rfc3339("2026-08-27T09:00:00.123Z")
            .unwrap()
            .with_timezone(&Utc);
        for (index, event_type) in [
            EventType::ContainerStart,
            EventType::ContainerDestroy,
            EventType::ChallengeOpened,
            EventType::Download,
            EventType::FlagSubmit,
            EventType::CheatDetected,
        ]
        .into_iter()
        .enumerate()
        {
            let message = GameEventMessage {
                id: index as i32 + 10,
                cursor: index as i64 + 20,
                event_type,
                values: serde_json::json!(["fixture"]),
                time: at,
                user: Some("player".to_owned()),
                team: Some("team".to_owned()),
            };
            let value = serde_json::to_value(message).unwrap();
            assert_eq!(value["id"], index as i32 + 10);
            assert_eq!(value["cursor"], index as i64 + 20);
            assert!(value["type"].is_string());
            assert_eq!(value["time"], at.timestamp_millis());
        }
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn committed_events_publish_and_backfill_without_gaps_or_cross_game_rows() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("monitor_feed_{}", Uuid::new_v4().simple());
        assert!(schema
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'));
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
            CREATE TABLE "AspNetUsers" (
                id UUID PRIMARY KEY,
                user_name TEXT
            );
            CREATE TABLE "Teams" (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL
            );
            CREATE TABLE "GameEvents" (
                id SERIAL PRIMARY KEY,
                game_id INTEGER NOT NULL,
                "Type" SMALLINT NOT NULL,
                "values" JSONB NOT NULL,
                publish_time_utc TIMESTAMPTZ NOT NULL,
                user_id UUID,
                team_id INTEGER NOT NULL
            );
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(crate::migrations::GAME_EVENT_FEED_CURSOR_SQL)
            .execute(&pool)
            .await
            .unwrap();
        // Re-running the forward migration must preserve the installed trigger
        // and existing cursor allocation.
        sqlx::raw_sql(crate::migrations::GAME_EVENT_FEED_CURSOR_SQL)
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

        let bus = EventBus::local();
        let mut received = bus.subscribe();
        let mut ids = Vec::new();
        for (index, event_type) in [
            EventType::ContainerStart,
            EventType::ContainerDestroy,
            EventType::ChallengeOpened,
            EventType::Download,
            EventType::FlagSubmit,
            EventType::CheatDetected,
        ]
        .into_iter()
        .enumerate()
        {
            let values = serde_json::json!([format!("event-{index}")]);
            let mut transaction = crate::utils::database::begin_sqlx_transaction(&pool)
                .await
                .unwrap();
            let id = insert_on(
                &mut transaction,
                NewGameEvent {
                    game_id: 7,
                    event_type,
                    values: &values,
                    publish_time: Utc::now(),
                    user_id: Some(user_id),
                    team_id: 9,
                },
            )
            .await
            .unwrap();
            transaction.commit().await.unwrap();
            ids.push(id);
        }
        publish_committed_on(&pool, &bus, &ids).await.unwrap();
        for (expected_id, expected_type) in ids.iter().zip([
            "ContainerStart",
            "ContainerDestroy",
            "ChallengeOpened",
            "Download",
            "FlagSubmit",
            "CheatDetected",
        ]) {
            let event = tokio::time::timeout(Duration::from_secs(1), received.recv())
                .await
                .unwrap()
                .unwrap();
            let payload: serde_json::Value = serde_json::from_str(&event.payload).unwrap();
            assert_eq!(event.game_id, Some(7));
            assert_eq!(payload["id"], *expected_id);
            assert_eq!(payload["type"], expected_type);
            assert_eq!(payload["user"], "player");
            assert_eq!(payload["team"], "alpha");
        }

        let saturated_options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let saturated_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(saturated_options)
            .await
            .unwrap();
        let held_connection = saturated_pool.acquire().await.unwrap();
        let publish_started = Instant::now();
        assert!(publish_committed_on(&saturated_pool, &bus, &[ids[0]])
            .await
            .is_err());
        assert!(publish_started.elapsed() < Duration::from_millis(500));
        drop(held_connection);
        saturated_pool.close().await;

        // A rolled-back row never gains a committed cursor and cannot publish.
        let rolled_back_values = serde_json::json!(["rolled-back"]);
        let mut rolled_back = crate::utils::database::begin_sqlx_transaction(&pool)
            .await
            .unwrap();
        let rolled_back_id = insert_on(
            &mut rolled_back,
            NewGameEvent {
                game_id: 7,
                event_type: EventType::Normal,
                values: &rolled_back_values,
                publish_time: Utc::now(),
                user_id: Some(user_id),
                team_id: 9,
            },
        )
        .await
        .unwrap();
        rolled_back.rollback().await.unwrap();
        assert!(publish_committed_on(&pool, &bus, &[rolled_back_id])
            .await
            .is_err());
        assert!(
            tokio::time::timeout(Duration::from_millis(30), received.recv())
                .await
                .is_err()
        );

        let first_page = backfill_after(&pool, 7, 0, 2).await.unwrap();
        assert_eq!(first_page.events.len(), 2);
        assert!(first_page.has_more);
        assert!(first_page.events[0].cursor < first_page.events[1].cursor);
        let second_page = backfill_after(&pool, 7, first_page.next_cursor, 100)
            .await
            .unwrap();
        assert_eq!(second_page.events.len(), 4);
        assert!(!second_page.has_more);
        let mut all_ids = first_page
            .events
            .into_iter()
            .chain(second_page.events)
            .map(|event| event.id)
            .collect::<Vec<_>>();
        all_ids.sort_unstable();
        let mut expected_ids = ids.clone();
        expected_ids.sort_unstable();
        assert_eq!(all_ids, expected_ids);

        let other_values = serde_json::json!(["wrong-game"]);
        let mut other = crate::utils::database::begin_sqlx_transaction(&pool)
            .await
            .unwrap();
        insert_on(
            &mut other,
            NewGameEvent {
                game_id: 8,
                event_type: EventType::Normal,
                values: &other_values,
                publish_time: Utc::now(),
                user_id: Some(user_id),
                team_id: 10,
            },
        )
        .await
        .unwrap();
        other.commit().await.unwrap();
        assert!(backfill_after(&pool, 7, second_page.next_cursor, 100)
            .await
            .unwrap()
            .events
            .is_empty());

        // Sequence ids are allocated at INSERT, so commit the higher-id row
        // first and prove its deferred cursor is the lower one. This is the
        // natural race a plain `id > cursor` reconnect would skip.
        let lower_id_values = serde_json::json!(["lower-id-late-commit"]);
        let mut lower_id = crate::utils::database::begin_sqlx_transaction(&pool)
            .await
            .unwrap();
        let lower_id_event = insert_on(
            &mut lower_id,
            NewGameEvent {
                game_id: 7,
                event_type: EventType::Normal,
                values: &lower_id_values,
                publish_time: Utc::now(),
                user_id: Some(user_id),
                team_id: 9,
            },
        )
        .await
        .unwrap();
        let higher_id_values = serde_json::json!(["higher-id-first-commit"]);
        let mut higher_id = crate::utils::database::begin_sqlx_transaction(&pool)
            .await
            .unwrap();
        let higher_id_event = insert_on(
            &mut higher_id,
            NewGameEvent {
                game_id: 7,
                event_type: EventType::Normal,
                values: &higher_id_values,
                publish_time: Utc::now(),
                user_id: Some(user_id),
                team_id: 9,
            },
        )
        .await
        .unwrap();
        assert!(lower_id_event < higher_id_event);
        higher_id.commit().await.unwrap();
        lower_id.commit().await.unwrap();
        let natural_commit_order: Vec<(i32, i64)> = sqlx::query_as(
            r#"SELECT id, feed_cursor FROM "GameEvents" WHERE id = ANY($1) ORDER BY feed_cursor"#,
        )
        .bind(&[lower_id_event, higher_id_event])
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(natural_commit_order[0].0, higher_id_event);
        assert_eq!(natural_commit_order[1].0, lower_id_event);

        // Force the deferred trigger in A while retaining its advisory lock.
        // B's commit must wait and receive the later cursor after A commits.
        let mut first = crate::utils::database::begin_sqlx_transaction(&pool)
            .await
            .unwrap();
        let first_id = insert_on(
            &mut first,
            NewGameEvent {
                game_id: 7,
                event_type: EventType::Normal,
                values: &serde_json::json!(["first-commit"]),
                publish_time: Utc::now(),
                user_id: Some(user_id),
                team_id: 9,
            },
        )
        .await
        .unwrap();
        sqlx::query("SET CONSTRAINTS tr_gameevents_feed_cursor IMMEDIATE")
            .execute(&mut *first)
            .await
            .unwrap();
        let mut second = crate::utils::database::begin_sqlx_transaction(&pool)
            .await
            .unwrap();
        let second_id = insert_on(
            &mut second,
            NewGameEvent {
                game_id: 7,
                event_type: EventType::Normal,
                values: &serde_json::json!(["second-commit"]),
                publish_time: Utc::now(),
                user_id: Some(user_id),
                team_id: 9,
            },
        )
        .await
        .unwrap();
        let mut second_commit = tokio::spawn(async move { second.commit().await });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut second_commit)
                .await
                .is_err()
        );
        first.commit().await.unwrap();
        second_commit.await.unwrap().unwrap();
        let cursor_pair: Vec<(i32, i64)> = sqlx::query_as(
            r#"SELECT id, feed_cursor FROM "GameEvents" WHERE id = ANY($1) ORDER BY feed_cursor"#,
        )
        .bind(&[first_id, second_id])
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(cursor_pair[0].0, first_id);
        assert_eq!(cursor_pair[1].0, second_id);
        assert!(cursor_pair[0].1 < cursor_pair[1].1);

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
