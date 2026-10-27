//! Independent bounded end-of-event teardown. The row retains its runtime
//! identity until snapshot/capture fencing and destruction have completed.
use std::time::Duration;

use tokio::sync::watch;
use uuid::Uuid;

use crate::app_state::SharedState;

const ITEM_BUDGET: Duration = Duration::from_secs(240);
const PASS_BUDGET: Duration = Duration::from_secs(270);
const PASS_ITEMS: usize = 16;
const LEASE_SECONDS: f64 = 600.0;

#[derive(Clone, Copy, Debug)]
enum Kind {
    Ad,
    Koth,
}

impl Kind {
    fn table(self) -> &'static str {
        match self {
            Self::Ad => "AdTeamServices",
            Self::Koth => "KothTargets",
        }
    }
}

async fn claim(pool: &sqlx::PgPool, kind: Kind) -> Result<Option<(i32, Uuid)>, sqlx::Error> {
    // Table identifiers come exclusively from the closed enum; all values are bound.
    let table = kind.table();
    let sql = format!(
        r#"WITH candidate AS (
        SELECT item.id FROM "{table}" item JOIN "Games" game ON game.id = item.game_id
         WHERE game.end_time_utc <= clock_timestamp() - make_interval(secs => $1)
           AND (item.container_id IS NOT NULL OR item.host <> '' OR item.cleanup_lease_token IS NOT NULL)
           AND item.cleanup_next_at <= clock_timestamp()
           AND (item.cleanup_lease_until IS NULL OR item.cleanup_lease_until <= clock_timestamp())
         ORDER BY item.cleanup_next_at, item.id
         FOR UPDATE OF game, item SKIP LOCKED LIMIT 1
    ) UPDATE "{table}" item
         SET cleanup_lease_token = $2,
             cleanup_lease_until = clock_timestamp() + make_interval(secs => $3),
             cleanup_attempts = LEAST(item.cleanup_attempts, 30) + 1
        FROM candidate WHERE item.id = candidate.id
    RETURNING item.id, item.cleanup_lease_token"#
    );
    sqlx::query_as(&sql)
        .bind(super::ADVANCE_BUDGET_SECS as f64)
        .bind(Uuid::new_v4())
        .bind(LEASE_SECONDS)
        .fetch_optional(pool)
        .await
}

async fn finish(
    pool: &sqlx::PgPool,
    kind: Kind,
    id: i32,
    token: Uuid,
    success: bool,
) -> Result<bool, sqlx::Error> {
    let table = kind.table();
    let sql = format!(
        r#"UPDATE "{table}"
        SET cleanup_lease_token = CASE WHEN $3 OR (container_id IS NULL AND host = '')
                                      THEN NULL ELSE cleanup_lease_token END,
            cleanup_lease_until = CASE WHEN $3 OR (container_id IS NULL AND host = '')
                                      THEN NULL ELSE clock_timestamp() + make_interval(secs =>
                                          LEAST(900, 30 * (1 << LEAST(cleanup_attempts, 5)))) END,
            cleanup_next_at = clock_timestamp() + make_interval(secs =>
                CASE WHEN $3 THEN 30 ELSE LEAST(900, 30 * (1 << LEAST(cleanup_attempts, 5))) END),
            cleanup_attempts = CASE WHEN $3 OR (container_id IS NULL AND host = '')
                                    THEN 0 ELSE cleanup_attempts END
      WHERE id = $1 AND cleanup_lease_token = $2"#
    );
    Ok(sqlx::query(&sql)
        .bind(id)
        .bind(token)
        .bind(success)
        .execute(pool)
        .await?
        .rows_affected()
        == 1)
}

/// The caller holds the Games row FOR UPDATE until its schedule change commits.
/// Claim takes that same row lock, so an extension either wins before teardown
/// admission or observes its durable fence. Expiry permits executor recovery,
/// not resuming an event whose external destruction might still be in flight.
pub(crate) async fn ensure_extension_safe(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
) -> crate::utils::error::AppResult<()> {
    let pending: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
        SELECT 1 FROM "AdTeamServices" WHERE game_id = $1 AND cleanup_lease_token IS NOT NULL
        UNION ALL
        SELECT 1 FROM "KothTargets" WHERE game_id = $1 AND cleanup_lease_token IS NOT NULL
    )"#,
    )
    .bind(game_id)
    .fetch_one(connection)
    .await
    .map_err(|error| crate::utils::error::AppError::internal(error.to_string()))?;
    if pending {
        return Err(crate::utils::error::AppError::conflict(
            "Event cleanup is running or recovering. Retry extending the event after cleanup finishes.",
        ));
    }
    Ok(())
}

pub(super) async fn supervise(state: SharedState, mut shutdown: watch::Receiver<bool>) {
    let mut ticker = tokio::time::interval(Duration::from_secs(30));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut next = 0;
    loop {
        tokio::select! {
            _ = shutdown.changed() => return,
            _ = ticker.tick() => {}
        }
        let deadline = tokio::time::Instant::now() + PASS_BUDGET;
        for _ in 0..PASS_ITEMS {
            if *shutdown.borrow() || tokio::time::Instant::now() >= deadline {
                break;
            }
            let kind = [Kind::Ad, Kind::Koth][next];
            next = (next + 1) % 2;
            let (id, token) = match claim(state.pg(), kind).await {
                Ok(Some(claimed)) => claimed,
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(%error, "backend cleanup claim failed");
                    break;
                }
            };
            if *shutdown.borrow() {
                let _ = finish(state.pg(), kind, id, token, true).await;
                return;
            }
            let work = async {
                match kind {
                    Kind::Ad => super::backend_reaper::reap_ad_service(&state, id).await,
                    Kind::Koth => super::backend_reaper::reap_koth_target(&state, id).await,
                }
            };
            let result = tokio::time::timeout_at(
                deadline.min(tokio::time::Instant::now() + ITEM_BUDGET),
                work,
            )
            .await;
            let success = match result {
                Ok(Ok(count)) => count > 0,
                Ok(Err(error)) => {
                    tracing::warn!(?kind, id, %error, "backend cleanup failed");
                    false
                }
                Err(_) => {
                    // An external operation might outlive cancellation. Keep the
                    // lease until its recovery window; never clear backend identity.
                    tracing::warn!(?kind, id, "backend cleanup timed out; lease retained");
                    continue;
                }
            };
            if let Err(error) = finish(state.pg(), kind, id, token, success).await {
                tracing::warn!(%error, "backend cleanup completion failed");
            }
        }
    }
}

#[cfg(test)]
#[path = "backend_cleanup_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "backend_cleanup_runtime_tests.rs"]
mod runtime_tests;
