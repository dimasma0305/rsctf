//! Player-facing A&D scoreboard + live team state + self-service reset.

use axum::response::Response;

use super::*;

/// Self-service reset cooldown fallback (seconds), used only when the game row
/// leaves `ad_reset_cooldown_minutes` null. The live value is
/// `game.ad_reset_cooldown_minutes * 60`, computed per game in `state` (to
/// report the remaining cooldown) and `reset_service` (to enforce it).
const RESET_COOLDOWN_SECS_DEFAULT: i64 = 300;
const AD_SCOREBOARD_FRESH_TTL: std::time::Duration = std::time::Duration::from_secs(5);
const AD_SCOREBOARD_STALE_TTL: std::time::Duration = std::time::Duration::from_secs(30);
const AD_SCOREBOARD_REFRESH_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(1);
const AD_SCOREBOARD_REFRESH_SHARDS: usize = 256;
const _: () = assert!(AD_SCOREBOARD_REFRESH_SHARDS.is_power_of_two());
static AD_SCOREBOARD_SF: std::sync::LazyLock<
    crate::utils::single_flight::SingleFlight<ScoreboardFillResult>,
> = std::sync::LazyLock::new(crate::utils::single_flight::SingleFlight::new);
static AD_SCOREBOARD_REFRESHES: [std::sync::atomic::AtomicBool; AD_SCOREBOARD_REFRESH_SHARDS] =
    [const { std::sync::atomic::AtomicBool::new(false) }; AD_SCOREBOARD_REFRESH_SHARDS];
static AD_STATE_CTX_SF: std::sync::LazyLock<
    crate::utils::single_flight::SingleFlight<StateCtxFillResult>,
> = std::sync::LazyLock::new(crate::utils::single_flight::SingleFlight::new);

#[derive(Clone, Debug, Default)]
enum ScoreboardFillResult {
    Ready(bytes::Bytes),
    NotFound(String),
    #[default]
    Failed,
}

enum ScoreboardBuildAttempt {
    Complete(ScoreboardFillResult),
    RevisionChanged,
}

#[derive(Clone, Debug, Default)]
enum StateCtxFillResult {
    Ready(AdStateCtx),
    NotFound(String),
    #[default]
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RevisionDisposition {
    Current,
    Changed,
    Missing,
}

/// `AdTeamServiceStateModel` — one service row in the player's state view.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdTeamServiceStateModel {
    pub ad_team_service_id: i32,
    pub challenge_id: i32,
    pub challenge_title: String,
    pub container_ip: Option<String>,
    pub container_port: Option<i32>,
    pub current_flag: Option<String>,
    pub last_check_status: Option<String>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub last_reset_at: Option<DateTime<Utc>>,
    pub can_reset: bool,
    pub reset_cooldown_seconds_remaining: Option<i64>,
    pub snapshot_available: bool,
    pub self_hosted: Option<bool>,
}

/// `AdStateModel` — GET `Ad/State` response.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdStateModel {
    pub current_round: i32,
    /// Number of scoring rounds in one official A&D epoch.
    pub epoch_ticks: i32,
    /// First round included in official A&D scoring. `None` during warmup.
    pub start_round: Option<i32>,
    /// False until the durable current-round flag-publication phase settles.
    /// Clients should wait instead of attacking with stale prior-round flags.
    pub flags_ready: bool,
    /// Number of participant services that did not acknowledge the current
    /// round's flag after the bounded retry policy. Zero means publication
    /// completed for the full field.
    pub flag_delivery_failures: i32,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub round_started_at: Option<DateTime<Utc>>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub round_ends_at: Option<DateTime<Utc>>,
    pub scoring_paused: bool,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub scoring_paused_at: Option<DateTime<Utc>>,
    pub services: Vec<AdTeamServiceStateModel>,
}

struct ScoreboardRefreshReservation {
    shard: usize,
}

impl Drop for ScoreboardRefreshReservation {
    fn drop(&mut self) {
        AD_SCOREBOARD_REFRESHES[self.shard].store(false, std::sync::atomic::Ordering::Release);
    }
}

fn scoreboard_refresh_shard(key: &str) -> usize {
    // A stable FNV-1a hash keeps the reservation independent of the process's
    // randomized HashMap seed. The power-of-two shard count makes selection a
    // cheap mask; collisions only defer a refresh until a later stale poll.
    let hash = key.bytes().fold(0xcbf29ce484222325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    });
    hash as usize & (AD_SCOREBOARD_REFRESH_SHARDS - 1)
}

fn reserve_scoreboard_refresh(key: &str) -> Option<ScoreboardRefreshReservation> {
    let shard = scoreboard_refresh_shard(key);
    if AD_SCOREBOARD_REFRESHES[shard]
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        )
        .is_err()
    {
        return None;
    }
    Some(ScoreboardRefreshReservation { shard })
}

fn stale_scoreboard_key(current_key: &str) -> String {
    format!("{current_key}:stale")
}

fn scoreboard_cache_key(game_id: i32, is_monitor: bool) -> String {
    if is_monitor {
        format!("_AdScoreBoard_{game_id}")
    } else {
        format!("_AdScoreBoardFrozen_{game_id}")
    }
}

fn live_operational_value<T>(archived: bool, value: T) -> Option<T> {
    (!archived).then_some(value)
}

/// Remove every A&D board representation after a destructive, visibility, or
/// configuration mutation. Routine round/submit invalidations intentionally
/// remove only the five-second fresh key so SWR can bridge an expensive rebuild.
pub(crate) async fn hard_invalidate_ad_scoreboard(st: &SharedState, game_id: i32) {
    // A board fill fences publication with Games.xmin. Most scoring inputs live
    // in child tables, so advance that revision before eviction: an older
    // detached fill can no longer republish stale data after this returns.
    if let Err(error) = sqlx::query(r#"UPDATE "Games" SET id = id WHERE id = $1"#)
        .bind(game_id)
        .execute(st.pg())
        .await
    {
        tracing::warn!(game = game_id, %error, "A&D scoreboard revision barrier failed");
    }
    hard_invalidate_ad_scoreboard_cache(st.cache.as_ref(), game_id).await;
    crate::controllers::game::invalidate_combined_scoreboard(st, game_id).await;
}

async fn hard_invalidate_ad_scoreboard_cache(
    cache: &dyn crate::services::cache::Cache,
    game_id: i32,
) {
    let live = scoreboard_cache_key(game_id, true);
    let frozen = scoreboard_cache_key(game_id, false);
    let live_stale = stale_scoreboard_key(&live);
    let frozen_stale = stale_scoreboard_key(&frozen);
    tokio::join!(
        cache.remove(&live),
        cache.remove(&live_stale),
        cache.remove(&frozen),
        cache.remove(&frozen_stale),
    );
}

fn revision_disposition(
    expected: &crate::services::ad::scoring::AdScoreboardRevision,
    observed: Option<&crate::services::ad::scoring::AdScoreboardRevision>,
) -> RevisionDisposition {
    match observed {
        None => RevisionDisposition::Missing,
        Some(observed) if observed == expected => RevisionDisposition::Current,
        Some(_) => RevisionDisposition::Changed,
    }
}

fn completed_scoreboard_bundle(result: ScoreboardFillResult) -> AppResult<bytes::Bytes> {
    match result {
        ScoreboardFillResult::Ready(bytes) => Ok(bytes),
        ScoreboardFillResult::NotFound(message) => Err(AppError::not_found(message)),
        ScoreboardFillResult::Failed => Err(AppError::internal("A&D scoreboard cache fill failed")),
    }
}

async fn cached_scoreboard_bundle(
    cache: &dyn crate::services::cache::Cache,
    key: &str,
) -> Option<bytes::Bytes> {
    let bytes = cache.get(key).await?;
    if super::scoreboard_encoding::valid_bundle(&bytes) {
        return Some(bytes);
    }
    tracing::warn!(
        cache_key = key,
        "evicting corrupt A&D scoreboard cache entry"
    );
    cache.remove(key).await;
    None
}

async fn build_scoreboard_bundle_attempt(
    st: &SharedState,
    id: i32,
    is_monitor: bool,
    current_key: &str,
    stale_key: &str,
) -> ScoreboardBuildAttempt {
    let before =
        match crate::services::ad::scoring::ad_scoreboard_revision(st.pg(), id, is_monitor).await {
            Ok(Some(revision)) => revision,
            Ok(None) => {
                hard_invalidate_ad_scoreboard_cache(st.cache.as_ref(), id).await;
                return ScoreboardBuildAttempt::Complete(ScoreboardFillResult::NotFound(
                    "Game not found".to_owned(),
                ));
            }
            Err(error) => {
                tracing::warn!(game = id, %error, "A&D scoreboard revision preflight failed");
                return ScoreboardBuildAttempt::Complete(ScoreboardFillResult::Failed);
            }
        };
    let model = match crate::services::ad::scoring::build_ad_scoreboard(
        st.pg(),
        id,
        is_monitor,
        Utc::now(),
    )
    .await
    {
        Ok(model) => model,
        Err(AppError::NotFound(message)) => {
            hard_invalidate_ad_scoreboard_cache(st.cache.as_ref(), id).await;
            return ScoreboardBuildAttempt::Complete(ScoreboardFillResult::NotFound(message));
        }
        Err(error) => {
            tracing::warn!(game = id, %error, "A&D scoreboard cache fill failed");
            return ScoreboardBuildAttempt::Complete(ScoreboardFillResult::Failed);
        }
    };
    let raw = match serde_json::to_vec(&model) {
        Ok(raw) => bytes::Bytes::from(raw),
        Err(error) => {
            tracing::warn!(game = id, %error, "A&D scoreboard serialization failed");
            return ScoreboardBuildAttempt::Complete(ScoreboardFillResult::Failed);
        }
    };
    let built = match super::scoreboard_encoding::build_bundle(raw).await {
        Ok(built) => built,
        Err(error) => {
            tracing::warn!(game = id, %error, "A&D scoreboard encoding failed");
            return ScoreboardBuildAttempt::Complete(ScoreboardFillResult::Failed);
        }
    };

    let after_build =
        match crate::services::ad::scoring::ad_scoreboard_revision(st.pg(), id, is_monitor).await {
            Ok(revision) => revision,
            Err(error) => {
                tracing::warn!(game = id, %error, "A&D scoreboard revision validation failed");
                return ScoreboardBuildAttempt::Complete(ScoreboardFillResult::Failed);
            }
        };
    match revision_disposition(&before, after_build.as_ref()) {
        RevisionDisposition::Current => {}
        RevisionDisposition::Changed => {
            hard_invalidate_ad_scoreboard_cache(st.cache.as_ref(), id).await;
            return ScoreboardBuildAttempt::RevisionChanged;
        }
        RevisionDisposition::Missing => {
            hard_invalidate_ad_scoreboard_cache(st.cache.as_ref(), id).await;
            return ScoreboardBuildAttempt::Complete(ScoreboardFillResult::NotFound(
                "Game not found".to_owned(),
            ));
        }
    }

    if built.cacheable {
        let fresh_ttl = super::scoreboard_encoding::final_or_live_cache_ttl(
            before.immutable_final,
            AD_SCOREBOARD_FRESH_TTL,
        );
        let stale_ttl = super::scoreboard_encoding::final_or_live_cache_ttl(
            before.immutable_final,
            AD_SCOREBOARD_STALE_TTL,
        );
        st.cache
            .set(current_key, &built.bytes, Some(fresh_ttl))
            .await;
        st.cache.set(stale_key, &built.bytes, Some(stale_ttl)).await;

        // Close the post-check/publication race: if a mutation committed and
        // hard-invalidated between validation and either SET, discard both
        // representations. If it commits after this query, its post-commit hard
        // invalidation owns the ordering and removes what was just published.
        let after_publish =
            match crate::services::ad::scoring::ad_scoreboard_revision(st.pg(), id, is_monitor)
                .await
            {
                Ok(revision) => revision,
                Err(error) => {
                    tracing::warn!(game = id, %error, "A&D scoreboard publication fence failed");
                    hard_invalidate_ad_scoreboard_cache(st.cache.as_ref(), id).await;
                    return ScoreboardBuildAttempt::Complete(ScoreboardFillResult::Failed);
                }
            };
        match revision_disposition(&before, after_publish.as_ref()) {
            RevisionDisposition::Current => {}
            RevisionDisposition::Changed => {
                hard_invalidate_ad_scoreboard_cache(st.cache.as_ref(), id).await;
                return ScoreboardBuildAttempt::RevisionChanged;
            }
            RevisionDisposition::Missing => {
                hard_invalidate_ad_scoreboard_cache(st.cache.as_ref(), id).await;
                return ScoreboardBuildAttempt::Complete(ScoreboardFillResult::NotFound(
                    "Game not found".to_owned(),
                ));
            }
        }
    }
    ScoreboardBuildAttempt::Complete(ScoreboardFillResult::Ready(built.bytes))
}

async fn fill_scoreboard_bundle(
    st: SharedState,
    id: i32,
    is_monitor: bool,
    current_key: String,
    stale_key: String,
) -> ScoreboardFillResult {
    let flight_key = current_key.clone();
    AD_SCOREBOARD_SF
        .run(&flight_key, move || async move {
            if let Some(bytes) = cached_scoreboard_bundle(st.cache.as_ref(), &current_key).await {
                return ScoreboardFillResult::Ready(bytes);
            }
            for attempt in 0..2 {
                match build_scoreboard_bundle_attempt(&st, id, is_monitor, &current_key, &stale_key)
                    .await
                {
                    ScoreboardBuildAttempt::Complete(result) => return result,
                    ScoreboardBuildAttempt::RevisionChanged if attempt == 0 => continue,
                    ScoreboardBuildAttempt::RevisionChanged => {
                        tracing::warn!(
                            game = id,
                            "A&D scoreboard revision changed during both fill attempts"
                        );
                        return ScoreboardFillResult::Failed;
                    }
                }
            }
            ScoreboardFillResult::Failed
        })
        .await
}

fn refresh_scoreboard_detached(
    st: SharedState,
    id: i32,
    is_monitor: bool,
    current_key: String,
    stale_key: String,
) {
    let Some(reservation) = reserve_scoreboard_refresh(&current_key) else {
        return;
    };
    tokio::spawn(async move {
        let refreshed = fill_scoreboard_bundle(st, id, is_monitor, current_key, stale_key).await;
        // Keep a failed build coalesced briefly. Without this bounded delay, a
        // fast database error could turn every stale-serving request into the
        // leader of a new sequential retry even though requests never dogpile.
        if !matches!(refreshed, ScoreboardFillResult::Ready(_)) {
            tokio::time::sleep(AD_SCOREBOARD_REFRESH_RETRY_DELAY).await;
        }
        drop(reservation);
    });
}

/// `GET /api/Game/{id}/Ad/Scoreboard` — the sole official A&D standings.
///
/// The builder reads config, timing metadata, roster, and evidence under one
/// repeatable-read snapshot. One atomic cache entry holds the raw, gzip, and
/// Brotli bodies; hits select a zero-copy `Bytes` slice without recompression.
/// After a fresh entry expires, a bounded stale copy keeps synchronized pollers
/// responsive while one detached single-flight rebuild refreshes both entries.
/// A true cold start still waits for that rebuild and never fabricates a board.
pub async fn scoreboard(
    State(st): State<SharedState>,
    MaybeUser(maybe): MaybeUser,
    Path(id): Path<i32>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let is_monitor = maybe.as_ref().is_some_and(|user| user.is_monitor());
    let game = crate::controllers::game::load_game_cached(&st, id).await?;
    if game.hidden && !is_monitor {
        return Err(AppError::not_found("Game not found"));
    }
    if Utc::now() < game.start_time_utc && !is_monitor {
        return Err(AppError::game_not_started());
    }
    let cache_key = scoreboard_cache_key(id, is_monitor);
    if let Some(bytes) = cached_scoreboard_bundle(st.cache.as_ref(), &cache_key).await {
        return super::scoreboard_encoding::response(bytes, &headers);
    }
    let stale_key = stale_scoreboard_key(&cache_key);
    if let Some(bytes) = cached_scoreboard_bundle(st.cache.as_ref(), &stale_key).await {
        refresh_scoreboard_detached(st, id, is_monitor, cache_key, stale_key);
        return super::scoreboard_encoding::response(bytes, &headers);
    }

    // The detached leader already logged the precise failure. The cloneable
    // result preserves a genuine 404 for every waiter instead of collapsing all
    // fill failures into a generic 500.
    let bytes = completed_scoreboard_bundle(
        fill_scoreboard_bundle(st, id, is_monitor, cache_key, stale_key).await,
    )?;
    super::scoreboard_encoding::response(bytes, &headers)
}

/// Cached A&D board as a model for internal projections such as the combined
/// multi-format standings. This follows the exact same fresh/stale/SWR path as
/// the public handler and extracts the identity body without a copy.
pub(crate) async fn build_ad_scoreboard_cached(
    st: &SharedState,
    id: i32,
    is_monitor: bool,
) -> AppResult<crate::services::ad::scoring::AdScoreboard> {
    let cache_key = scoreboard_cache_key(id, is_monitor);
    let bundle = if let Some(bytes) = cached_scoreboard_bundle(st.cache.as_ref(), &cache_key).await
    {
        bytes
    } else {
        let stale_key = stale_scoreboard_key(&cache_key);
        if let Some(bytes) = cached_scoreboard_bundle(st.cache.as_ref(), &stale_key).await {
            refresh_scoreboard_detached(st.clone(), id, is_monitor, cache_key, stale_key);
            bytes
        } else {
            completed_scoreboard_bundle(
                fill_scoreboard_bundle(st.clone(), id, is_monitor, cache_key, stale_key).await,
            )?
        }
    };
    let raw = super::scoreboard_encoding::identity_body(bundle)?;
    serde_json::from_slice(&raw).map_err(|error| AppError::internal(error.to_string()))
}

/// Game-global half of `Ad/State` — config + the challenge title/policy map. Shared by
/// every team and near-static, so it's cached (5 s); the per-team half (services, checks,
/// live flags) and the current round are read fresh so a just-planted flag is never stale
/// (the round is what the flag query keys on — the one field caching couldn't front).
#[derive(Clone, Debug, Serialize, Deserialize)]
struct AdStateCtx {
    reset_cooldown_secs: i64,
    allow_snapshot: bool,
    epoch_ticks: i32,
    start_round: Option<i32>,
    /// challenge_id -> (title, ad_self_hosted, ad_allow_self_reset)
    challenges: HashMap<i32, (String, bool, bool)>,
}

/// Cache the global State context (game config + all-challenge title/policy) for 5 s
/// behind single-flight, so a poll storm resolves it once instead of ~3 DB reads/team/poll.
async fn state_ctx_cached(st: &SharedState, id: i32) -> AppResult<AdStateCtx> {
    let key = format!("adstatectx:{id}");
    if let Some(b) = st.cache.get(&key).await {
        if let Ok(ctx) = serde_json::from_slice::<AdStateCtx>(&b) {
            return Ok(ctx);
        }
    }
    let st = st.clone();
    let key_for_fill = key.clone();
    let result = AD_STATE_CTX_SF
        .run(&key, move || async move {
            if let Some(bytes) = st.cache.get(&key_for_fill).await {
                if let Ok(ctx) = serde_json::from_slice::<AdStateCtx>(&bytes) {
                    return StateCtxFillResult::Ready(ctx);
                }
            }
            let ctx = match build_state_ctx(&st, id).await {
                Ok(ctx) => ctx,
                Err(AppError::NotFound(message)) => {
                    return StateCtxFillResult::NotFound(message);
                }
                Err(error) => {
                    tracing::warn!(game = id, %error, "A&D state context cache fill failed");
                    return StateCtxFillResult::Failed;
                }
            };
            let json = match serde_json::to_vec(&ctx) {
                Ok(json) => json,
                Err(error) => {
                    tracing::warn!(game = id, %error, "A&D state context serialization failed");
                    return StateCtxFillResult::Failed;
                }
            };
            st.cache
                .set(
                    &key_for_fill,
                    &json,
                    Some(std::time::Duration::from_secs(5)),
                )
                .await;
            StateCtxFillResult::Ready(ctx)
        })
        .await;
    match result {
        StateCtxFillResult::Ready(ctx) => Ok(ctx),
        StateCtxFillResult::NotFound(message) => Err(AppError::not_found(message)),
        StateCtxFillResult::Failed => {
            Err(AppError::internal("A&D state context cache fill failed"))
        }
    }
}

async fn build_state_ctx(st: &SharedState, id: i32) -> AppResult<AdStateCtx> {
    let (reset_minutes, allow_snapshot, epoch_ticks, start_round) =
        sqlx::query_as::<_, (Option<i32>, bool, i32, Option<i32>)>(
            r#"SELECT ad_reset_cooldown_minutes, ad_allow_snapshot_download,
                      ad_epoch_ticks, ad_scoring_start_round
             FROM "Games" WHERE id = $1"#,
        )
        .bind(id)
        .fetch_optional(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .ok_or_else(|| AppError::not_found("Game not found"))?;
    let challenge_rows = sqlx::query_as::<_, (i32, String, bool, bool)>(
        r#"SELECT id, title, ad_self_hosted, ad_allow_self_reset
             FROM "GameChallenges" WHERE game_id = $1"#,
    )
    .bind(id)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(AdStateCtx {
        reset_cooldown_secs: reset_minutes
            .map(|minutes| minutes as i64 * 60)
            .unwrap_or(RESET_COOLDOWN_SECS_DEFAULT),
        allow_snapshot,
        epoch_ticks: epoch_ticks.clamp(1, 64),
        start_round: start_round.map(|round| round.max(1)),
        challenges: challenge_rows
            .into_iter()
            .map(|(id, title, self_hosted, allow_reset)| (id, (title, self_hosted, allow_reset)))
            .collect(),
    })
}

/// `GET /api/Game/{id}/Ad/State` — the caller team's live round + service view.
pub async fn state(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<AdStateModel>> {
    let part = resolve_participation(&st, &user, id).await?;

    // Config + the challenge title/policy map are game-global and near-static → cached.
    // The round and this team's services/checks/flags are read fresh below.
    let ctx = state_ctx_cached(&st, id).await?;
    let reset_cooldown_secs = ctx.reset_cooldown_secs;
    let mut roster = crate::services::live_roster::try_acquire_participation_fence(
        st.pg(),
        user.id,
        &user.security_stamp,
        id,
        part.team_id,
        part.id,
        true,
    )
    .await?
    .ok_or(AppError::Forbidden)?;

    let now = Utc::now();
    // One statement keeps the fresh round/services/checks/flags tail on one
    // MVCC snapshot, applies the event window with PostgreSQL's clock, and
    // avoids four sequential pool checkouts/round trips.
    let super::state_tail::AdStateTail {
        event_started,
        event_ended,
        current_round,
        round_started_at,
        round_ends_at,
        flags_ready,
        flag_delivery_failures,
        scoring_paused,
        scoring_paused_at,
        services,
    } = super::state_tail::load(&mut **roster.transaction_mut(), id, part.id).await?;
    if !event_started {
        roster.release().await?;
        return Err(AppError::game_not_started());
    }
    let archived = event_ended;
    // `snapshot_available` per service must be the exact success condition of
    // the Snapshot route: event ended, enabled, and a platform container.
    let snapshots_downloadable = event_ended && ctx.allow_snapshot;

    let items = services
        .into_iter()
        .map(|s| {
            // RSCTF `AdGameController` State: `LastCheckStatus` is sourced purely
            // from AdCheckResults (`?.Status.ToString()`) — it stays null until a
            // real checker verdict exists, never fabricated from `s.status`.
            let last_check_status = s.last_check_status.map(status_str);
            let (challenge_title, self_hosted, allow_self_reset) = ctx
                .challenges
                .get(&s.challenge_id)
                .cloned()
                .unwrap_or_default();
            // Downloadable exactly when the route would serve it (see above).
            let snapshot_available = snapshots_downloadable && s.snapshot_available && !self_hosted;
            // Remaining cooldown from the last self-reset (0 if never reset or the
            // window has elapsed); the button only lights when it's fully elapsed.
            let cooldown_remaining = s
                .last_reset_at
                .map(|last| (reset_cooldown_secs - (now - last).num_seconds()).max(0))
                .unwrap_or(0);
            AdTeamServiceStateModel {
                ad_team_service_id: s.id,
                challenge_id: s.challenge_id,
                challenge_title,
                container_ip: live_operational_value(archived, s.host),
                container_port: live_operational_value(archived, s.port),
                current_flag: live_operational_value(archived, s.current_flag).flatten(),
                last_check_status,
                last_reset_at: s.last_reset_at,
                // Self-hosted (BYOC): nothing on our side to relaunch, so never offer
                // the reset button (RSCTF State reduction 1388: `&& !AdSelfHosted`).
                can_reset: !archived && allow_self_reset && cooldown_remaining == 0 && !self_hosted,
                reset_cooldown_seconds_remaining: (!archived && cooldown_remaining > 0)
                    .then_some(cooldown_remaining),
                snapshot_available,
                self_hosted: Some(self_hosted),
            }
        })
        .collect();

    let response = RequestResponse::ok(AdStateModel {
        current_round,
        epoch_ticks: ctx.epoch_ticks,
        start_round: ctx.start_round,
        flags_ready,
        flag_delivery_failures,
        round_started_at,
        round_ends_at,
        scoring_paused,
        scoring_paused_at,
        services: items,
    });
    roster.release().await?;
    Ok(response)
}

/// `POST /api/Game/{id}/Ad/Services/{adTeamServiceId}/Reset` — the caller
/// restarts their own service container: destroy it, launch a fresh one with a
/// newly-planted flag, and stamp the self-reset cooldown. Requires the challenge
/// to allow self-reset and the cooldown to have elapsed.

pub async fn reset_service(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, ad_team_service_id)): Path<(i32, i32)>,
    headers: HeaderMap,
) -> AppResult<(
    StatusCode,
    RequestResponse<crate::services::control_jobs::ControlJobModel>,
)> {
    let part = resolve_participation(&st, &user, id).await?;
    let service = ad_team_service::Entity::find_by_id(ad_team_service_id)
        .one(&st.db)
        .await?
        .filter(|service| service.game_id == id && service.participation_id == part.id)
        .ok_or(AppError::Forbidden)?;
    let operation_id = crate::controllers::edit::control_jobs::operation_id(&headers)?;
    let input = serde_json::json!({
        "serviceId": service.id,
        "participationId": service.participation_id,
        "expectedBackendId": service.container_id,
        "playerPolicy": true,
    });
    let fingerprint = crate::controllers::edit::control_jobs::fingerprint(&input)?;
    let job = crate::services::control_jobs::enqueue(
        st.pg(),
        crate::services::control_jobs::ControlJobKind::AdReset,
        &format!("ad-service:{}", service.id),
        id,
        Some(service.challenge_id),
        operation_id,
        &fingerprint,
        input,
    )
    .await?;
    crate::services::control_jobs::kick(st);
    Ok((StatusCode::ACCEPTED, RequestResponse::ok(job)))
}

pub async fn reset_job_status(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, job_id)): Path<(i32, uuid::Uuid)>,
) -> AppResult<RequestResponse<crate::services::control_jobs::ControlJobModel>> {
    let part = resolve_participation(&st, &user, id).await?;
    let job = crate::services::control_jobs::get_ad_reset_for_participation(
        st.pg(),
        id,
        part.id,
        Some(job_id),
        None,
    )
    .await?
    .ok_or_else(|| AppError::not_found("Reset job not found"))?;
    Ok(RequestResponse::ok(job))
}

pub async fn cancel_reset_job(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, job_id)): Path<(i32, uuid::Uuid)>,
) -> AppResult<RequestResponse<crate::services::control_jobs::ControlJobModel>> {
    let part = resolve_participation(&st, &user, id).await?;
    crate::services::control_jobs::get_ad_reset_for_participation(
        st.pg(),
        id,
        part.id,
        Some(job_id),
        None,
    )
    .await?
    .ok_or_else(|| AppError::not_found("Reset job not found"))?;
    let job = crate::services::control_jobs::request_cancellation(st.pg(), job_id).await?;
    Ok(RequestResponse::ok(job))
}

pub async fn reset_job_by_operation(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, operation_id)): Path<(i32, uuid::Uuid)>,
) -> AppResult<RequestResponse<crate::services::control_jobs::ControlJobModel>> {
    let part = resolve_participation(&st, &user, id).await?;
    let job = crate::services::control_jobs::get_ad_reset_for_participation(
        st.pg(),
        id,
        part.id,
        None,
        Some(operation_id),
    )
    .await?
    .ok_or_else(|| AppError::not_found("Reset job not found"))?;
    Ok(RequestResponse::ok(job))
}

/// `GET /api/Game/{id}/Ad/Services/{adTeamServiceId}/Snapshot` — download the
/// compressed post-game container snapshot for one of the caller's OWN team's
/// services. Ported from RSCTF `AdGameController.DownloadSnapshot`.
///
/// The end-of-event lifecycle captures an immutable Docker filesystem export in
/// blob storage before destroying the service backend. The gate is identical to
/// the `snapshotAvailable` flag the player `state` reports, so the client's
/// download button never lies.
pub async fn download_snapshot(
    State(st): State<SharedState>,
    user: CurrentUser,
    headers: axum::http::HeaderMap,
    Path((id, ad_team_service_id)): Path<(i32, i32)>,
) -> AppResult<Response> {
    let part = resolve_participation(&st, &user, id).await?;
    let svc = ad_team_service::Entity::find_by_id(ad_team_service_id)
        .one(&st.db)
        .await?
        .filter(|s| s.game_id == id)
        .ok_or_else(|| AppError::not_found("Service not found"))?;
    // Team-scoped (unlike the admin forensics endpoint): only the owning team.
    if svc.participation_id != part.id {
        return Err(AppError::Forbidden);
    }

    let game = game::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Game not found"))?;
    // Honor the LIVE download policy + post-game gate (mirrors RSCTF): an operator
    // may revoke download after capture, and a snapshot must never leak mid-game.
    if !game.ad_allow_snapshot_download {
        return Err(AppError::not_found(
            "Snapshot download is disabled for this game",
        ));
    }
    if Utc::now() < game.end_time_utc {
        return Err(AppError::not_found(
            "Snapshot is only available after the game ends",
        ));
    }

    // Self-hosted (BYOC): the container is the tunnel relay, not the team's box —
    // exporting it would leak relay internals. Refuse, as RSCTF does.
    let challenge = game_challenge::Entity::find_by_id(svc.challenge_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Challenge not found"))?;
    if challenge.ad_self_hosted {
        return Err(AppError::not_found(
            "Self-hosted (BYOC) service has no platform snapshot",
        ));
    }

    let Some(snapshot) = crate::services::blob_refs::load_service_snapshot(st.pg(), svc.id).await?
    else {
        return Err(AppError::not_found(
            "Snapshot not available for this service",
        ));
    };
    let grant = super::snapshot_download::SnapshotResponseGrant {
        team_service_id: svc.id,
        snapshot_id: snapshot.id,
        hash: snapshot.hash,
        filename: snapshot.name,
        file_size: snapshot.file_size,
    };
    let prepared = match super::snapshot_download::prepare_snapshot_stream(&st, &headers, &grant)
        .await?
    {
        super::snapshot_download::SnapshotPreparation::Ready(prepared) => prepared,
        super::snapshot_download::SnapshotPreparation::Response(response) => return Ok(response),
    };
    super::snapshot_download::finish_snapshot_response(
        st.pg(),
        crate::services::live_roster::LiveParticipationIdentity {
            user_id: user.id,
            expected_security_stamp: &user.security_stamp,
            game_id: id,
            team_id: part.team_id,
            participation_id: part.id,
        },
        grant,
        prepared,
    )
    .await
}

#[cfg(test)]
#[path = "scoreboard_tests.rs"]
mod scoreboard_cache_tests;

