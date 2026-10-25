//! Opt-in read-ahead for recently viewed boards. Never serve past the normal TTL.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use futures::StreamExt;

use super::*;

const MAX_ACTIVE_VIEWS: usize = 64;
const ACTIVE_MILLIS: u64 = 30_000;
const REFRESH_MILLIS: u64 = 3_000;

static ENABLED: LazyLock<bool> = LazyLock::new(|| {
    std::env::var("RSCTF_SCOREBOARD_BACKGROUND_REFRESH")
        .is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
});
static STARTED: LazyLock<Instant> = LazyLock::new(Instant::now);
static INTEREST: LazyLock<DashMap<(i32, bool), Arc<Interest>>> = LazyLock::new(DashMap::new);
// Registration is rare; existing hot entries only update an atomic timestamp.
static REGISTER: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Interest {
    last_request: AtomicU64,
    next_refresh: AtomicU64,
}

fn now_millis() -> u64 {
    u64::try_from(STARTED.elapsed().as_millis()).unwrap_or(u64::MAX)
}

pub(super) fn observe(game: &game::Model, monitor: bool) {
    if !*ENABLED || Utc::now() >= game.end_time_utc {
        return;
    }
    let key = (game.id, monitor);
    let now = now_millis();
    if let Some(entry) = INTEREST.get(&key) {
        entry.last_request.store(now, Relaxed);
        return;
    }
    let _registration = REGISTER.lock().unwrap_or_else(|error| error.into_inner());
    INTEREST
        .retain(|_, entry| now.saturating_sub(entry.last_request.load(Relaxed)) < ACTIVE_MILLIS);
    if INTEREST.len() < MAX_ACTIVE_VIEWS {
        INTEREST.entry(key).or_insert_with(|| {
            Arc::new(Interest {
                last_request: AtomicU64::new(now),
                next_refresh: AtomicU64::new(now + REFRESH_MILLIS),
            })
        });
    }
}

async fn refresh_active(st: &SharedState) {
    let now = now_millis();
    INTEREST
        .retain(|_, entry| now.saturating_sub(entry.last_request.load(Relaxed)) < ACTIVE_MILLIS);
    let due: Vec<_> = INTEREST
        .iter()
        .filter_map(|entry| {
            let next = entry.next_refresh.load(Relaxed);
            (next <= now
                && entry
                    .next_refresh
                    .compare_exchange(next, now + REFRESH_MILLIS, Relaxed, Relaxed)
                    .is_ok())
            .then_some(*entry.key())
        })
        .collect();
    futures::stream::iter(due)
        .for_each_concurrent(2, |(id, monitor)| async move {
            // Keep the lease until expiry, preventing staggered replicas from each
            // repeating the same refresh. A failed/unavailable Redis skips read-ahead.
            let lease = format!("_ScoreboardRefreshV1_{id}:{monitor}");
            if !st
                .cache
                .set_if_absent_authoritative(&lease, b"refresh", Some(Duration::from_secs(3)))
                .await
            {
                return;
            }
            let work = async {
                let game = load_game_cached(st, id).await?;
                refresh_scoreboard_bundle(st, &game, monitor).await?;
                if Utc::now() >= game.end_time_utc {
                    INTEREST.remove(&(id, monitor));
                }
                Ok::<_, AppError>(())
            };
            match tokio::time::timeout(Duration::from_secs(2), work).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::warn!(game = id, %error, "scoreboard read-ahead failed"),
                Err(_) => tracing::warn!(game = id, "scoreboard read-ahead exceeded its budget"),
            }
        })
        .await;
}

/// Publish only over the exact generation observed before computation. A
/// concurrent invalidation or writer wins; background work cannot resurrect it.
async fn refresh_scoreboard_bundle(
    st: &SharedState,
    game: &game::Model,
    monitor: bool,
) -> AppResult<()> {
    use super::scoreboard_board::{
        encode_current_scoreboard, index_scoreboard, scoreboard_cache_key, SCOREBOARD_CACHE_TTL,
    };
    let key = scoreboard_cache_key(game, monitor);
    let expected = st.cache.get_authoritative(&key).await;
    // Read metadata AFTER the expected generation. A replica's one-second game
    // cache may precede an organizer's freeze/edit whose new board is already
    // published. Never replace that board using stale cached game metadata.
    let game = load_game(st, game.id).await?;
    let Some(expected) = expected else {
        build_scoreboard_bundle(st, &game, monitor).await?;
        return Ok(());
    };
    let (model, built) = encode_current_scoreboard(st, &game, monitor).await?;
    if built.cacheable {
        let ttl = super::scoreboard_encoding::final_or_live_cache_ttl(
            !game.practice_mode && Utc::now() >= game.end_time_utc,
            SCOREBOARD_CACHE_TTL,
        );
        if st
            .cache
            .compare_and_set_authoritative(&key, &expected, &built.bytes, Some(ttl))
            .await
        {
            index_scoreboard(key, &model);
        }
    }
    Ok(())
}

pub fn start_scoreboard_refresh(
    st: SharedState,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { return; }
                }
                _ = ticker.tick(), if *ENABLED => {
                    tokio::select! {
                        _ = shutdown.changed() => return,
                        _ = refresh_active(&st) => {}
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::super::ai_chats::tests::fixture;
    use super::*;

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn read_ahead_with_stale_game_metadata_cannot_unfreeze_a_new_public_snapshot() {
        let f = fixture().await;
        let old = load_game(&f.st, f.off_game).await.unwrap();
        build_scoreboard_bundle(&f.st, &old, false).await.unwrap();
        sqlx::query(
            r#"UPDATE "Games" SET freeze_time_utc = now() - interval '100 minutes',
            end_time_utc = now() + interval '1 hour', writeup_deadline = now() + interval '2 hours'
            WHERE id = $1"#,
        )
        .bind(f.off_game)
        .execute(&f.pool)
        .await
        .unwrap();
        super::super::invalidate_standard_scoreboard(&f.st, f.off_game).await;
        let fresh = load_game(&f.st, f.off_game).await.unwrap();
        let expected = build_scoreboard_cached(&f.st, &fresh, false).await.unwrap();
        assert!(expected.is_frozen_view);
        assert_eq!(expected.items[0].score, 0);
        // Model the remote replica still holding its pre-edit game row.
        refresh_scoreboard_bundle(&f.st, &old, false).await.unwrap();
        let actual = build_scoreboard_cached(&f.st, &fresh, false).await.unwrap();
        assert!(actual.is_frozen_view);
        assert_eq!(actual.items[0].score, 0);
        f.teardown().await;
    }
}
