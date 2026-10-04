//! Fair, bounded recovery for game-event cursor assignments that lost the
//! commit-time non-blocking advisory-lock race.

use sqlx::{Acquire, PgConnection};

use crate::app_state::{HubEvent, SharedState};

pub(super) const CURSOR_LOCK_NAMESPACE: i32 = 1_195_722_068;
pub(super) const MAX_ASSIGNMENTS_PER_GAME: i64 = 100;
pub(super) const MAX_GAMES_PER_PASS: i64 = 16;
const RECONCILE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
const _: () = assert!(MAX_ASSIGNMENTS_PER_GAME <= 100);
const _: () = assert!(MAX_GAMES_PER_PASS <= 16);

pub(super) const ASSIGN_PENDING_SQL: &str = r#"
    WITH pending AS MATERIALIZED (
      SELECT queue.event_id
        FROM "GameEventFeedPending" queue
       WHERE queue.game_id = $1
       ORDER BY queue.event_id
       LIMIT $2
       FOR UPDATE SKIP LOCKED
    ), assigned AS (
      UPDATE "GameEvents" event
         SET feed_cursor = nextval('rsctf_game_event_feed_cursor_seq')
        FROM pending
       WHERE event.id = pending.event_id
         AND event.feed_cursor IS NULL
       RETURNING event.id, event.feed_cursor
    ), removed AS (
      DELETE FROM "GameEventFeedPending" queue
       USING pending
       WHERE queue.event_id = pending.event_id
       RETURNING queue.event_id
    )
    SELECT id FROM assigned ORDER BY feed_cursor ASC
"#;

pub(super) const FIRST_PENDING_GAME_SQL: &str = r#"
    SELECT game_id
      FROM "GameEventFeedPending"
     ORDER BY game_id, event_id
     LIMIT 1
"#;

pub(super) const NEXT_PENDING_GAME_SQL: &str = r#"
    SELECT game_id
      FROM "GameEventFeedPending"
     WHERE game_id > $1
     ORDER BY game_id, event_id
     LIMIT 1
"#;

pub(super) async fn assign_pending_on(
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

pub(super) async fn pending_game_ids(
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

async fn reconcile_pending_once(
    st: &SharedState,
    last_game_id: &mut Option<i32>,
) -> anyhow::Result<usize> {
    let game_ids = pending_game_ids(st.pg(), last_game_id).await?;
    let mut assigned_count = 0;
    for game_id in game_ids {
        let mut connection = st.pg().acquire().await?;
        let event_ids =
            assign_pending_on(&mut connection, game_id, MAX_ASSIGNMENTS_PER_GAME).await?;
        assigned_count += event_ids.len();
        let messages = super::committed_by_ids_on(
            &mut connection,
            &event_ids,
            MAX_ASSIGNMENTS_PER_GAME as usize,
            false,
        )
        .await?;
        for (message_game_id, message) in messages {
            st.events.publish(HubEvent {
                target: "ReceivedGameEvent",
                game_id: Some(message_game_id),
                payload: serde_json::to_string(&message)?,
            });
        }
    }
    Ok(assigned_count)
}

/// Reconcile rows whose commit-time non-blocking cursor attempt lost a race.
/// Every pass is bounded and the per-game fence is non-blocking, so active
/// control replicas cannot convoy writers or one another.
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
                            "reconciled pending game-event feed cursor(s)"
                        ),
                        Ok(_) => {}
                        Err(error) => tracing::warn!(
                            %error,
                            "game-event feed cursor reconciliation failed"
                        ),
                    }
                }
            }
        }
    })
}
