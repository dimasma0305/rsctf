//! Shared cache policy for final, immutable scoreboard representations.

use crate::app_state::SharedState;
use crate::services::cache::Cache;
use crate::utils::error::{AppError, AppResult};

/// Final standings are immutable for ordinary reads. A one-day TTL keeps the
/// Redis working set bounded while covering the clients' entire six-hour
/// settlement window without rebuilding the same version every minute.
pub(crate) const FINAL_SCOREBOARD_CACHE_TTL: std::time::Duration =
    std::time::Duration::from_secs(24 * 60 * 60);

pub(crate) fn scoreboard_cache_ttl(
    live_ttl: std::time::Duration,
    immutable: bool,
) -> std::time::Duration {
    if immutable {
        FINAL_SCOREBOARD_CACHE_TTL
    } else {
        live_ttl
    }
}

/// Every stable rendering key whose value can cross the live-to-final event
/// boundary. Keep this list aligned with the builders in this module tree.
pub(crate) fn scoreboard_render_cache_keys(game_id: i32) -> [String; 16] {
    [
        format!("_ScoreBoard_{game_id}"),
        format!("_ScoreBoardFrozen_{game_id}"),
        format!("_ScoreBoardWireV2_{game_id}"),
        format!("_ScoreBoardWireV2Frozen_{game_id}"),
        format!("_AdScoreBoard_{game_id}"),
        format!("_AdScoreBoard_{game_id}:stale"),
        format!("_AdScoreBoardFrozen_{game_id}"),
        format!("_AdScoreBoardFrozen_{game_id}:stale"),
        format!("_KothScoreBoard_{game_id}"),
        format!("_KothScoreBoardFrozen_{game_id}"),
        format!("_KothScoreBoardWireV2_{game_id}"),
        format!("_KothScoreBoardWireV2Frozen_{game_id}"),
        format!("_KothTimeline_{game_id}"),
        format!("_KothTimelineFrozen_{game_id}"),
        format!("_CombinedScoreBoardByChallenge_{game_id}"),
        format!("_CombinedScoreBoardByChallengeFrozen_{game_id}"),
    ]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScoreboardPublish {
    Published,
    RevisionChanged,
    GameMissing,
}

/// PostgreSQL's row revision is the cross-replica publication fence. Every
/// explicit whole-board invalidation advances this value before deleting the
/// render family, so a fill that began earlier cannot publish after the delete.
pub(crate) async fn scoreboard_render_revision(
    st: &SharedState,
    game_id: i32,
    is_monitor: bool,
) -> AppResult<Option<String>> {
    sqlx::query_scalar::<_, String>(
        r#"SELECT game.xmin::text
             FROM "Games" game
            WHERE game.id = $1 AND (game.hidden = FALSE OR $2)"#,
    )
    .bind(game_id)
    .bind(is_monitor)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

/// Load the game model between two equal revision reads. This binds every field
/// used by a fill (end/practice/hidden/scoring config) to its publication fence
/// even when the handler entered through the one-second game-row cache.
pub(crate) async fn load_scoreboard_game_revision(
    st: &SharedState,
    game_id: i32,
    is_monitor: bool,
) -> AppResult<Option<(crate::models::data::game::Model, String)>> {
    for _ in 0..2 {
        let Some(before) = scoreboard_render_revision(st, game_id, is_monitor).await? else {
            return Ok(None);
        };
        let game = super::load_game(st, game_id).await?;
        if game.hidden && !is_monitor {
            return Ok(None);
        }
        let after = scoreboard_render_revision(st, game_id, is_monitor).await?;
        match after {
            Some(after) if after == before => return Ok(Some((game, before))),
            Some(_) => continue,
            None => return Ok(None),
        }
    }
    Err(AppError::internal(
        "game changed during both scoreboard snapshot attempts",
    ))
}

fn revision_status(expected: &str, observed: Option<&str>) -> ScoreboardPublish {
    match observed {
        Some(observed) if observed == expected => ScoreboardPublish::Published,
        Some(_) => ScoreboardPublish::RevisionChanged,
        None => ScoreboardPublish::GameMissing,
    }
}

/// Publish one rendered value only while its captured game revision remains
/// current. The authoritative cache acknowledgement is required, and a value
/// that loses the post-SET fence is removed with compare-and-delete so it can
/// neither resurrect after closeout nor delete a newer replica's value.
pub(crate) async fn publish_scoreboard_render(
    st: &SharedState,
    game_id: i32,
    is_monitor: bool,
    expected_revision: &str,
    key: &str,
    value: &[u8],
    ttl: std::time::Duration,
) -> AppResult<ScoreboardPublish> {
    let before = scoreboard_render_revision(st, game_id, is_monitor).await?;
    let status = revision_status(expected_revision, before.as_deref());
    if status != ScoreboardPublish::Published {
        return Ok(status);
    }
    if !st.cache.set_confirmed(key, value, Some(ttl)).await {
        let cleanup = st.cache.compare_and_remove_confirmed(key, value).await;
        if cleanup.is_none() {
            return Err(AppError::internal(format!(
                "scoreboard cache backend acknowledged neither publication nor cleanup for {key}"
            )));
        }
        return Err(AppError::internal(format!(
            "scoreboard cache backend did not acknowledge publication for {key}"
        )));
    }

    let after = match scoreboard_render_revision(st, game_id, is_monitor).await {
        Ok(after) => after,
        Err(error) => {
            if st
                .cache
                .compare_and_remove_confirmed(key, value)
                .await
                .is_none()
            {
                return Err(AppError::internal(format!(
                    "scoreboard revision validation and cache cleanup both failed for {key}: {error}"
                )));
            }
            return Err(error);
        }
    };
    let status = revision_status(expected_revision, after.as_deref());
    if status == ScoreboardPublish::Published {
        return Ok(status);
    }
    if st
        .cache
        .compare_and_remove_confirmed(key, value)
        .await
        .is_none()
    {
        return Err(AppError::internal(format!(
            "scoreboard cache backend did not acknowledge stale publication cleanup for {key}"
        )));
    }
    Ok(status)
}

#[cfg(test)]
pub(crate) async fn evict_scoreboard_render_cache(cache: &dyn Cache, game_id: i32) {
    let keys = scoreboard_render_cache_keys(game_id);
    futures::future::join_all(keys.iter().map(|key| cache.remove(key))).await;
}

async fn evict_scoreboard_render_cache_confirmed(cache: &dyn Cache, game_id: i32) -> AppResult<()> {
    let keys = scoreboard_render_cache_keys(game_id);
    let results =
        futures::future::join_all(keys.iter().map(|key| cache.remove_confirmed(key))).await;
    if results.into_iter().all(|removed| removed) {
        Ok(())
    } else {
        Err(AppError::internal(
            "scoreboard cache backend did not acknowledge complete render eviction",
        ))
    }
}

/// Fence fills that started before event close, then evict the complete render
/// family. The durable closeout worker calls this at most once per event-end
/// version; organizer mutations may call it explicitly as a repair boundary.
pub(crate) async fn invalidate_scoreboard_render_version(
    st: &SharedState,
    game_id: i32,
) -> AppResult<()> {
    let updated = sqlx::query(r#"UPDATE "Games" SET id = id WHERE id = $1"#)
        .bind(game_id)
        .execute(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .rows_affected();
    if updated != 1 {
        return Err(AppError::not_found("Game not found"));
    }
    evict_scoreboard_render_cache_confirmed(st.cache.as_ref(), game_id).await
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use async_trait::async_trait;
    use bytes::Bytes;

    use super::*;
    use crate::services::cache::InMemoryCache;

    struct UnacknowledgedRemovalCache {
        inner: InMemoryCache,
    }

    #[async_trait]
    impl Cache for UnacknowledgedRemovalCache {
        async fn get(&self, key: &str) -> Option<Bytes> {
            self.inner.get(key).await
        }

        async fn get_and_remove(&self, key: &str) -> Option<Bytes> {
            self.inner.get_and_remove(key).await
        }

        async fn compare_and_remove(&self, key: &str, expected: &[u8]) -> bool {
            self.inner.compare_and_remove(key, expected).await
        }

        async fn set_if_absent(
            &self,
            key: &str,
            value: &[u8],
            ttl: Option<std::time::Duration>,
        ) -> bool {
            self.inner.set_if_absent(key, value, ttl).await
        }

        async fn set(&self, key: &str, value: &[u8], ttl: Option<std::time::Duration>) {
            self.inner.set(key, value, ttl).await;
        }

        async fn remove(&self, _key: &str) {}
    }

    #[test]
    fn immutable_ttl_outlives_the_old_six_hour_sweep_but_remains_bounded() {
        assert!(FINAL_SCOREBOARD_CACHE_TTL > std::time::Duration::from_secs(6 * 60 * 60));
        assert!(FINAL_SCOREBOARD_CACHE_TTL <= std::time::Duration::from_secs(7 * 24 * 60 * 60));
        assert_eq!(
            scoreboard_cache_ttl(std::time::Duration::from_secs(5), true),
            FINAL_SCOREBOARD_CACHE_TTL
        );
        assert_eq!(
            scoreboard_cache_ttl(std::time::Duration::from_secs(5), false),
            std::time::Duration::from_secs(5)
        );
    }

    #[test]
    fn complete_render_family_has_unique_real_keys() {
        let keys = scoreboard_render_cache_keys(17);
        assert_eq!(keys.iter().collect::<HashSet<_>>().len(), keys.len());
        assert!(keys.contains(&"_CombinedScoreBoardByChallenge_17".to_owned()));
        assert!(keys.contains(&"_CombinedScoreBoardByChallengeFrozen_17".to_owned()));
        assert!(keys.contains(&"_AdScoreBoard_17:stale".to_owned()));
    }

    #[test]
    fn revision_status_distinguishes_current_changed_and_missing_games() {
        assert_eq!(
            revision_status("17", Some("17")),
            ScoreboardPublish::Published
        );
        assert_eq!(
            revision_status("17", Some("18")),
            ScoreboardPublish::RevisionChanged
        );
        assert_eq!(revision_status("17", None), ScoreboardPublish::GameMissing);
    }

    #[tokio::test]
    async fn closeout_rejects_an_unacknowledged_cache_eviction() {
        let cache = UnacknowledgedRemovalCache {
            inner: InMemoryCache::new(),
        };
        let key = scoreboard_render_cache_keys(17)[0].clone();
        cache.set(&key, b"stale", None).await;
        assert!(evict_scoreboard_render_cache_confirmed(&cache, 17)
            .await
            .is_err());
        assert_eq!(cache.get(&key).await.as_deref(), Some(b"stale".as_slice()));
    }
}
