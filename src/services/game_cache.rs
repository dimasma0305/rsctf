//! Shared game-row reads and generation-fenced cache invalidation.

use crate::app_state::SharedState;
use crate::models::data::game;
use crate::utils::error::{AppError, AppResult};
use sea_orm::{EntityTrait, QueryFilter};
use std::collections::HashMap;

#[derive(Clone, Default)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum GameLoadFlight {
    Found(game::Model),
    NotFound,
    #[default]
    Failed,
}

/// One-second, read-only game-row cache for hot board endpoints. Mutation paths
/// use [`load_game`]; explicit invalidation handles organizer timing edits.
static GAME_ROW_CACHE: std::sync::LazyLock<
    std::sync::RwLock<HashMap<i32, (game::Model, std::time::Instant)>>,
> = std::sync::LazyLock::new(|| std::sync::RwLock::new(HashMap::new()));

/// Coalesce concurrent reloads at the one-second TTL boundary, including
/// negative and failed lookups so an invalid id or database outage cannot
/// trigger a follower recompute herd.
static GAME_SF: std::sync::LazyLock<crate::utils::single_flight::SingleFlight<GameLoadFlight>> =
    std::sync::LazyLock::new(crate::utils::single_flight::SingleFlight::new);

const GAME_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(1);

pub(crate) async fn load_game_cached(st: &SharedState, id: i32) -> AppResult<game::Model> {
    let generation = game_row_cache_generation(id);
    if let Ok(cache) = GAME_ROW_CACHE.read() {
        if let Some((g, at)) = cache.get(&id) {
            if at.elapsed() < GAME_CACHE_TTL {
                return Ok(g.clone());
            }
        }
    }
    // Miss: single-flight the DB load. Preserve the public 404 contract while
    // broadcasting transient failures as one generic 500.
    let st2 = st.clone();
    let coalesced = GAME_SF
        .run(&format!("game:{id}:v{generation}"), move || async move {
            if let Ok(cache) = GAME_ROW_CACHE.read() {
                if let Some((g, at)) = cache.get(&id) {
                    if at.elapsed() < GAME_CACHE_TTL {
                        return GameLoadFlight::Found(g.clone());
                    }
                }
            }
            match load_game(&st2, id).await {
                Ok(g) => {
                    cache_game_row_if_current(id, g.clone(), generation);
                    GameLoadFlight::Found(g)
                }
                Err(AppError::NotFound(_)) => GameLoadFlight::NotFound,
                Err(error) => {
                    tracing::warn!(game = id, %error, "game cache fill failed");
                    GameLoadFlight::Failed
                }
            }
        })
        .await;
    match coalesced {
        GameLoadFlight::Found(g) => Ok(g),
        GameLoadFlight::NotFound => Err(AppError::not_found("Game not found")),
        GameLoadFlight::Failed => Err(AppError::internal("game cache fill failed")),
    }
}

static GAME_ROW_CACHE_GENERATIONS: std::sync::LazyLock<std::sync::RwLock<HashMap<i32, u64>>> =
    std::sync::LazyLock::new(|| std::sync::RwLock::new(HashMap::new()));

pub(crate) fn game_row_cache_generation(id: i32) -> u64 {
    GAME_ROW_CACHE_GENERATIONS
        .read()
        .ok()
        .and_then(|generations| generations.get(&id).copied())
        .unwrap_or(0)
}

pub(crate) fn cache_game_row_if_current(id: i32, game: game::Model, generation: u64) {
    if game_row_cache_generation(id) != generation {
        return;
    }
    if let Ok(mut cache) = GAME_ROW_CACHE.write() {
        // Recheck after taking the cache write lock so invalidation cannot slip
        // between the generation check and insertion.
        if game_row_cache_generation(id) == generation {
            cache.insert(id, (game, std::time::Instant::now()));
        }
    }
}

pub(crate) async fn load_game(st: &SharedState, id: i32) -> AppResult<game::Model> {
    game::Entity::find_by_id(id)
        .filter(sea_orm::sea_query::Expr::cust(
            "\"Games\".deletion_pending = FALSE",
        ))
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Game not found"))
}

pub(crate) fn invalidate_game_row_cache(id: i32) {
    // Cache lock first is the global order also used by insertion. Incrementing
    // makes a pre-invalidation single-flight leader unable to repopulate the row.
    if let Ok(mut cache) = GAME_ROW_CACHE.write() {
        cache.remove(&id);
        if let Ok(mut generations) = GAME_ROW_CACHE_GENERATIONS.write() {
            let generation = generations.entry(id).or_insert(0);
            *generation = generation.wrapping_add(1);
        }
    }
}
