//! Durable admin Flag Egress feed and best-effort real-time publication.
//!
//! A row identity remains stable across windowed upserts. `feed_cursor` changes
//! after every committed insert or update, so HTTP snapshots, reconnect pages,
//! and SignalR pushes can share one DTO without timestamp-derived identities.

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::app_state::{HubEvent, SharedState};
use crate::services::event_bus::EventBus;

pub const MAX_FLAG_EGRESS_PAGE: i64 = 100;
pub const MAX_FLAG_EGRESS_BACKFILL: i64 = 100;
pub const MAX_FLAG_EGRESS_SKIP: u64 = 10_000;
const MAX_FLAG_EGRESS_SEARCH_CHARS: usize = 128;
const MAX_FLAG_EGRESS_SEARCH_INSPECT_CHARS: usize = 512;

#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct FlagEgressEventModel {
    /// Stable aggregate-row identity used for de-duplication.
    pub id: i32,
    /// Commit/update-ordered cursor used for reconnect recovery.
    pub cursor: i64,
    pub game_id: i32,
    pub participation_id: i32,
    pub challenge_id: i32,
    pub container_id: Option<Uuid>,
    pub team_name: String,
    pub challenge_title: String,
    pub remote_ip: String,
    pub remote_port: i32,
    pub hit_count: i32,
    #[serde(with = "crate::utils::datetime::millis")]
    pub first_seen_utc: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub last_seen_utc: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlagEgressBackfill {
    pub events: Vec<FlagEgressEventModel>,
    pub next_cursor: i64,
    pub has_more: bool,
}

pub struct FlagEgressPage {
    pub events: Vec<FlagEgressEventModel>,
    pub total: i64,
}

const PROJECTION: &str = r#"
    SELECT event.id,
           event.feed_cursor AS cursor,
           event.game_id,
           event.participation_id,
           event.challenge_id,
           event.container_id,
           COALESCE(team.name, '') AS team_name,
           COALESCE(challenge.title, '') AS challenge_title,
           event.remote_ip,
           event.remote_port,
           event.hit_count,
           event.first_seen_utc,
           event.last_seen_utc
      FROM "FlagEgressEvents" event
      LEFT JOIN "Participations" participation ON participation.id = event.participation_id
      LEFT JOIN "Teams" team ON team.id = participation.team_id
      LEFT JOIN "GameChallenges" challenge ON challenge.id = event.challenge_id
"#;

const PAGE_SUFFIX: &str = r#"
     WHERE event.game_id = $1
       AND event.feed_cursor IS NOT NULL
       AND (
            $2::text IS NULL
            OR LOWER(COALESCE(team.name, '')) LIKE $2 ESCAPE '\'
            OR LOWER(COALESCE(challenge.title, '')) LIKE $2 ESCAPE '\'
            OR LOWER(event.remote_ip) LIKE $2 ESCAPE '\'
       )
     ORDER BY event.last_seen_utc DESC, event.feed_cursor DESC
     OFFSET $3
     LIMIT $4
"#;

const COUNT_SQL: &str = r#"
    SELECT COUNT(*)::bigint
      FROM "FlagEgressEvents" event
      LEFT JOIN "Participations" participation ON participation.id = event.participation_id
      LEFT JOIN "Teams" team ON team.id = participation.team_id
      LEFT JOIN "GameChallenges" challenge ON challenge.id = event.challenge_id
     WHERE event.game_id = $1
       AND event.feed_cursor IS NOT NULL
       AND (
            $2::text IS NULL
            OR LOWER(COALESCE(team.name, '')) LIKE $2 ESCAPE '\'
            OR LOWER(COALESCE(challenge.title, '')) LIKE $2 ESCAPE '\'
            OR LOWER(event.remote_ip) LIKE $2 ESCAPE '\'
       )
"#;

const BACKFILL_SUFFIX: &str = r#"
     WHERE event.game_id = $1
       AND event.feed_cursor > $2
     ORDER BY event.feed_cursor ASC
     LIMIT $3
"#;

const COMMITTED_SUFFIX: &str = r#"
     WHERE event.id = $1
       AND event.feed_cursor IS NOT NULL
"#;

fn projection_with(suffix: &str) -> String {
    let mut sql = String::with_capacity(PROJECTION.len() + suffix.len());
    sql.push_str(PROJECTION);
    sql.push_str(suffix);
    sql
}

fn search_pattern(search: Option<&str>) -> Option<String> {
    let mut normalized = String::with_capacity(MAX_FLAG_EGRESS_SEARCH_CHARS);
    let mut scalar_count = 0;
    let mut pending_space = false;
    'input: for character in search?.chars().take(MAX_FLAG_EGRESS_SEARCH_INSPECT_CHARS) {
        if character.is_whitespace() {
            pending_space = scalar_count > 0;
            continue;
        }
        if pending_space {
            if scalar_count == MAX_FLAG_EGRESS_SEARCH_CHARS {
                break;
            }
            normalized.push(' ');
            scalar_count += 1;
            pending_space = false;
        }
        for lower in character.to_lowercase() {
            if scalar_count == MAX_FLAG_EGRESS_SEARCH_CHARS {
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

fn page_offset(skip: u64) -> i64 {
    skip.min(MAX_FLAG_EGRESS_SKIP) as i64
}

/// Return one bounded newest-first page and its filtered total.
pub async fn page(
    pool: &sqlx::PgPool,
    game_id: i32,
    search: Option<&str>,
    skip: u64,
    requested_count: u64,
) -> anyhow::Result<FlagEgressPage> {
    let search = search_pattern(search);
    let offset = page_offset(skip);
    let limit = i64::try_from(requested_count)
        .unwrap_or(i64::MAX)
        .clamp(1, MAX_FLAG_EGRESS_PAGE);
    let total = sqlx::query_scalar::<_, i64>(COUNT_SQL)
        .bind(game_id)
        .bind(search.as_deref())
        .fetch_one(pool)
        .await?;
    let page_sql = projection_with(PAGE_SUFFIX);
    let events = sqlx::query_as::<_, FlagEgressEventModel>(&page_sql)
        .bind(game_id)
        .bind(search.as_deref())
        .bind(offset)
        .bind(limit)
        .fetch_all(pool)
        .await?;
    Ok(FlagEgressPage { events, total })
}

/// Return a bounded ascending cursor page for reconnect recovery.
pub async fn backfill_after(
    pool: &sqlx::PgPool,
    game_id: i32,
    after: i64,
    requested_limit: i64,
) -> anyhow::Result<FlagEgressBackfill> {
    let limit = requested_limit.clamp(1, MAX_FLAG_EGRESS_BACKFILL);
    let backfill_sql = projection_with(BACKFILL_SUFFIX);
    let mut events = sqlx::query_as::<_, FlagEgressEventModel>(&backfill_sql)
        .bind(game_id)
        .bind(after)
        .bind(limit + 1)
        .fetch_all(pool)
        .await?;
    let has_more = events.len() > limit as usize;
    if has_more {
        events.pop();
    }
    let next_cursor = events.last().map_or(after, |event| event.cursor);
    Ok(FlagEgressBackfill {
        events,
        next_cursor,
        has_more,
    })
}

pub async fn latest_cursor(pool: &sqlx::PgPool, game_id: i32) -> anyhow::Result<i64> {
    Ok(sqlx::query_scalar(
        r#"SELECT COALESCE(MAX(feed_cursor), 0)::bigint
             FROM "FlagEgressEvents"
            WHERE game_id = $1
              AND feed_cursor IS NOT NULL"#,
    )
    .bind(game_id)
    .fetch_one(pool)
    .await?)
}

/// Publish one already-committed row. PostgreSQL remains authoritative when
/// the bounded best-effort projection cannot acquire a connection promptly.
pub async fn publish_committed_on(
    pool: &sqlx::PgPool,
    events: &EventBus,
    event_id: i32,
) -> anyhow::Result<()> {
    let mut connection = pool
        .try_acquire()
        .ok_or_else(|| anyhow::anyhow!("flag-egress publish skipped while SQL pool is busy"))?;
    let committed_sql = projection_with(COMMITTED_SUFFIX);
    let message = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        sqlx::query_as::<_, FlagEgressEventModel>(&committed_sql)
            .bind(event_id)
            .fetch_optional(&mut *connection),
    )
    .await
    .map_err(|_| anyhow::anyhow!("flag-egress publish projection timed out"))??
    .ok_or_else(|| anyhow::anyhow!("committed flag-egress row is unavailable"))?;
    let game_id = message.game_id;
    events.publish(HubEvent {
        target: "ReceivedFlagEgress",
        game_id: Some(game_id),
        payload: serde_json::to_string(&message)?,
    });
    Ok(())
}

pub async fn publish_committed(st: &SharedState, event_id: i32) -> anyhow::Result<()> {
    publish_committed_on(st.pg(), &st.events, event_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use std::str::FromStr;
    use std::time::Duration;

    #[test]
    fn shared_wire_model_is_camel_case_numeric_and_one_way() {
        let at = DateTime::parse_from_rfc3339("2026-08-28T09:00:00.123Z")
            .unwrap()
            .with_timezone(&Utc);
        let value = serde_json::to_value(FlagEgressEventModel {
            id: 7,
            cursor: 11,
            game_id: 13,
            participation_id: 17,
            challenge_id: 19,
            container_id: None,
            team_name: "alpha".to_owned(),
            challenge_title: "pwn".to_owned(),
            remote_ip: "192.0.2.10".to_owned(),
            remote_port: 0,
            hit_count: 3,
            first_seen_utc: at,
            last_seen_utc: at,
        })
        .unwrap();
        assert_eq!(value["gameId"], 13);
        assert_eq!(value["cursor"], 11);
        assert_eq!(value["firstSeenUtc"], at.timestamp_millis());
        assert_eq!(value["lastSeenUtc"], at.timestamp_millis());
        assert!(value.get("direction").is_none());
    }

    #[test]
    fn list_and_recovery_queries_are_bound_and_stably_ordered() {
        assert!(COUNT_SQL.contains("event.game_id = $1"));
        assert!(PAGE_SUFFIX.contains("ORDER BY event.last_seen_utc DESC, event.feed_cursor DESC"));
        assert!(PAGE_SUFFIX.contains("OFFSET $3"));
        assert!(PAGE_SUFFIX.contains("LIMIT $4"));
        assert!(BACKFILL_SUFFIX.contains("event.feed_cursor > $2"));
        assert!(BACKFILL_SUFFIX.contains("LIMIT $3"));
        assert!(PAGE_SUFFIX.contains("LIKE $2 ESCAPE '\\'"));
        assert_eq!(page_offset(42), 42);
        assert_eq!(page_offset(MAX_FLAG_EGRESS_SKIP), 10_000);
        assert_eq!(page_offset(u64::MAX), 10_000);
    }

    #[test]
    fn search_is_normalized_capped_and_treats_wildcards_literally() {
        assert_eq!(
            search_pattern(Some("  ReD   Team  ")),
            Some("%red team%".to_owned())
        );
        assert_eq!(search_pattern(Some(" \n\t ")), None);
        assert_eq!(
            search_pattern(Some("100%_\\")),
            Some(r"%100\%\_\\%".to_owned())
        );

        let long = "é".repeat(MAX_FLAG_EGRESS_SEARCH_CHARS + 40);
        let pattern = search_pattern(Some(&long)).unwrap();
        assert_eq!(
            pattern.trim_matches('%').chars().count(),
            MAX_FLAG_EGRESS_SEARCH_CHARS
        );
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn upsert_updates_publish_and_backfill_with_one_stable_id() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("flag_egress_feed_{}", Uuid::new_v4().simple());
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
            .max_connections(4)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "Teams" (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
            CREATE TABLE "Participations" (id INTEGER PRIMARY KEY, team_id INTEGER NOT NULL);
            CREATE TABLE "GameChallenges" (id INTEGER PRIMARY KEY, title TEXT NOT NULL);
            CREATE TABLE "FlagEgressEvents" (
                id INTEGER GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
                game_id INTEGER NOT NULL,
                participation_id INTEGER NOT NULL,
                challenge_id INTEGER NOT NULL,
                container_id UUID,
                remote_ip TEXT NOT NULL,
                remote_port INTEGER NOT NULL,
                hit_count INTEGER NOT NULL,
                first_seen_utc TIMESTAMPTZ NOT NULL,
                last_seen_utc TIMESTAMPTZ NOT NULL
            );
            CREATE UNIQUE INDEX ux_flagegress_event_endpoint
                ON "FlagEgressEvents"(
                    game_id, participation_id, challenge_id,
                    COALESCE(container_id::TEXT, ''::TEXT), remote_ip, remote_port
                );
            INSERT INTO "Teams" VALUES (1, 'Alpha'), (2, 'Other');
            INSERT INTO "Participations" VALUES (11, 1), (12, 2);
            INSERT INTO "GameChallenges" VALUES (21, 'Overflow'), (22, 'Other');
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(crate::migrations::FLAG_EGRESS_FEED_CURSOR_SQL)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(crate::migrations::FLAG_EGRESS_FEED_CURSOR_SQL)
            .execute(&pool)
            .await
            .unwrap();

        let first_id: i32 = sqlx::query_scalar(
            r#"INSERT INTO "FlagEgressEvents"
                   (game_id, participation_id, challenge_id, container_id,
                    remote_ip, remote_port, hit_count, first_seen_utc, last_seen_utc)
               VALUES (7, 11, 21, NULL, '192.0.2.10', 0, 1, now(), now())
               RETURNING id"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let first_cursor = latest_cursor(&pool, 7).await.unwrap();
        assert!(first_cursor > 0);

        let bus = EventBus::local();
        let mut received = bus.subscribe();
        publish_committed_on(&pool, &bus, first_id).await.unwrap();
        let pushed = tokio::time::timeout(Duration::from_secs(1), received.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pushed.target, "ReceivedFlagEgress");
        assert_eq!(pushed.game_id, Some(7));
        let pushed: serde_json::Value = serde_json::from_str(&pushed.payload).unwrap();
        assert_eq!(pushed["id"], first_id);
        assert_eq!(pushed["cursor"], first_cursor);
        assert!(pushed["lastSeenUtc"].is_i64());

        let updated_id: i32 = sqlx::query_scalar(
            r#"INSERT INTO "FlagEgressEvents"
                   (game_id, participation_id, challenge_id, container_id,
                    remote_ip, remote_port, hit_count, first_seen_utc, last_seen_utc)
               VALUES (7, 11, 21, NULL, '192.0.2.10', 0, 1, now(), now())
               ON CONFLICT
                   (game_id, participation_id, challenge_id,
                    (COALESCE(container_id::TEXT, ''::TEXT)), remote_ip, remote_port)
               DO UPDATE SET hit_count = "FlagEgressEvents".hit_count + 1,
                             last_seen_utc = EXCLUDED.last_seen_utc
               RETURNING id"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(updated_id, first_id);
        let second_cursor = latest_cursor(&pool, 7).await.unwrap();
        assert!(second_cursor > first_cursor);

        let recovered = backfill_after(&pool, 7, first_cursor, 100).await.unwrap();
        assert_eq!(recovered.events.len(), 1);
        assert_eq!(recovered.events[0].id, first_id);
        assert_eq!(recovered.events[0].cursor, second_cursor);
        assert_eq!(recovered.events[0].hit_count, 2);

        let filtered = page(&pool, 7, Some("alpha"), 0, 500).await.unwrap();
        assert_eq!(filtered.total, 1);
        assert_eq!(filtered.events.len(), 1);
        assert_eq!(filtered.events[0].team_name, "Alpha");

        pool.close().await;
        let cleanup = format!(r#"DROP SCHEMA "{schema}" CASCADE"#);
        sqlx::query(&cleanup).execute(&admin).await.unwrap();
    }
}
