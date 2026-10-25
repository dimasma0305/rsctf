//! Bounded projections of the canonical, freeze-aware scoreboard snapshot.

use std::sync::{Arc, LazyLock};
use std::time::Duration;

use bytes::Bytes;
use moka::future::Cache;

use super::scoreboard_encoding as encoding;
use super::*;

const MAX_PAGE_SIZE: usize = 100;
const MAX_SKIP: usize = 100_000;
const PROJECTION_TTL: Duration = Duration::from_secs(5);

struct Snapshot {
    board: ScoreboardModel,
    version: String,
    // Conservative accounting includes decoded structs, strings and indices.
    weight: u32,
    names: Vec<String>,
}

static SNAPSHOTS: LazyLock<Cache<String, Arc<Snapshot>>> = LazyLock::new(|| {
    Cache::builder()
        .max_capacity(32 * 1024 * 1024)
        .weigher(|_: &String, value: &Arc<Snapshot>| value.weight)
        .time_to_idle(Duration::from_secs(30))
        .build()
});

static PROJECTIONS: LazyLock<crate::utils::single_flight::SingleFlight<Option<Bytes>>> =
    LazyLock::new(crate::utils::single_flight::SingleFlight::new);
static DECODE_GATE: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(4)));

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreboardPageQuery {
    pub skip: Option<usize>,
    pub count: Option<usize>,
    pub division_id: Option<i32>,
    pub search: Option<String>,
    pub locate_team: Option<String>,
}

impl ScoreboardPageQuery {
    fn bounded(mut self) -> AppResult<Self> {
        if self.skip.unwrap_or(0) > MAX_SKIP {
            return Err(AppError::bad_request("Scoreboard offset is too large"));
        }
        if self.search.as_ref().is_some_and(|value| value.len() > 128)
            || self
                .locate_team
                .as_ref()
                .is_some_and(|value| value.len() > 256)
        {
            return Err(AppError::bad_request("Scoreboard search is too long"));
        }
        self.skip = Some(self.skip.unwrap_or(0));
        self.count = Some(self.count.unwrap_or(30).clamp(1, MAX_PAGE_SIZE));
        self.search = self
            .search
            .map(|value| value.trim().to_lowercase())
            .filter(|v| !v.is_empty());
        self.locate_team = self.locate_team.filter(|value| !value.is_empty());
        Ok(self)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Page<'a> {
    #[serde(with = "crate::utils::datetime::millis")]
    update_time_utc: DateTime<Utc>,
    blood_bonus: i64,
    timelines: &'a [Json],
    items: Vec<&'a ScoreboardItem>,
    divisions: &'a [Json],
    challenges: &'a BTreeMap<String, Vec<ChallengeInfo>>,
    challenge_count: i32,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    freeze: Option<DateTime<Utc>>,
    is_frozen_view: bool,
    snapshot_version: &'a str,
    total: usize,
    skip: usize,
    count: usize,
    located: bool,
}

fn project<'a>(snapshot: &'a Snapshot, query: &ScoreboardPageQuery) -> Page<'a> {
    let board = &snapshot.board;
    let rows: Vec<_> = board
        .items
        .iter()
        .zip(&snapshot.names)
        .filter(|(item, name)| {
            query
                .division_id
                .map_or(item.rank > 0, |division| item.division_id == Some(division))
                && query
                    .search
                    .as_ref()
                    .is_none_or(|search| name.contains(search))
        })
        .map(|(item, _)| item)
        .collect();
    let count = query.count.unwrap_or(30);
    let position = query
        .locate_team
        .as_ref()
        .and_then(|name| rows.iter().position(|item| item.name == *name));
    let skip = position
        .map_or(query.skip.unwrap_or(0), |index| index / count * count)
        .min(rows.len().saturating_sub(1) / count * count);
    Page {
        update_time_utc: board.update_time_utc,
        blood_bonus: board.blood_bonus,
        timelines: &board.timelines,
        items: rows.iter().skip(skip).take(count).copied().collect(),
        divisions: &board.divisions,
        challenges: &board.challenges,
        challenge_count: board.challenge_count,
        freeze: board.freeze,
        is_frozen_view: board.is_frozen_view,
        snapshot_version: &snapshot.version,
        total: rows.len(),
        skip,
        count,
        located: position.is_some(),
    }
}

async fn snapshot(
    id: i32,
    monitor: bool,
    version: String,
    bundle: Bytes,
) -> AppResult<Arc<Snapshot>> {
    // Never select a retained snapshot without first consulting the canonical
    // cache: edits, freeze/end transitions and monitor views remain authoritative.
    let key = format!("{id}:{monitor}:{version}");
    SNAPSHOTS
        .try_get_with(key, async move {
            let permit = DECODE_GATE
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| AppError::unavailable("scoreboard decode capacity unavailable"))?;
            let raw = encoding::identity_body(bundle)?;
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let board: ScoreboardModel = serde_json::from_slice(&raw)
                    .map_err(|error| AppError::internal(error.to_string()))?;
                let names = board
                    .items
                    .iter()
                    .map(|item| item.name.to_lowercase())
                    .collect();
                Ok::<_, AppError>(Arc::new(Snapshot {
                    board,
                    version,
                    names,
                    weight: u32::try_from(raw.len().saturating_mul(4).saturating_add(1024))
                        .unwrap_or(u32::MAX),
                }))
            })
            .await
            .map_err(|error| AppError::internal(error.to_string()))?
        })
        .await
        .map_err(|_| AppError::internal("scoreboard snapshot decode failed"))
}

/// Shares the legacy direct-link visibility contract: hidden games are unlisted,
/// public standings contain accepted participants only, and pre-start reads fail.
pub async fn scoreboard_page(
    State(st): State<SharedState>,
    MaybeUser(user): MaybeUser,
    Path(id): Path<i32>,
    Query(query): Query<ScoreboardPageQuery>,
    headers: axum::http::HeaderMap,
) -> AppResult<Response> {
    let query = query.bounded()?;
    let game = load_game_cached(&st, id).await?;
    if Utc::now() < game.start_time_utc {
        return Err(AppError::game_not_started());
    }
    let monitor = user.as_ref().is_some_and(|user| user.is_monitor());
    super::scoreboard_refresh::observe(&game, monitor);
    let canonical = build_scoreboard_bundle(&st, &game, monitor).await?;
    let version = encoding::semantic_version(&canonical)?;
    let query_hash = crate::utils::codec::sha256_hex(
        &serde_json::to_vec(&query).map_err(|error| AppError::internal(error.to_string()))?,
    );
    let key = format!("_ScoreboardPageV1_{id}:{monitor}:{version}:{query_hash}");
    let scope = if monitor {
        "standard-monitor-page"
    } else {
        "standard-public-page"
    };
    if let Some(bundle) = st.cache.get(&key).await {
        return encoding::scoped_response(bundle, &headers, scope);
    }
    let (cache, fill_key) = (st.cache.clone(), key.clone());
    let bundle = PROJECTIONS
        .run_with_limit(&key, Duration::from_secs(15), 64, move || async move {
            if let Some(bundle) = cache.get(&fill_key).await {
                return Some(bundle);
            }
            let snapshot = snapshot(id, monitor, version, canonical).await.ok()?;
            let raw = tokio::task::spawn_blocking(move || {
                serde_json::to_vec(&project(&snapshot, &query))
            })
            .await
            .ok()?
            .ok()?;
            let built = encoding::build_stable_bundle(
                Bytes::from(raw),
                fill_key.clone(),
                b"\"updateTimeUtc\":",
            )
            .await
            .ok()?;
            if built.cacheable {
                cache
                    .set(&fill_key, &built.bytes, Some(PROJECTION_TTL))
                    .await;
            }
            Some(built.bytes)
        })
        .await
        .ok_or_else(|| AppError::unavailable("scoreboard page capacity exhausted"))?;
    encoding::scoped_response(bundle, &headers, scope)
}

#[derive(Serialize)]
struct CtftimeStanding<'a> {
    pos: i32,
    team: &'a str,
    score: i64,
}

#[derive(Serialize)]
struct CtftimeStandings<'a> {
    standings: Vec<CtftimeStanding<'a>>,
}

/// CTFtime expects an anonymous public `{standings:[{pos,team,score}]}` feed.
/// Always use the public frozen view, even for an authenticated organizer.
pub async fn ctftime_scoreboard(
    State(st): State<SharedState>,
    Path(id): Path<i32>,
    headers: axum::http::HeaderMap,
) -> AppResult<Response> {
    let game = load_game_cached(&st, id).await?;
    if game.hidden {
        return Err(AppError::not_found("Game not found"));
    }
    if Utc::now() < game.start_time_utc {
        return Err(AppError::game_not_started());
    }
    super::scoreboard_refresh::observe(&game, false);
    let canonical = build_scoreboard_bundle(&st, &game, false).await?;
    let version = encoding::semantic_version(&canonical)?;
    let key = format!("_CtftimeJeopardyV1_{id}:{version}");
    if let Some(bundle) = st.cache.get(&key).await {
        return encoding::scoped_response(bundle, &headers, "ctftime-public");
    }
    let snapshot = snapshot(id, false, version, canonical).await?;
    if snapshot
        .board
        .challenges
        .values()
        .flatten()
        .any(|challenge| {
            matches!(
                challenge.challenge_type,
                ChallengeType::AttackDefense | ChallengeType::KingOfTheHill
            )
        })
    {
        return Err(AppError::bad_request(
            "CTFtime export supports Jeopardy-only events",
        ));
    }
    let (cache, fill_key) = (st.cache.clone(), key.clone());
    let bundle = PROJECTIONS
        .run_with_limit(&key, Duration::from_secs(15), 64, move || async move {
            if let Some(bundle) = cache.get(&fill_key).await {
                return Some(bundle);
            }
            let raw = tokio::task::spawn_blocking(move || {
                let standings = snapshot
                    .board
                    .items
                    .iter()
                    .filter(|item| item.rank > 0)
                    .map(|item| CtftimeStanding {
                        pos: item.rank,
                        team: &item.name,
                        score: item.score,
                    })
                    .collect();
                serde_json::to_vec(&CtftimeStandings { standings })
            })
            .await
            .ok()?
            .ok()?;
            let built = encoding::build_versioned_bundle(Bytes::from(raw), fill_key.clone())
                .await
                .ok()?;
            if built.cacheable {
                cache
                    .set(&fill_key, &built.bytes, Some(PROJECTION_TTL))
                    .await;
            }
            Some(built.bytes)
        })
        .await
        .ok_or_else(|| AppError::unavailable("scoreboard export capacity exhausted"))?;
    encoding::scoped_response(bundle, &headers, "ctftime-public")
}

#[cfg(test)]
#[path = "scoreboard_page_tests.rs"]
mod tests;
