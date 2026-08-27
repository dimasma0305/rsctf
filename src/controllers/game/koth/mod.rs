//! King-of-the-Hill (KotH) gameplay, scoring, and lifecycle endpoints.
//!
//!   * `GET  /api/game/{id}/ad/koth/scoreboard`      → [`KothScoreboardModel`]
//!   * `GET  /api/game/{id}/ad/koth/timeline`        → [`KothScoreTimelineModel`]
//!   * `GET  /api/edit/games/{id}/ad/koth/state`     → [`AdminKothStateModel`] (admin)
//!
//!   * `GET  /api/game/{id}/ad/koth/{challengeId}/token` → the team's minted token
//!
//! # King-of-the-Hill — flow overview
//!
//! Unlike Attack & Defense (one container per team), a KotH challenge is a single
//! SHARED "hill" container that every team races to control. Each hill is modeled
//! by a [`koth_target`] row for the game.
//!
//! ## Control-token mechanism
//! Each accepted participation receives one exact capability per hill and crown
//! cycle. A team that has pwned a hill writes that hill's token into `/koth/king`.
//! The checker binds the observation to the exact cycle and container, confirms
//! consecutive healthy control, and updates the published holder. A token for one
//! hill or an earlier cycle is never valid for another target.
//!
//! ## Per-round history + scoring
//! `advance_round` persists one `KothControlResult` per hill. Official scoring
//! normalizes acquisition, control duration, and responsible-holder reliability
//! inside fixed epochs.

use std::collections::HashMap;

use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{header, HeaderMap};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use chrono::{DateTime, Utc};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};

use crate::app_state::SharedState;
use crate::controllers::game::ad::resolve_participation;
use crate::middlewares::privilege_authentication::{CurrentUser, MaybeUser};
use crate::middlewares::rate_limiter::{limited, Policy};
use crate::models::data::{game, game_challenge, koth_target};
use crate::utils::enums::{
    ChallengeCategory, ChallengeReviewStatus, ChallengeType, ParticipationStatus,
};
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::RequestResponse;

mod admin;
mod api;
mod api_contract;
mod board;
mod capture;
mod eligibility;
mod lifecycle;
mod listing;
mod routes;
mod scoring;
mod scoring_formula;
#[cfg(test)]
mod state_tests;
mod timeline;
mod tokens;
pub use admin::{admin_state, audit_receipts, recover_hill};
pub use api::{
    authenticate_capability, get_observer, observer_context, recover_observer_operation,
    revoke_observer, rotate_observer, submit_observation,
};
use board::*;
pub use capture::ensure_koth_hills;
pub(crate) use eligibility::invalidate_live_hill_cache;
use eligibility::require_live_hill;
pub(crate) use lifecycle::invalidate_live_lifecycle_cache;
use lifecycle::load_lifecycle_map;
pub use lifecycle::KothCooldownParticipant;
pub use listing::{koth_hills, KothHillListItem};
pub use routes::{router, stateful_router, web_router};
pub(crate) use scoring::{
    invalidate_rollups_for_end_change, lock_epoch_rollups, refresh_epoch_rollups,
};
use scoring::{load_koth_scoring, KothScoringSnapshot};
pub use timeline::{timeline, KothScoreTimelineModel, KothTeamTimeline, KothTimelinePoint};
#[cfg(test)]
use tokens::koth_token_cache_key;
pub(crate) use tokens::load_latest_round_cached;
pub use tokens::{
    koth_hill_token, koth_token_all, rotate_koth_api_token, KothHillTokenModel, KothTokenModel,
};

const KOTH_DETAIL_EPOCH_LIMIT: usize = 3;

/// AdCheckStatus numeric -> label, for the KotH board's per-hill verdict display.
fn koth_check_status_label(status: i16) -> &'static str {
    match status {
        0 => "Ok",
        1 => "Mumble",
        2 => "Offline",
        _ => "InternalError",
    }
}

// ---------------------------------------------------------------------------
// Response DTOs (camelCase on the wire; field-for-field with useGame.ts).
// ---------------------------------------------------------------------------

/// One hill column on the KotH board (`KothScoreboardHill` in useGame.ts).
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KothScoreboardHill {
    pub challenge_id: i32,
    pub title: String,
    /// Serializes as the enum's string name (e.g. `"Web"`, `"PPC"`), matching the
    /// `category: string` the React board feeds to `useChallengeCategoryLabelMap`.
    pub category: ChallengeCategory,
    /// `Marker` for exclusive boot2root control, `Api` for normalized arena scoring.
    pub claim_source: String,
    pub current_holder_team_name: Option<String>,
    pub current_holder_participation_id: Option<i32>,
    pub provisional_claimant_team_name: Option<String>,
    pub provisional_claimant_participation_id: Option<i32>,
    pub provisional_confirmation_ticks: i32,
    pub cycle_number: i32,
    pub cycle_tick: i32,
    pub reset_phase: String,
    pub is_scorable: bool,
    pub next_reset_ticks: Option<i32>,
    pub cooldown_participants: Vec<KothCooldownParticipant>,
    /// Latest checker verdict for the hill (from the KothControlResult history).
    pub last_check_status: Option<String>,
}

/// One team's score on one hill (`KothHillScore` in useGame.ts).
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KothHillScore {
    pub challenge_id: i32,
    pub settled_points: f64,
    pub projected_points: f64,
    pub acquisition_rate: f64,
    pub control_rate: f64,
    pub reliability_rate: f64,
    pub acquisition_windows: i64,
    pub controlled_ticks: i64,
    pub responsible_ticks: i64,
    pub healthy_responsible_ticks: i64,
    pub is_current_holder: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KothEpochScore {
    pub epoch: i32,
    pub points: f64,
    pub epoch_weight: f64,
    pub finalized: bool,
}

/// One team row on the KotH board (`KothTeamScoreRow`), shared by the player
/// scoreboard and the admin console.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KothTeamScoreRow {
    pub rank: i32,
    pub participation_id: i32,
    pub team_id: i32,
    pub team_name: String,
    pub division: Option<String>,
    pub settled_total: f64,
    pub projected_total: f64,
    /// Weighted point numerator behind `settled_total`.
    pub settled_epoch_points: f64,
    /// Finalized epoch weight behind `settled_total`.
    pub settled_epoch_weight: f64,
    /// Weighted point numerator behind `projected_total`.
    pub projected_epoch_points: f64,
    /// Finalized plus live epoch weight behind `projected_total`.
    pub projected_epoch_weight: f64,
    pub acquisition_rate: f64,
    pub control_rate: f64,
    pub reliability_rate: f64,
    pub hills: Vec<KothHillScore>,
    pub epochs: Vec<KothEpochScore>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KothCycleChampion {
    pub source_cycle_number: i32,
    pub participation_id: i32,
    pub team_name: String,
    pub healthy_controlled_ticks: i64,
}

/// `GET /api/game/{id}/ad/koth/scoreboard` response (`KothScoreboardModel`).
///
/// Timestamps follow the platform wire invariant and serialize as Unix millis.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KothScoreboardModel {
    pub epoch_ticks: i32,
    pub cycle_ticks: i32,
    pub champion_cooldown_ticks: i32,
    pub claim_confirmation_ticks: i32,
    pub start_round: Option<i32>,
    pub started: bool,
    pub fully_settled: bool,
    pub current_epoch: i32,
    pub detail_epoch_limit: usize,
    pub latest_round: i32,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub current_round_ends_at: Option<DateTime<Utc>>,
    pub tick_seconds: i64,
    #[serde(with = "crate::utils::datetime::millis")]
    pub generated_at: DateTime<Utc>,
    pub is_frozen_view: bool,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub freeze: Option<DateTime<Utc>>,
    pub hills: Vec<KothScoreboardHill>,
    pub teams: Vec<KothTeamScoreRow>,
}

/// One hill in the operator console (`AdminKothHill` in useGame.ts).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminKothHill {
    pub challenge_id: i32,
    pub title: String,
    pub is_enabled: bool,
    /// Shared hill container id (koth_target.container_id), when platform-hosted.
    pub container_guid: Option<String>,
    pub container_ip: Option<String>,
    pub container_port: Option<i32>,
    pub last_check_status: Option<String>,
    pub current_holder_team_name: Option<String>,
    pub current_holder_participation_id: Option<i32>,
    pub provisional_claimant_team_name: Option<String>,
    pub provisional_claimant_participation_id: Option<i32>,
    pub provisional_confirmation_ticks: i32,
    pub cycle_number: i32,
    pub cycle_tick: i32,
    pub durable_phase: String,
    pub reset_phase: String,
    pub is_scorable: bool,
    pub next_reset_ticks: Option<i32>,
    pub cooldown_participants: Vec<KothCooldownParticipant>,
    pub cycle_champions: Vec<KothCycleChampion>,
    pub old_container_id: Option<String>,
    pub replacement_container_id: Option<String>,
    pub reset_attempt: i32,
    pub readiness_failure_count: i32,
    pub last_readiness_error: Option<String>,
    pub can_retry: bool,
    pub reset_receipt_id: Option<i64>,
    pub scoring_receipt_id: Option<i64>,
    /// Officially snapshotted input transport, or the pre-start selection.
    pub claim_source: String,
    pub api_observer_configured: bool,
    pub api_observer_secret_hint: Option<String>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub api_last_observation_at: Option<DateTime<Utc>>,
}

/// `GET /api/edit/games/{id}/ad/koth/state` response (`AdminKothStateModel`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminKothStateModel {
    pub epoch_ticks: i32,
    pub cycle_ticks: i32,
    pub champion_cooldown_ticks: i32,
    pub claim_confirmation_ticks: i32,
    pub tick_seconds: i64,
    /// Version of the shared, single-flight scoring snapshot overlaid below.
    #[serde(with = "crate::utils::datetime::millis")]
    pub scoring_generated_at: DateTime<Utc>,
    pub latest_round: i32,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub current_round_ends_at: Option<DateTime<Utc>>,
    pub scoring_paused: bool,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub scoring_paused_at: Option<DateTime<Utc>>,
    pub hills: Vec<AdminKothHill>,
    pub teams: Vec<KothTeamScoreRow>,
}

/// `KothHillStateModel` — the hill's live holder + latest checker verdict.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KothHillStateModel {
    pub round: i32,
    /// The one currently published endpoint for this hill. Managed hills hide
    /// it during resets or an identity handoff to a replacement container.
    pub ip: Option<String>,
    pub port: Option<i32>,
    /// Marker is exclusive boot2root control; Api is normalized arena evidence.
    pub claim_source: String,
    pub holder_participation_id: Option<i32>,
    pub holder_team_name: Option<String>,
    pub is_you: bool,
    pub provisional_claimant_participation_id: Option<i32>,
    pub provisional_claimant_team_name: Option<String>,
    pub provisional_confirmation_ticks: i32,
    pub claim_confirmation_ticks: i32,
    pub cycle_number: i32,
    pub cycle_tick: i32,
    pub cycle_ticks: i32,
    pub reset_phase: String,
    pub is_scorable: bool,
    pub eligible_now: bool,
    pub is_you_cooldown: bool,
    pub cooldown_participants: Vec<KothCooldownParticipant>,
    pub next_reset_ticks: Option<i32>,
    pub status: Option<String>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub checked_at: Option<DateTime<Utc>>,
}

/// `GET /api/game/{id}/ad/koth/{challengeId}/state` — the hill's current holder
/// and last check status. Polled by `KothChallengePanel` (5s).
/// Cacheable, viewer-independent holder and verdict slice of one hill's live state.
/// The king of a hill is the same for everyone, so this is cached game-wide *per hill*;
/// viewer-specific and round-sensitive lifecycle fields are assembled per request.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct KothHillBase {
    container_id: Option<String>,
    ip: Option<String>,
    port: Option<i32>,
    managed_crown_cycle: bool,
    claim_source: String,
    holder_participation_id: Option<i32>,
    holder_team_name: Option<String>,
    status: Option<String>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    checked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, sqlx::FromRow)]
struct KothHillBaseRow {
    container_id: Option<String>,
    ip: Option<String>,
    port: Option<i32>,
    claim_source: String,
    holder_participation_id: Option<i32>,
    holder_team_name: Option<String>,
    evidence_container_id: Option<String>,
    status_raw: Option<i16>,
    checked_at: Option<DateTime<Utc>>,
    managed_crown_cycle: bool,
}

const KOTH_HILL_BASE_SQL: &str = r#"SELECT
         t.container_id,
         NULLIF(t.host, '') AS ip,
         NULLIF(t.port, 0) AS port,
         COALESCE((
           SELECT frozen.item->>'claimSource'
             FROM "KothOfficialConfigs" config,
                  LATERAL jsonb_array_elements(config.hills_snapshot) frozen(item)
            WHERE config.game_id = $1
              AND (frozen.item->>'challengeId')::integer = $2
            LIMIT 1
         ), CASE WHEN EXISTS (
           SELECT 1 FROM "KothApiObservers" observer
            WHERE observer.game_id = $1
              AND observer.challenge_id = $2
         ) THEN 'Api' ELSE 'Marker' END) AS claim_source,
         p.id AS holder_participation_id,
         tm.name AS holder_team_name,
         cr.container_id AS evidence_container_id,
         cr.status AS status_raw,
         cr.checked_at,
         EXISTS (
           SELECT 1 FROM "KothCrownCycles" crown
            WHERE crown.game_id = $1
              AND crown.challenge_id = $2
         ) AS managed_crown_cycle
       FROM "Games" g
       LEFT JOIN "KothTargets" t    ON t.game_id = $1 AND t.challenge_id = $2
       LEFT JOIN "Participations" p ON p.id = t.holder_participation_id
                                       AND p.game_id = $1
                                       AND p.status = 1
       LEFT JOIN "Teams" tm         ON tm.id = p.team_id
       LEFT JOIN LATERAL (
         SELECT result.container_id, result.status, result.checked_at
           FROM "KothControlResults" result
          WHERE result.game_id = $1 AND result.challenge_id = $2
          ORDER BY result.ad_round_id DESC, result.id DESC LIMIT 1
       ) cr ON TRUE
       WHERE g.id = $1"#;

fn holder_identity_is_current(
    cycle_number: i32,
    target_container_id: Option<&str>,
    cycle_container_id: Option<&str>,
) -> bool {
    cycle_number == 0
        || matches!(
            (target_container_id, cycle_container_id),
            (Some(target), Some(cycle)) if !target.is_empty() && target == cycle
        )
}

fn endpoint_identity_is_current(
    round: i32,
    managed_crown_cycle: bool,
    cycle_number: i32,
    reset_phase: &str,
    target_container_id: Option<&str>,
    cycle_container_id: Option<&str>,
) -> bool {
    round > 0
        && (!managed_crown_cycle
            || (cycle_number > 0
                && reset_phase == "Active"
                && holder_identity_is_current(
                    cycle_number,
                    target_container_id,
                    cycle_container_id,
                )))
}

pub(crate) fn control_evidence_is_current(
    managed_crown_cycle: bool,
    observed_container_id: Option<&str>,
    published_container_id: Option<&str>,
) -> bool {
    if managed_crown_cycle {
        observed_container_id.is_some() && observed_container_id == published_container_id
    } else {
        observed_container_id == published_container_id
    }
}

static KOTH_HILL_STATE_SF: std::sync::LazyLock<
    crate::utils::single_flight::SingleFlight<Option<KothHillBase>>,
> = std::sync::LazyLock::new(crate::utils::single_flight::SingleFlight::new);

/// Load one hill's game-wide state — one raw-SQL join (was five sequential `.one()`s)
/// behind a 5 s cache, single-flighted so a 250-team poll herd at TTL expiry rebuilds
/// once. Both levers: the cache kills the herd, the join makes the miss cheap too.
async fn load_hill_base(st: &SharedState, id: i32, challenge_id: i32) -> AppResult<KothHillBase> {
    let key = format!("_KothHillState_{id}_{challenge_id}");
    if let Some(b) = st.cache.get(&key).await {
        if let Ok(x) = serde_json::from_slice::<KothHillBase>(&b) {
            return Ok(x);
        }
    }
    let st = st.clone();
    let key_for_fill = key.clone();
    KOTH_HILL_STATE_SF
        .run(&key, move || async move {
            if let Some(bytes) = st.cache.get(&key_for_fill).await {
                if let Ok(base) = serde_json::from_slice::<KothHillBase>(&bytes) {
                    return Some(base);
                }
            }
            let row = sqlx::query_as::<_, KothHillBaseRow>(KOTH_HILL_BASE_SQL)
            .bind(id)
            .bind(challenge_id)
            .fetch_one(st.pg())
            .await;
            let row = match row {
                Ok(row) => row,
                Err(error) => {
                    tracing::warn!(game = id, challenge = challenge_id, %error, "KotH hill state cache fill failed");
                    return None;
                }
            };
            let evidence_is_current = control_evidence_is_current(
                row.managed_crown_cycle,
                row.evidence_container_id.as_deref(),
                row.container_id.as_deref(),
            );
            let base = KothHillBase {
                container_id: row.container_id,
                ip: row.ip,
                port: row.port,
                managed_crown_cycle: row.managed_crown_cycle,
                claim_source: row.claim_source,
                holder_participation_id: row.holder_participation_id,
                holder_team_name: row.holder_team_name,
                status: evidence_is_current
                    .then(|| {
                        row.status_raw
                            .map(|status| koth_check_status_label(status).to_string())
                    })
                    .flatten(),
                checked_at: evidence_is_current.then_some(row.checked_at).flatten(),
            };
            let json = match serde_json::to_vec(&base) {
                Ok(json) => json,
                Err(error) => {
                    tracing::warn!(game = id, challenge = challenge_id, %error, "KotH hill state serialization failed");
                    return None;
                }
            };
            st.cache
                .set(
                    &key_for_fill,
                    &json,
                    Some(std::time::Duration::from_secs(5)),
                )
                .await;
            Some(base)
        })
        .await
        .ok_or_else(|| AppError::internal("KotH hill state cache fill failed"))
}

pub async fn koth_hill_state(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, challenge_id)): Path<(i32, i32)>,
) -> AppResult<RequestResponse<KothHillStateModel>> {
    let part = resolve_participation(&st, &user, id).await?;
    require_live_hill(&st, id, challenge_id).await?;
    let (base, round) = tokio::try_join!(
        load_hill_base(&st, id, challenge_id),
        load_latest_round_cached(&st, id)
    )?;
    let lifecycle = load_lifecycle_map(&st, id, round, None).await?;
    let view = lifecycle.get(&challenge_id).cloned().unwrap_or_default();
    let holder_is_current = holder_identity_is_current(
        view.cycle_number,
        base.container_id.as_deref(),
        view.replacement_container_id.as_deref(),
    );
    let holder_participation_id = holder_is_current
        .then_some(base.holder_participation_id)
        .flatten();
    let holder_team_name = holder_is_current.then_some(base.holder_team_name).flatten();
    let status = holder_is_current.then_some(base.status).flatten();
    let checked_at = holder_is_current.then_some(base.checked_at).flatten();
    let endpoint_is_current = endpoint_identity_is_current(
        round,
        base.managed_crown_cycle,
        view.cycle_number,
        &view.reset_phase,
        base.container_id.as_deref(),
        view.replacement_container_id.as_deref(),
    );
    let ip = endpoint_is_current.then_some(base.ip).flatten();
    let port = endpoint_is_current.then_some(base.port).flatten();
    let is_you_cooldown = view
        .cooldown_participants
        .iter()
        .any(|cooldown| cooldown.participation_id == part.id);
    Ok(RequestResponse::ok(KothHillStateModel {
        round,
        ip,
        port,
        claim_source: base.claim_source,
        holder_participation_id,
        holder_team_name,
        is_you: holder_participation_id == Some(part.id),
        provisional_claimant_participation_id: view.provisional_participation_id,
        provisional_claimant_team_name: view.provisional_team_name,
        provisional_confirmation_ticks: view.confirmation_progress,
        claim_confirmation_ticks: view.claim_confirmation_ticks,
        cycle_number: view.cycle_number,
        cycle_tick: view.cycle_tick,
        cycle_ticks: view.cycle_ticks,
        reset_phase: view.reset_phase,
        is_scorable: view.is_scorable,
        eligible_now: view.is_scorable && !is_you_cooldown,
        is_you_cooldown,
        cooldown_participants: view.cooldown_participants,
        next_reset_ticks: view.next_reset_ticks,
        status,
        checked_at,
    }))
}

fn common_router() -> Router<SharedState> {
    Router::new()
        // Player KotH board. Lowercase `/api/game/{id}/...` — distinct from the
        // A&D board's capitalized `/api/Game/{id}/...` (routing is case-sensitive),
        // and the `{id}` param name matches game.rs/edit.rs so the shared prefix
        // doesn't trip matchit's param-name conflict check at merge time.
        .route("/api/game/{id}/ad/koth/scoreboard", get(scoreboard))
        // Player KotH score-over-time chart (A&D timeline shape).
        .route("/api/game/{id}/ad/koth/timeline", get(timeline))
        // Per-hill player token + state (KothChallengePanel polls these).
        .route(
            "/api/game/{id}/ad/koth/{challengeId}/token",
            get(koth_hill_token).merge(limited(Policy::Container, post(rotate_koth_api_token))),
        )
        .route(
            "/api/game/{id}/ad/koth/{challengeId}/state",
            get(koth_hill_state),
        )
        // Admin KotH operator console.
        .route("/api/edit/games/{id}/ad/koth/state", get(admin_state))
        .route(
            "/api/edit/games/{id}/ad/koth/{challengeId}/receipts",
            get(audit_receipts),
        )
        .route(
            "/api/edit/games/{id}/ad/koth/{challengeId}/observer",
            get(get_observer)
                .post(rotate_observer)
                .delete(revoke_observer),
        )
        .route(
            "/api/edit/games/{id}/ad/koth/{challengeId}/observer/operations/{operationId}",
            get(recover_observer_operation),
        )
        .route(
            "/api/v1/koth/games/{id}/challenges/{challengeId}/context",
            get(observer_context),
        )
        .route(
            "/api/v1/koth/games/{id}/challenges/{challengeId}/observations",
            post(submit_observation).layer(DefaultBodyLimit::max(api_contract::MAX_BODY_BYTES)),
        )
        .route(
            "/api/v1/koth/capability/authenticate",
            post(authenticate_capability).layer(DefaultBodyLimit::max(1_024)),
        )
    // No player score endpoint: Boot2Root hills read /koth/king, while Leaderboard
    // accepts evidence only from its challenge-scoped trusted referee.
}

fn recovery_router() -> Router<SharedState> {
    Router::new()
        .route(
            "/api/edit/games/{id}/ad/koth/{challengeId}/recover",
            post(recover_hill),
        )
        .route(
            "/api/stateful/edit/games/{id}/ad/koth/{challengeId}/recover",
            post(recover_hill),
        )
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod token_cache_tests {
    use super::{
        cached_koth_json, can_view_koth_standings, control_evidence_is_current,
        endpoint_identity_is_current, holder_identity_is_current, koth_cache_key,
        koth_token_cache_key, KothHillBase,
    };
    use chrono::{TimeZone, Utc};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[test]
    fn bearer_capabilities_are_cached_per_hill() {
        assert_ne!(
            koth_token_cache_key(1, 10, 7, 3),
            koth_token_cache_key(1, 11, 7, 3)
        );
    }

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
                cached_koth_json(cache, key, move || async move {
                    builds.fetch_add(1, Ordering::SeqCst);
                    tokio::task::yield_now().await;
                    Some(bytes::Bytes::from_static(b"{\"version\":41}"))
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
        let cached = cached_koth_json(cache, key, move || async move {
            second_version_builds.fetch_add(1, Ordering::SeqCst);
            Some(bytes::Bytes::from_static(b"unexpected"))
        })
        .await
        .unwrap();
        assert_eq!(cached.as_ref(), b"{\"version\":41}");
        assert_eq!(builds.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn lifecycle_round_is_not_part_of_cached_hill_state() {
        let cached = serde_json::to_value(KothHillBase {
            container_id: Some("container-a".to_string()),
            ip: Some("10.0.0.7".to_string()),
            port: Some(31337),
            managed_crown_cycle: true,
            claim_source: "Marker".to_string(),
            holder_participation_id: Some(7),
            holder_team_name: Some("red".to_string()),
            status: Some("Ok".to_string()),
            checked_at: None,
        })
        .unwrap();

        assert!(cached.get("round").is_none());
    }

    #[test]
    fn cached_holder_is_hidden_when_the_published_container_changes() {
        assert!(holder_identity_is_current(
            4,
            Some("container-a"),
            Some("container-a")
        ));
        assert!(!holder_identity_is_current(
            4,
            Some("container-a"),
            Some("container-b")
        ));
        assert!(!holder_identity_is_current(4, Some("container-a"), None));
        assert!(!holder_identity_is_current(4, None, None));
        assert!(holder_identity_is_current(
            0,
            Some("legacy-container"),
            None
        ));
    }

    #[test]
    fn scoped_endpoint_tracks_managed_identity_and_fails_closed_without_a_live_cycle() {
        assert!(endpoint_identity_is_current(
            7,
            true,
            4,
            "Active",
            Some("container-a"),
            Some("container-a")
        ));
        assert!(!endpoint_identity_is_current(
            7,
            true,
            4,
            "Readiness",
            Some("container-a"),
            Some("container-a")
        ));
        assert!(!endpoint_identity_is_current(
            7,
            true,
            4,
            "Active",
            Some("container-a"),
            Some("container-b")
        ));
        assert!(!endpoint_identity_is_current(
            7,
            true,
            0,
            "Readiness",
            None,
            None
        ));
        assert!(!endpoint_identity_is_current(
            7,
            true,
            0,
            "Active",
            Some("stale-container"),
            None
        ));
        assert!(endpoint_identity_is_current(
            7, false, 0, "Active", None, None
        ));
        assert!(!endpoint_identity_is_current(
            0,
            false,
            0,
            "Active",
            Some("pre-start-target"),
            None
        ));
    }

    #[test]
    fn external_null_identity_keeps_status_but_managed_null_identity_does_not() {
        assert!(control_evidence_is_current(false, None, None));
        assert!(control_evidence_is_current(
            false,
            Some("external-a"),
            Some("external-a")
        ));
        assert!(!control_evidence_is_current(true, None, None));
        assert!(control_evidence_is_current(
            true,
            Some("container-a"),
            Some("container-a")
        ));
        assert!(!control_evidence_is_current(
            true,
            Some("container-a"),
            Some("container-b")
        ));
    }
}

#[cfg(test)]
mod recovery_route_tests {
    use axum::extract::Path;
    use axum::response::IntoResponse;

    #[tokio::test]
    async fn web_recovery_redirect_preserves_method_and_uses_stateful_prefix() {
        let response = super::routes::redirect_recover_hill(Path((17, 23)))
            .await
            .into_response();
        assert_eq!(
            response.status(),
            axum::http::StatusCode::TEMPORARY_REDIRECT
        );
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::LOCATION)
                .unwrap(),
            "/api/stateful/edit/games/17/ad/koth/23/recover"
        );
    }
}

// ---------------------------------------------------------------------------
// Handlers — read
// ---------------------------------------------------------------------------

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
        format!("_KothScoreBoardFrozen_{game_id}")
    } else {
        format!("_KothScoreBoard_{game_id}")
    }
}

/// Hidden event standings stay undiscoverable to ordinary callers while the
/// authenticated monitor retains the same operational view exposed by the
/// combined scoreboard and other game read endpoints.
pub(super) fn can_view_koth_standings(game_hidden: bool, is_monitor: bool) -> bool {
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

async fn koth_scoreboard_json(
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
    cached_koth_json(st.cache.clone(), key, move || async move {
        let model = build_koth_scoreboard(&st2, &game2, is_monitor, now)
            .await
            .ok()?;
        serde_json::to_vec(&model).ok().map(bytes::Bytes::from)
    })
    .await
}

async fn cached_koth_json<Build, BuildFuture>(
    cache: std::sync::Arc<dyn crate::services::cache::Cache>,
    key: String,
    build: Build,
) -> AppResult<bytes::Bytes>
where
    Build: FnOnce() -> BuildFuture + Send + 'static,
    BuildFuture: std::future::Future<Output = Option<bytes::Bytes>> + Send + 'static,
{
    if let Some(bytes) = cache.get(&key).await {
        return Ok(bytes);
    }
    let (cache2, key2) = (cache, key.clone());
    let coalesced = KOTH_SF
        .run(&key, move || async move {
            if let Some(bytes) = cache2.get(&key2).await {
                return Some(bytes);
            }
            let bytes = build().await?;
            cache2.set(&key2, &bytes, Some(KOTH_CACHE_TTL)).await;
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
    let bytes = koth_scoreboard_json(st, game, is_monitor).await?;
    serde_json::from_slice(&bytes).map_err(|error| AppError::internal(error.to_string()))
}

/// `GET /api/game/{id}/ad/koth/scoreboard` — the player KotH board: one column per
/// enabled hill, one ranked row per team with its bounded per-hill epoch score. Served
/// from the two-tier cache as raw bytes (byte-identical to the model), so a poll
/// flood no longer recomputes the board on every request.
pub async fn scoreboard(
    State(st): State<SharedState>,
    MaybeUser(maybe): MaybeUser,
    Path(game_id): Path<i32>,
) -> AppResult<Response> {
    // Keep hidden events undiscoverable to ordinary callers while allowing the
    // authenticated monitor to operate the private event. 1s-cached game row.
    let game = super::load_game_cached(&st, game_id).await?;
    let is_monitor = maybe.as_ref().is_some_and(|u| u.is_monitor());
    if !can_view_koth_standings(game.hidden, is_monitor) {
        return Err(AppError::not_found("Game not found"));
    }
    let json = koth_scoreboard_json(&st, &game, is_monitor).await?;
    Ok(([(header::CONTENT_TYPE, "application/json")], json).into_response())
}

// ---------------------------------------------------------------------------
// Handlers — write
// ---------------------------------------------------------------------------
