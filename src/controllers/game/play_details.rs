//! Split challenge-catalog and participant-delta reads for the live play page.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use dashmap::DashMap;

use super::*;

const PARTICIPANT_ROWS_TTL: Duration = Duration::from_secs(5);
const PARTICIPANT_ROWS_MAX_GAMES: usize = 32;
const PARTICIPANT_ROWS_MAX_BYTES: usize = 512 * 1024;

struct ParticipantRows {
    expires_at: Instant,
    rows: Arc<HashMap<i32, Bytes>>,
}

type ParticipantRowsFlight = Option<Arc<HashMap<i32, Bytes>>>;
static PARTICIPANT_ROWS: std::sync::LazyLock<DashMap<String, ParticipantRows>> =
    std::sync::LazyLock::new(DashMap::new);
static PARTICIPANT_ROWS_SF: std::sync::LazyLock<
    crate::utils::single_flight::SingleFlight<ParticipantRowsFlight>,
> = std::sync::LazyLock::new(crate::utils::single_flight::SingleFlight::new);

fn participant_rows_key(game_id: i32, is_monitor: bool) -> String {
    format!("{game_id}:{}", u8::from(is_monitor))
}

pub(crate) fn invalidate_participant_rows(game_id: i32) {
    PARTICIPANT_ROWS.remove(&participant_rows_key(game_id, false));
    PARTICIPANT_ROWS.remove(&participant_rows_key(game_id, true));
}

fn cached_participant_rows(key: &str, now: Instant) -> Option<Arc<HashMap<i32, Bytes>>> {
    let current = PARTICIPANT_ROWS.get(key)?;
    if current.expires_at > now {
        return Some(current.rows.clone());
    }
    drop(current);
    PARTICIPANT_ROWS.remove(key);
    None
}

fn insert_participant_rows(key: String, rows: Arc<HashMap<i32, Bytes>>, encoded_bytes: usize) {
    if encoded_bytes > PARTICIPANT_ROWS_MAX_BYTES {
        return;
    }
    let now = Instant::now();
    PARTICIPANT_ROWS.retain(|_, entry| entry.expires_at > now);
    if PARTICIPANT_ROWS.len() >= PARTICIPANT_ROWS_MAX_GAMES {
        if let Some(oldest) = PARTICIPANT_ROWS
            .iter()
            .min_by_key(|entry| entry.expires_at)
            .map(|entry| entry.key().clone())
        {
            PARTICIPANT_ROWS.remove(&oldest);
        }
    }
    PARTICIPANT_ROWS.insert(
        key,
        ParticipantRows {
            expires_at: now + PARTICIPANT_ROWS_TTL,
            rows,
        },
    );
}

async fn participant_rows(
    st: &SharedState,
    game: &game::Model,
    is_monitor: bool,
) -> AppResult<Arc<HashMap<i32, Bytes>>> {
    let key = participant_rows_key(game.id, is_monitor);
    if let Some(rows) = cached_participant_rows(&key, Instant::now()) {
        return Ok(rows);
    }
    let st = st.clone();
    let game = game.clone();
    let flight_key = key.clone();
    PARTICIPANT_ROWS_SF
        .run(&key, move || async move {
            if let Some(rows) = cached_participant_rows(&flight_key, Instant::now()) {
                return Some(rows);
            }
            let board = build_scoreboard_cached(&st, &game, is_monitor).await.ok()?;
            let mut encoded_bytes = 0usize;
            let mut rows = HashMap::with_capacity(board.items.len());
            for item in board.items {
                let bytes = Bytes::from(serde_json::to_vec(&item).ok()?);
                // The cache has a hard byte ceiling, but that ceiling must not
                // turn a maximum-roster event into an unavailable endpoint.
                // Oversized projections remain request-local and are simply
                // not retained by `insert_participant_rows` below.
                encoded_bytes = encoded_bytes.saturating_add(bytes.len());
                rows.insert(item.id, bytes);
            }
            let rows = Arc::new(rows);
            insert_participant_rows(flight_key, rows.clone(), encoded_bytes);
            Some(rows)
        })
        .await
        .ok_or_else(|| AppError::internal("participant delta projection is unavailable"))
}

pub async fn game_challenge_catalog(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
    headers: axum::http::HeaderMap,
) -> AppResult<Response> {
    let ctx = context_info(&st, &user, id, false).await?;
    let board = build_scoreboard_cached(&st, &ctx.game, user.is_monitor()).await?;
    let all_challenge_ids: Vec<i32> = board
        .challenges
        .values()
        .flatten()
        .map(|item| item.id)
        .collect();
    let permissions =
        effective_permissions_batch(&st, &ctx.participation, &all_challenge_ids).await?;
    let mut challenges: BTreeMap<String, Vec<ChallengeInfo>> = BTreeMap::new();
    for (category, infos) in board.challenges {
        let visible: Vec<_> = infos
            .into_iter()
            .filter(|info| {
                permissions
                    .get(&info.id)
                    .is_none_or(|permission| permission.contains(GamePermission::VIEW_CHALLENGE))
            })
            .collect();
        if !visible.is_empty() {
            challenges.insert(category, visible);
        }
    }
    let visible_ids = visible_challenge_ids(&challenges);
    let model = GameChallengeCatalogModel {
        challenge_count: i32::try_from(visible_ids.len()).unwrap_or(i32::MAX),
        challenges,
        team_token: ctx.participation.token.clone(),
        writeup_required: ctx.game.writeup_required,
        writeup_deadline: ctx.game.writeup_deadline,
    };
    final_policy::finish_catalog_response(
        st.pg(),
        &user,
        id,
        ctx.participation.team_id,
        ctx.participation.id,
        visible_ids,
        headers
            .get(axum::http::header::IF_NONE_MATCH)
            .and_then(|value| value.to_str().ok()),
        model,
    )
    .await
}

pub async fn game_participant_delta(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
    headers: axum::http::HeaderMap,
) -> AppResult<Response> {
    let ctx = context_info(&st, &user, id, false).await?;
    let rows = participant_rows(&st, &ctx.game, user.is_monitor()).await?;
    let mut rank = rows
        .get(&ctx.participation.team_id)
        .map(|row| serde_json::from_slice::<ScoreboardItem>(row))
        .transpose()
        .map_err(|error| AppError::internal(error.to_string()))?;
    if let Some(rank) = &mut rank {
        let solved_ids: Vec<_> = rank
            .solved_challenges
            .iter()
            .map(|solve| solve.id)
            .collect();
        let permissions = effective_permissions_batch(&st, &ctx.participation, &solved_ids).await?;
        let visible: HashSet<i32> = permissions
            .into_iter()
            .filter_map(|(challenge_id, permission)| {
                permission
                    .contains(GamePermission::VIEW_CHALLENGE)
                    .then_some(challenge_id)
            })
            .collect();
        retain_visible_solves(rank, &visible);
    }
    final_policy::finish_participant_delta_response(
        st.pg(),
        &user,
        id,
        ctx.participation.team_id,
        ctx.participation.id,
        headers
            .get(axum::http::header::IF_NONE_MATCH)
            .and_then(|value| value.to_str().ok()),
        GameParticipantDeltaModel { rank },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn participant_projection_cache_is_explicitly_bounded() {
        assert!(PARTICIPANT_ROWS_MAX_GAMES <= 64);
        assert!(PARTICIPANT_ROWS_MAX_BYTES <= 512 * 1024);
        assert!(PARTICIPANT_ROWS_TTL <= Duration::from_secs(5));
    }
}
