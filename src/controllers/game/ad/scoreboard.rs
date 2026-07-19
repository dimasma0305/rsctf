//! Player-facing A&D scoreboard + live team state + self-service reset.

use axum::http::header;
use axum::response::{IntoResponse, Response};

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

async fn visible_game_revision(st: &SharedState, game_id: i32) -> AppResult<Option<String>> {
    sqlx::query_scalar::<_, String>(
        r#"SELECT game.xmin::text
             FROM "Games" AS game
            WHERE game.id = $1 AND game.hidden = FALSE"#,
    )
    .bind(game_id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

fn revision_disposition(expected: &str, observed: Option<&str>) -> RevisionDisposition {
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
    let before = match visible_game_revision(st, id).await {
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

    let after_build = match visible_game_revision(st, id).await {
        Ok(revision) => revision,
        Err(error) => {
            tracing::warn!(game = id, %error, "A&D scoreboard revision validation failed");
            return ScoreboardBuildAttempt::Complete(ScoreboardFillResult::Failed);
        }
    };
    match revision_disposition(&before, after_build.as_deref()) {
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
        st.cache
            .set(current_key, &built.bytes, Some(AD_SCOREBOARD_FRESH_TTL))
            .await;
        st.cache
            .set(stale_key, &built.bytes, Some(AD_SCOREBOARD_STALE_TTL))
            .await;

        // Close the post-check/publication race: if a mutation committed and
        // hard-invalidated between validation and either SET, discard both
        // representations. If it commits after this query, its post-commit hard
        // invalidation owns the ordering and removes what was just published.
        let after_publish = match visible_game_revision(st, id).await {
            Ok(revision) => revision,
            Err(error) => {
                tracing::warn!(game = id, %error, "A&D scoreboard publication fence failed");
                hard_invalidate_ad_scoreboard_cache(st.cache.as_ref(), id).await;
                return ScoreboardBuildAttempt::Complete(ScoreboardFillResult::Failed);
            }
        };
        match revision_disposition(&before, after_publish.as_deref()) {
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

/// Game-global half of `Ad/State` — config + the challenge title/policy map. Shared by
/// every team and near-static, so it's cached (5 s); the per-team half (services, checks,
/// live flags) and the current round are read fresh so a just-planted flag is never stale
/// (the round is what the flag query keys on — the one field caching couldn't front).
#[derive(Clone, Debug, Serialize, Deserialize)]
struct AdStateCtx {
    reset_cooldown_secs: i64,
    end_time_utc: chrono::DateTime<chrono::Utc>,
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
    let (reset_minutes, end_time_utc, allow_snapshot, epoch_ticks, start_round) =
        sqlx::query_as::<_, (Option<i32>, DateTime<Utc>, bool, i32, Option<i32>)>(
            r#"SELECT ad_reset_cooldown_minutes, end_time_utc, ad_allow_snapshot_download,
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
        end_time_utc,
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

    // Post-game snapshot policy. `snapshot_available` per service must be the EXACT
    // success condition of the `Snapshot` download route below (or the client's download
    // button lies): the game is over, download is enabled, and the service has a platform
    // container that isn't a self-hosted (BYOC) relay.
    let now = Utc::now();
    let snapshots_downloadable = now >= ctx.end_time_utc && ctx.allow_snapshot;

    // One statement keeps the fresh round/services/checks/flags tail on one
    // MVCC snapshot and avoids four sequential pool checkouts/round trips.
    let super::state_tail::AdStateTail {
        current_round,
        round_started_at,
        round_ends_at,
        flags_ready,
        flag_delivery_failures,
        services,
    } = super::state_tail::load(st.pg(), id, part.id).await?;

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
            let snapshot_available =
                snapshots_downloadable && s.container_id.is_some() && !self_hosted;
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
                container_ip: Some(s.host),
                container_port: Some(s.port),
                current_flag: s.current_flag,
                last_check_status,
                last_reset_at: s.last_reset_at,
                // Self-hosted (BYOC): nothing on our side to relaunch, so never offer
                // the reset button (RSCTF State reduction 1388: `&& !AdSelfHosted`).
                can_reset: allow_self_reset && cooldown_remaining == 0 && !self_hosted,
                reset_cooldown_seconds_remaining: (cooldown_remaining > 0)
                    .then_some(cooldown_remaining),
                snapshot_available,
                self_hosted: Some(self_hosted),
            }
        })
        .collect();

    Ok(RequestResponse::ok(AdStateModel {
        current_round,
        epoch_ticks: ctx.epoch_ticks,
        start_round: ctx.start_round,
        flags_ready,
        flag_delivery_failures,
        round_started_at,
        round_ends_at,
        services: items,
    }))
}

/// `POST /api/Game/{id}/Ad/Services/{adTeamServiceId}/Reset` — the caller
/// restarts their own service container: destroy it, launch a fresh one with a
/// newly-planted flag, and stamp the self-reset cooldown. Requires the challenge
/// to allow self-reset and the cooldown to have elapsed.
pub async fn reset_service(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, ad_team_service_id)): Path<(i32, i32)>,
) -> AppResult<Response> {
    let part = resolve_participation(&st, &user, id).await?;
    let initial = ad_team_service::Entity::find_by_id(ad_team_service_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Service not found"))?;
    if initial.participation_id != part.id {
        return Err(AppError::Forbidden);
    }
    let lock_key = format!(
        "ad-service:{}:{}",
        initial.participation_id, initial.challenge_id
    );
    let _local = crate::utils::single_flight::coalesce(&lock_key).await;
    let distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(st.pg(), &lock_key)
            .await?;
    let svc = ad_team_service::Entity::find_by_id(ad_team_service_id)
        .one(&st.db)
        .await?
        .filter(|service| service.participation_id == part.id && service.game_id == id)
        .ok_or_else(|| AppError::not_found("Service not found"))?;
    let part = participation::Entity::find()
        .filter(participation::Column::Id.eq(part.id))
        .filter(participation::Column::GameId.eq(id))
        .filter(participation::Column::Status.eq(ParticipationStatus::Accepted))
        .one(&st.db)
        .await?
        .ok_or(AppError::Forbidden)?;
    let challenge = game_challenge::Entity::find()
        .filter(game_challenge::Column::Id.eq(svc.challenge_id))
        .filter(game_challenge::Column::GameId.eq(id))
        .filter(game_challenge::Column::IsEnabled.eq(true))
        .filter(game_challenge::Column::ReviewStatus.eq(ChallengeReviewStatus::Active))
        .filter(game_challenge::Column::ChallengeType.eq(ChallengeType::AttackDefense))
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Active A&D challenge not found"))?;
    if !challenge.ad_allow_self_reset {
        return Err(AppError::bad_request(
            "Self-reset is not allowed for this service",
        ));
    }
    // Self-hosted / BYOC services run in the team's own container, not one the
    // platform can relaunch — refuse rather than destroy a container we don't own.
    if challenge.ad_self_hosted {
        return Err(AppError::bad_request(
            "Self-hosted services cannot be reset from the platform",
        ));
    }
    let game = game::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Game not found"))?;
    // Reset only inside the game window. A post-game reset would recreate a
    // container for a finished game and race the end-of-game teardown; a
    // pre-start reset has nothing to reset (mirrors RSCTF's ResetService).
    let now = Utc::now();
    if now < game.start_time_utc || now >= game.end_time_utc {
        return Err(AppError::bad_request(
            "Reset is only available while the game is running",
        ));
    }
    let image = crate::services::challenge_images::runtime_image(&st, &challenge)?;
    let reset_cooldown_secs = game
        .ad_reset_cooldown_minutes
        .map(|m| m as i64 * 60)
        .unwrap_or(RESET_COOLDOWN_SECS_DEFAULT);
    if let Some(last) = svc.last_reset_at {
        let remaining = reset_cooldown_secs - (Utc::now() - last).num_seconds();
        if remaining > 0 {
            // RSCTF answers a cooldown rejection with 429 TooManyRequests + a
            // Retry-After header (whole seconds), not a plain 400 — mirror that so
            // scripted callers can honor the backoff.
            let mut resp =
                MessageResponse::new(format!("Cooldown active; try again in {remaining}s"), 429)
                    .into_response();
            if let Ok(val) = axum::http::HeaderValue::from_str(&remaining.to_string()) {
                resp.headers_mut().insert(header::RETRY_AFTER, val);
            }
            return Ok(resp);
        }
    }

    // Serialize with checker persistence, settle an unresolved current sample as
    // explicit reset downtime, and blank the old endpoint before Docker work.
    // Once rounds exist, the persisted AdFlags row is the only flag source.
    let replacement = crate::services::ad_engine::prepare_service_reset(
        &st.db,
        id,
        svc.id,
        "service reset before checker completion",
    )
    .await?;
    // Revoke the endpoint before teardown so a recycled Docker address cannot
    // remain reachable through this game's old policy.
    crate::services::ad_vpn::deactivate_team_service(&st.db, svc.id).await?;
    // Stop its capture + destroy the old container (best-effort), then relaunch.
    if let Some(cid) = &replacement.retired_container_id {
        crate::services::traffic::stop_container_capture(&st, cid).await?;
        let _ = st.containers.destroy(cid).await;
    }
    let prepared_round_id = replacement.prepared_round_id;
    let flag = replacement.current_flag.unwrap_or_else(|| {
        let salt = crate::utils::flag_generator::team_hash_salt(&game.private_key);
        let team_hash =
            crate::utils::flag_generator::team_challenge_hash(&salt, challenge.id, &part.token);
        crate::utils::flag_generator::generate_flag(challenge.flag_template.as_deref(), &team_hash)
    });
    let info = st
        .containers
        .create(crate::services::container::ContainerSpec::ad_service(
            image,
            challenge.memory_limit.unwrap_or(256),
            challenge.cpu_count.unwrap_or(1),
            challenge.expose_port.unwrap_or(80),
            part.team_id,
            challenge.ad_allow_egress,
            flag,
        ))
        .await?;

    let backend_id = info.id.clone();
    let published = match crate::services::ad_engine::publish_service_reset(
        &st.db,
        id,
        svc.id,
        &info.ip,
        info.port,
        &info.id,
        prepared_round_id,
        true,
    )
    .await
    {
        Ok(published) => published,
        Err(error) => {
            crate::services::traffic::stop_container_capture(&st, &backend_id).await?;
            let _ = st.containers.destroy(&backend_id).await;
            return Err(error);
        }
    };
    if !published {
        crate::services::traffic::stop_container_capture(&st, &backend_id).await?;
        let _ = st.containers.destroy(&backend_id).await;
        distributed.release().await?;
        return Err(AppError::Forbidden);
    }
    distributed.release().await?;
    if challenge.enable_traffic_capture {
        crate::services::traffic::start_container_capture(&st, &backend_id).await?;
    }
    crate::services::ad_vpn::reconcile_for_deployment(&st.db).await?;
    Ok(MessageResponse::ok("Service reset").into_response())
}

/// `GET /api/Game/{id}/Ad/Services/{adTeamServiceId}/Snapshot` — download the
/// post-game container snapshot tarball for one of the caller's OWN team's
/// services. Ported from RSCTF `AdGameController.DownloadSnapshot`.
///
/// RSCTF streams a stored `.tar.gz` blob keyed on `SnapshotBlobKey`. rsctf keeps
/// no snapshot-blob column, so this is **best-effort deterministic-without-
/// persistence**: the tarball is produced on demand by `docker export` of the
/// live service container (an uncompressed TAR of its current filesystem). The
/// gate is identical to the `snapshotAvailable` flag the player `state` reports,
/// so the client's download button never lies. Only the Docker backend can
/// export; on other backends the export fails with a clear message.
pub async fn download_snapshot(
    State(st): State<SharedState>,
    user: CurrentUser,
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

    let Some(cid) = svc.container_id.as_deref().filter(|c| !c.is_empty()) else {
        return Err(AppError::not_found(
            "Snapshot not available (no platform container for this service)",
        ));
    };
    let tar = st.containers.export(cid).await?;
    let filename = format!(
        "ad-snapshot-team{}-challenge{}.tar",
        svc.participation_id, svc.challenge_id
    );
    Ok((
        [
            (header::CONTENT_TYPE, "application/x-tar".to_string()),
            (header::CACHE_CONTROL, "private, no-store".to_string()),
            (header::PRAGMA, "no-cache".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        tar,
    )
        .into_response())
}

#[cfg(test)]
#[path = "scoreboard_tests.rs"]
mod scoreboard_cache_tests;
