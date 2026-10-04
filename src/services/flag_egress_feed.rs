//! Durable admin Flag Egress feed and best-effort real-time publication.
//!
//! A row identity remains stable across windowed upserts. Every visible state
//! has a checkpoint-safe `feed_cursor`; states awaiting cursor reconciliation
//! stay hidden from HTTP and SignalR until they can no longer fall behind a
//! checkpoint. All delivery paths share one timestamp-independent DTO.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::app_state::{HubEvent, SharedState};
use crate::services::event_bus::EventBus;

mod reconcile;
pub use reconcile::start_reconciler;

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
    /// Monotonic checkpoint-safe cursor used for reconnect recovery.
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
      FROM (
        SELECT 1
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
         LIMIT $3
      ) bounded_events
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

const COMMITTED_BATCH_SUFFIX: &str = r#"
     WHERE event.game_id = $1
       AND event.id = ANY($2)
       AND event.feed_cursor IS NOT NULL
     ORDER BY event.feed_cursor ASC
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

fn total_limit(page_size: i64) -> i64 {
    MAX_FLAG_EGRESS_SKIP as i64 + page_size
}

/// Return one bounded newest-first page and its bounded filtered total.
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
        .bind(total_limit(limit))
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

pub(crate) async fn publish_committed_batch(
    pool: &sqlx::PgPool,
    events: &EventBus,
    game_id: i32,
    event_ids: &[i32],
) -> anyhow::Result<()> {
    if event_ids.is_empty() {
        return Ok(());
    }
    let mut connection = pool.acquire().await?;
    let messages = committed_by_ids_on(&mut connection, game_id, event_ids, 256).await?;
    publish_messages(events, messages);
    Ok(())
}

pub(super) async fn committed_by_ids_on(
    connection: &mut PgConnection,
    game_id: i32,
    event_ids: &[i32],
    max_batch: usize,
) -> anyhow::Result<Vec<FlagEgressEventModel>> {
    if event_ids.len() > max_batch {
        anyhow::bail!("flag-egress publish batch exceeds {max_batch} rows");
    }
    if event_ids.is_empty() {
        return Ok(Vec::new());
    }
    let sql = projection_with(COMMITTED_BATCH_SUFFIX);
    Ok(sqlx::query_as::<_, FlagEgressEventModel>(&sql)
        .bind(game_id)
        .bind(event_ids)
        .fetch_all(connection)
        .await?)
}

pub(super) fn publish_messages(events: &EventBus, messages: Vec<FlagEgressEventModel>) {
    for message in messages {
        let game_id = message.game_id;
        match serde_json::to_string(&message) {
            Ok(payload) => events.publish(HubEvent {
                target: "ReceivedFlagEgress",
                game_id: Some(game_id),
                payload,
            }),
            Err(error) => tracing::warn!(
                game = game_id,
                event = message.id,
                %error,
                "flag-egress feed row could not be serialized"
            ),
        }
    }
}

async fn committed_or_pending_on(
    connection: &mut PgConnection,
    event_id: i32,
) -> anyhow::Result<Option<FlagEgressEventModel>> {
    let sql = projection_with(COMMITTED_SUFFIX);
    if let Some(message) = sqlx::query_as::<_, FlagEgressEventModel>(&sql)
        .bind(event_id)
        .fetch_optional(&mut *connection)
        .await?
    {
        return Ok(Some(message));
    }
    let pending: bool = sqlx::query_scalar(
        r#"SELECT EXISTS (
             SELECT 1 FROM "FlagEgressFeedPending" WHERE event_id = $1
           )"#,
    )
    .bind(event_id)
    .fetch_one(&mut *connection)
    .await?;
    if pending {
        tracing::debug!(
            event = event_id,
            "flag-egress feed cursor assignment remains pending"
        );
        return Ok(None);
    }
    // Close the race where a reconciler assigned and removed the pending row
    // between the first projection and the queue check.
    if let Some(message) = sqlx::query_as::<_, FlagEgressEventModel>(&sql)
        .bind(event_id)
        .fetch_optional(&mut *connection)
        .await?
    {
        return Ok(Some(message));
    }
    anyhow::bail!("committed flag-egress row is unavailable for publication")
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
    let message = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        committed_or_pending_on(&mut connection, event_id),
    )
    .await
    .map_err(|_| anyhow::anyhow!("flag-egress publish projection timed out"))??;
    if let Some(message) = message {
        publish_messages(events, vec![message]);
    }
    Ok(())
}

pub async fn publish_committed(st: &SharedState, event_id: i32) -> anyhow::Result<()> {
    publish_committed_on(st.pg(), &st.events, event_id).await
}

#[cfg(test)]
mod tests {
    use super::reconcile::{
        assign_pending_on, pending_game_ids, ASSIGN_PENDING_SQL, CURSOR_LOCK_NAMESPACE,
        FIRST_PENDING_GAME_SQL, MAX_ASSIGNMENTS_PER_GAME, NEXT_PENDING_GAME_SQL,
    };
    use super::*;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use std::str::FromStr;
    use std::time::Duration;

    async fn upsert_test_event(
        pool: &sqlx::PgPool,
        game_id: i32,
        participation_id: i32,
        challenge_id: i32,
        remote_port: i32,
    ) -> i32 {
        let mut transaction = pool.begin().await.unwrap();
        let event_id = sqlx::query_scalar(
            r#"INSERT INTO "FlagEgressEvents"
                   (game_id, participation_id, challenge_id, container_id,
                    remote_ip, remote_port, hit_count, first_seen_utc, last_seen_utc)
               VALUES ($1, $2, $3, NULL, '192.0.2.10', $4, 1,
                       clock_timestamp(), clock_timestamp())
               ON CONFLICT
                   (game_id, participation_id, challenge_id,
                    (COALESCE(container_id::TEXT, ''::TEXT)), remote_ip, remote_port)
               DO UPDATE SET hit_count = "FlagEgressEvents".hit_count + 1,
                             last_seen_utc = EXCLUDED.last_seen_utc
               RETURNING id"#,
        )
        .bind(game_id)
        .bind(participation_id)
        .bind(challenge_id)
        .bind(remote_port)
        .fetch_one(&mut *transaction)
        .await
        .unwrap();
        transaction.commit().await.unwrap();
        event_id
    }

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
        assert!(COUNT_SQL.contains("LIMIT $3"));
        assert_eq!(page_offset(42), 42);
        assert_eq!(page_offset(MAX_FLAG_EGRESS_SKIP), 10_000);
        assert_eq!(page_offset(u64::MAX), 10_000);
        assert_eq!(total_limit(50), 10_050);
        assert_eq!(total_limit(MAX_FLAG_EGRESS_PAGE), 10_100);
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

    #[test]
    fn pending_reconciliation_is_nonblocking_bounded_and_fair() {
        assert!(ASSIGN_PENDING_SQL.contains("FOR UPDATE OF queue, event SKIP LOCKED"));
        assert!(ASSIGN_PENDING_SQL.contains("LIMIT $2"));
        assert!(ASSIGN_PENDING_SQL.contains("DELETE FROM \"FlagEgressFeedPending\""));
        assert!(!NEXT_PENDING_GAME_SQL.contains("IS NULL OR"));
        assert!(NEXT_PENDING_GAME_SQL.contains("game_id > $1"));
        assert!(FIRST_PENDING_GAME_SQL.contains("ORDER BY game_id, event_id"));
        assert!(NEXT_PENDING_GAME_SQL.contains("LIMIT 1"));
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

        let first_id = upsert_test_event(&pool, 7, 11, 21, 0).await;
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

        let updated_id = upsert_test_event(&pool, 7, 11, 21, 0).await;
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

        // Holding the same-game cursor fence must not hold proxy commits
        // hostage. Both an aggregate update and a distinct endpoint commit
        // with NULL cursors and durable queue rows.
        let mut blocker = pool.begin().await.unwrap();
        sqlx::query("SELECT pg_advisory_xact_lock($1, $2)")
            .bind(CURSOR_LOCK_NAMESPACE)
            .bind(7_i32)
            .execute(&mut *blocker)
            .await
            .unwrap();
        let concurrent_commits = async {
            tokio::join!(
                upsert_test_event(&pool, 7, 11, 21, 0),
                upsert_test_event(&pool, 7, 11, 21, 1),
            )
        };
        let (pending_update_id, pending_insert_id) =
            tokio::time::timeout(Duration::from_secs(2), concurrent_commits)
                .await
                .expect("flag-egress proxy commits waited behind the cursor fence");
        assert_eq!(pending_update_id, first_id);
        assert_ne!(pending_insert_id, first_id);

        // Updating a row that is already pending must remain nonblocking and
        // retain one queue identity without recursively firing NULL updates.
        let repeated_pending_id = tokio::time::timeout(
            Duration::from_secs(2),
            upsert_test_event(&pool, 7, 11, 21, 0),
        )
        .await
        .expect("a repeated pending Flag Egress update did not commit");
        assert_eq!(repeated_pending_id, first_id);
        let pending_rows: Vec<(i32, Option<i64>)> = sqlx::query_as(
            r#"SELECT event.id, event.feed_cursor
                 FROM "FlagEgressEvents" event
                 JOIN "FlagEgressFeedPending" queue ON queue.event_id = event.id
                WHERE queue.game_id = 7
                ORDER BY event.id"#,
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(pending_rows.len(), 2);
        assert!(pending_rows.iter().all(|(_, cursor)| cursor.is_none()));
        assert!(pending_rows.iter().any(|(id, _)| *id == first_id));
        assert!(pending_rows.iter().any(|(id, _)| *id == pending_insert_id));

        let hidden_pending = page(&pool, 7, None, 0, 100).await.unwrap();
        assert_eq!(hidden_pending.total, 0);
        assert!(hidden_pending.events.is_empty());
        publish_committed_on(&pool, &bus, first_id).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(30), received.recv())
                .await
                .is_err()
        );
        blocker.rollback().await.unwrap();

        // A later visible commit may safely advance the checkpoint. Pending
        // states get fresh cursors above it, so `after = checkpoint` recovers
        // both without a permanent gap.
        let visible_after_pending = upsert_test_event(&pool, 7, 11, 21, 2).await;
        let checkpoint = latest_cursor(&pool, 7).await.unwrap();
        assert!(checkpoint > second_cursor);
        assert!(backfill_after(&pool, 7, checkpoint, 100)
            .await
            .unwrap()
            .events
            .is_empty());

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
        assert!(assigned.contains(&first_id));
        assert!(assigned.contains(&pending_insert_id));
        let pending_after_assignment: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*)::bigint FROM "FlagEgressFeedPending" WHERE game_id = 7"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(pending_after_assignment, 0);

        let recovered = backfill_after(&pool, 7, checkpoint, 100).await.unwrap();
        assert_eq!(recovered.events.len(), 2);
        assert!(recovered
            .events
            .iter()
            .all(|event| event.cursor > checkpoint));
        assert_eq!(
            recovered
                .events
                .iter()
                .find(|event| event.id == first_id)
                .unwrap()
                .hit_count,
            4
        );
        let assigned_ids = assigned.into_iter().collect::<Vec<_>>();
        let mut connection = pool.acquire().await.unwrap();
        let recovered_messages = committed_by_ids_on(
            &mut connection,
            7,
            &assigned_ids,
            MAX_ASSIGNMENTS_PER_GAME as usize,
        )
        .await
        .unwrap();
        publish_messages(&bus, recovered_messages);
        let mut published = std::collections::HashSet::new();
        for _ in 0..2 {
            let event = tokio::time::timeout(Duration::from_secs(1), received.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(event.target, "ReceivedFlagEgress");
            assert_eq!(event.game_id, Some(7));
            let payload: serde_json::Value = serde_json::from_str(&event.payload).unwrap();
            published.insert(payload["id"].as_i64().unwrap() as i32);
        }
        assert_eq!(
            published,
            std::collections::HashSet::from([first_id, pending_insert_id])
        );

        // Rotation is game-fair even if the current game remains non-empty.
        let other_game_id = upsert_test_event(&pool, 8, 12, 22, 0).await;
        sqlx::query(
            r#"INSERT INTO "FlagEgressFeedPending" (event_id, game_id)
               VALUES ($1, 7), ($2, 8)"#,
        )
        .bind(visible_after_pending)
        .bind(other_game_id)
        .execute(&pool)
        .await
        .unwrap();
        let mut last_game_id = Some(7);
        let fair_games = pending_game_ids(&pool, &mut last_game_id).await.unwrap();
        assert_eq!(fair_games.first(), Some(&8));
        assert!(fair_games.contains(&7));
        sqlx::query(r#"DELETE FROM "FlagEgressFeedPending" WHERE event_id = ANY($1)"#)
            .bind([visible_after_pending, other_game_id])
            .execute(&pool)
            .await
            .unwrap();

        sqlx::query(
            r#"INSERT INTO "FlagEgressEvents"
                   (game_id, participation_id, challenge_id, container_id,
                    remote_ip, remote_port, hit_count, first_seen_utc, last_seen_utc)
               SELECT 8, 12, 22, NULL, '198.51.100.10', number, 1,
                      now() - make_interval(secs => number),
                      now() - make_interval(secs => number)
                 FROM generate_series(1, 10101) number"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let bounded = page(&pool, 8, None, 0, 50).await.unwrap();
        assert_eq!(bounded.total, 10_050);
        assert_eq!(bounded.events.len(), 50);
        let last_page = page(&pool, 8, None, MAX_FLAG_EGRESS_SKIP, 50)
            .await
            .unwrap();
        assert_eq!(last_page.total, 10_050);
        assert_eq!(last_page.events.len(), 50);

        pool.close().await;
        let cleanup = format!(r#"DROP SCHEMA "{schema}" CASCADE"#);
        sqlx::query(&cleanup).execute(&admin).await.unwrap();
    }
}
