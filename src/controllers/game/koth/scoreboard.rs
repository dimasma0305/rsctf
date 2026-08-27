//! Cached KotH scoreboard rendering shared by player and operator reads.

use super::*;
use axum::http::HeaderMap;
use axum::response::Response;

use crate::controllers::game::load_game_cached;
use crate::middlewares::privilege_authentication::MaybeUser;

/// Cache + coalesce the KotH board like the jeopardy + A&D boards. Its recompute
/// (`compute_koth_board` — a per-hill/-team scan of the control-result history)
/// otherwise ran on EVERY poll (measured ~26× slower than the cached boards, with
/// Postgres pinned at ~216% under a poll flood). Live player/operator reads share
/// one game key; the public freeze variant bakes the cutoff, so a cached copy is only
/// ever `KOTH_CACHE_TTL` stale across the freeze/end boundary — the same tradeoff
/// the other cached boards accept.
static KOTH_SF: std::sync::LazyLock<
    crate::utils::single_flight::SingleFlight<Option<bytes::Bytes>>,
> = std::sync::LazyLock::new(crate::utils::single_flight::SingleFlight::new);
const KOTH_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(5);

fn koth_cache_key(
    game_id: i32,
    freeze: Option<DateTime<Utc>>,
    end: DateTime<Utc>,
    now: DateTime<Utc>,
    is_monitor: bool,
) -> String {
    if crate::utils::scoring::public_scoreboard_frozen(freeze, end, now, is_monitor) {
        format!("_KothScoreBoardWireV2Frozen_{game_id}")
    } else {
        format!("_KothScoreBoardWireV2_{game_id}")
    }
}

/// Hidden event standings stay undiscoverable to ordinary callers while the
/// authenticated monitor retains the same operational view exposed by the
/// combined scoreboard and other game read endpoints.
pub(in crate::controllers::game) fn can_view_koth_standings(
    game_hidden: bool,
    is_monitor: bool,
) -> bool {
    !game_hidden || is_monitor
}

/// Compute the rendered KotH board for `(game, is_monitor)`: derive the ICPC
/// freeze / post-end cutoff, run [`compute_koth_board`], and shape the wire model.
async fn build_koth_scoreboard(
    st: &SharedState,
    game: &game::Model,
    is_monitor: bool,
    now: DateTime<Utc>,
) -> AppResult<KothScoreboardModel> {
    // ICPC freeze: a non-monitor inside `[FreezeTimeUtc, EndTimeUtc)` sees the
    // FROZEN board; monitors always see it live.
    let is_frozen_view = crate::utils::scoring::public_scoreboard_frozen(
        game.freeze_time_utc,
        game.end_time_utc,
        now,
        is_monitor,
    );
    let mut cutoff: Option<DateTime<Utc>> =
        is_frozen_view.then_some(game.freeze_time_utc).flatten();
    // After the game ends, freeze the rendered board at the end instant.
    if now >= game.end_time_utc {
        cutoff = Some(cutoff.map_or(game.end_time_utc, |c| c.min(game.end_time_utc)));
    }

    let board = compute_koth_board(st, game.id, cutoff, false).await?;
    let mut lifecycle = load_lifecycle_map(st, game.id, board.latest_round, cutoff).await?;
    // The player board only shows enabled hills (an admin can disable one mid-game).
    let enabled: Vec<&KothHillInfo> = board.hills.iter().filter(|h| h.is_enabled).collect();
    let hills: Vec<KothScoreboardHill> = enabled
        .iter()
        .map(|h| {
            let view = lifecycle.remove(&h.challenge_id).unwrap_or_default();
            KothScoreboardHill {
                challenge_id: h.challenge_id,
                title: h.title.clone(),
                category: h.category,
                claim_source: h.claim_source.clone(),
                current_holder_team_name: board
                    .holder_team_name_by_challenge
                    .get(&h.challenge_id)
                    .cloned(),
                current_holder_participation_id: board
                    .holder_by_challenge
                    .get(&h.challenge_id)
                    .copied(),
                provisional_claimant_team_name: view.provisional_team_name,
                provisional_claimant_participation_id: view.provisional_participation_id,
                provisional_confirmation_ticks: view.confirmation_progress,
                cycle_number: view.cycle_number,
                cycle_tick: view.cycle_tick,
                reset_phase: view.reset_phase,
                is_scorable: view.is_scorable,
                next_reset_ticks: view.next_reset_ticks,
                cooldown_participants: view.cooldown_participants,
                last_check_status: board
                    .latest_control_by_challenge
                    .get(&h.challenge_id)
                    .map(|(s, _)| s.clone()),
            }
        })
        .collect();
    let teams = build_team_rows(&board, &enabled);
    let current_epoch = board
        .scoring_start_round
        .filter(|start| board.latest_round >= *start)
        .map_or(0, |start| {
            ((board.latest_round - start) / board.epoch_ticks) + 1
        });
    Ok(KothScoreboardModel {
        epoch_ticks: game.koth_epoch_ticks,
        cycle_ticks: game.koth_cycle_ticks,
        champion_cooldown_ticks: game.koth_champion_cooldown_ticks,
        claim_confirmation_ticks: game.koth_claim_confirmation_ticks,
        start_round: board.scoring_start_round,
        started: board.scoring_start_round.is_some(),
        fully_settled: board.scoring.fully_settled,
        current_epoch,
        detail_epoch_limit: KOTH_DETAIL_EPOCH_LIMIT,
        latest_round: board.latest_round,
        current_round_ends_at: board.current_round_ends_at,
        tick_seconds: board.tick_seconds,
        generated_at: Utc::now(),
        is_frozen_view,
        freeze: board.freeze,
        hills,
        teams,
    })
}

async fn koth_scoreboard_bundle(
    st: &SharedState,
    game: &game::Model,
    is_monitor: bool,
) -> AppResult<bytes::Bytes> {
    let now = Utc::now();
    let key = koth_cache_key(
        game.id,
        game.freeze_time_utc,
        game.end_time_utc,
        now,
        is_monitor,
    );
    let (st2, game2) = (st.clone(), game.clone());
    cached_koth_bundle(st.cache.clone(), key.clone(), move || async move {
        let model = build_koth_scoreboard(&st2, &game2, is_monitor, now)
            .await
            .ok()?;
        let raw = bytes::Bytes::from(serde_json::to_vec(&model).ok()?);
        let built =
            super::super::scoreboard_encoding::build_stable_bundle(raw, key, b"\"generatedAt\":")
                .await
                .ok()?;
        Some((built.bytes, built.cacheable))
    })
    .await
}

async fn cached_koth_bundle<Build, BuildFuture>(
    cache: std::sync::Arc<dyn crate::services::cache::Cache>,
    key: String,
    build: Build,
) -> AppResult<bytes::Bytes>
where
    Build: FnOnce() -> BuildFuture + Send + 'static,
    BuildFuture: std::future::Future<Output = Option<(bytes::Bytes, bool)>> + Send + 'static,
{
    if let Some(bytes) = cache.get(&key).await {
        if super::super::scoreboard_encoding::valid_bundle(&bytes) {
            return Ok(bytes);
        }
        tracing::warn!(
            cache_key = key,
            "evicting corrupt KotH scoreboard cache entry"
        );
        cache.remove(&key).await;
    }
    let (cache2, key2) = (cache, key.clone());
    let coalesced = KOTH_SF
        .run(&key, move || async move {
            if let Some(bytes) = cache2.get(&key2).await {
                if super::super::scoreboard_encoding::valid_bundle(&bytes) {
                    return Some(bytes);
                }
                cache2.remove(&key2).await;
            }
            let (bytes, cacheable) = build().await?;
            if cacheable {
                cache2.set(&key2, &bytes, Some(KOTH_CACHE_TTL)).await;
            }
            Some(bytes)
        })
        .await;
    match coalesced {
        Some(bytes) => Ok(bytes),
        None => Err(AppError::internal("KotH scoreboard cache fill failed")),
    }
}

pub(crate) async fn build_koth_scoreboard_cached(
    st: &SharedState,
    game: &game::Model,
    is_monitor: bool,
) -> AppResult<KothScoreboardModel> {
    let bundle = koth_scoreboard_bundle(st, game, is_monitor).await?;
    let raw = super::super::scoreboard_encoding::identity_body(bundle)?;
    serde_json::from_slice(&raw).map_err(|error| AppError::internal(error.to_string()))
}

/// `GET /api/game/{id}/ad/koth/scoreboard` — the player KotH board: one column per
/// enabled hill, one ranked row per team with its bounded per-hill epoch score. Served
/// from the two-tier cache as raw bytes (byte-identical to the model), so a poll
/// flood no longer recomputes the board on every request.
pub async fn scoreboard(
    State(st): State<SharedState>,
    MaybeUser(maybe): MaybeUser,
    Path(game_id): Path<i32>,
    headers: HeaderMap,
) -> AppResult<Response> {
    // Keep hidden events undiscoverable to ordinary callers while allowing the
    // authenticated monitor to operate the private event. 1s-cached game row.
    let game = load_game_cached(&st, game_id).await?;
    let is_monitor = maybe.as_ref().is_some_and(|u| u.is_monitor());
    if !can_view_koth_standings(game.hidden, is_monitor) {
        return Err(AppError::not_found("Game not found"));
    }
    let bundle = koth_scoreboard_bundle(&st, &game, is_monitor).await?;
    let validator_scope = if is_monitor {
        "koth-monitor"
    } else {
        "koth-public"
    };
    super::super::scoreboard_encoding::scoped_response(bundle, &headers, validator_scope)
}

#[cfg(test)]
mod tests {
    use super::{cached_koth_bundle, can_view_koth_standings, koth_cache_key};
    use chrono::{TimeZone, Utc};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[test]
    fn hidden_standings_are_monitor_only() {
        assert!(can_view_koth_standings(false, false));
        assert!(can_view_koth_standings(false, true));
        assert!(can_view_koth_standings(true, true));
        assert!(!can_view_koth_standings(true, false));
    }

    #[test]
    fn live_player_and_operator_views_share_one_scoring_version() {
        let start = Utc.with_ymd_and_hms(2026, 8, 27, 8, 0, 0).unwrap();
        let freeze = start + chrono::Duration::hours(1);
        let end = start + chrono::Duration::hours(2);
        assert_eq!(
            koth_cache_key(9, Some(freeze), end, start, false),
            koth_cache_key(9, Some(freeze), end, start, true)
        );
        assert_ne!(
            koth_cache_key(9, Some(freeze), end, freeze, false),
            koth_cache_key(9, Some(freeze), end, freeze, true)
        );
        assert_eq!(
            koth_cache_key(9, Some(freeze), end, end, false),
            koth_cache_key(9, Some(freeze), end, end, true)
        );
    }

    #[tokio::test]
    async fn concurrent_operator_tabs_build_one_cold_scoring_version() {
        let cache: Arc<dyn crate::services::cache::Cache> =
            Arc::new(crate::services::cache::InMemoryCache::new());
        let builds = Arc::new(AtomicUsize::new(0));
        let key = format!(
            "koth-scoreboard-single-flight-test:{}",
            uuid::Uuid::new_v4()
        );
        let readers = (0..32).map(|_| {
            let cache = cache.clone();
            let builds = builds.clone();
            let key = key.clone();
            async move {
                cached_koth_bundle(cache, key, move || async move {
                    builds.fetch_add(1, Ordering::SeqCst);
                    tokio::task::yield_now().await;
                    Some((bytes::Bytes::from_static(b"{\"version\":41}"), true))
                })
                .await
                .unwrap()
            }
        });
        let responses = futures::future::join_all(readers).await;
        assert!(responses
            .iter()
            .all(|response| response.as_ref() == b"{\"version\":41}"));
        assert_eq!(builds.load(Ordering::SeqCst), 1);
        let second_version_builds = builds.clone();
        let cached = cached_koth_bundle(cache, key, move || async move {
            second_version_builds.fetch_add(1, Ordering::SeqCst);
            Some((bytes::Bytes::from_static(b"unexpected"), true))
        })
        .await
        .unwrap();
        assert_eq!(cached.as_ref(), b"{\"version\":41}");
        assert_eq!(builds.load(Ordering::SeqCst), 1);
    }
}
