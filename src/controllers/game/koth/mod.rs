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
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::Router;
use chrono::{DateTime, Utc};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};

use crate::app_state::SharedState;
use crate::controllers::game::ad::resolve_participation;
use crate::middlewares::privilege_authentication::CurrentUser;
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
mod scoreboard;
#[cfg(test)]
#[path = "scoreboard_wire_tests.rs"]
mod scoreboard_wire_tests;
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
pub(crate) use capture::ensure_koth_hills_with_operation;
pub(crate) use eligibility::invalidate_live_hill_cache;
use eligibility::require_live_hill;
pub(crate) use lifecycle::invalidate_live_lifecycle_cache;
use lifecycle::load_lifecycle_map;
pub use lifecycle::KothCooldownParticipant;
pub use listing::{koth_hills, KothHillListItem};
pub use routes::{router, stateful_router, web_router};
pub(crate) use scoreboard::build_koth_scoreboard_cached;
pub(super) use scoreboard::can_view_koth_standings;
pub use scoreboard::scoreboard;
pub(crate) use scoring::{
    invalidate_rollups_for_end_change, lock_epoch_rollups, refresh_epoch_rollups,
};
use scoring::{load_koth_scoring, KothScoringSnapshot};
pub use timeline::{timeline, KothScoreTimelineModel, KothTeamTimeline, KothTimelinePoint};
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
    pub control_revision: i64,
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
    pub control_revision: i64,
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
    result_is_scorable: bool,
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
    result_is_scorable: Option<bool>,
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
         cr.is_scorable AS result_is_scorable,
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
         SELECT result.container_id, result.status, result.is_scorable, result.checked_at
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

fn control_status_is_player_visible(
    holder_identity_is_current: bool,
    managed_crown_cycle: bool,
    reset_phase: &str,
    is_scorable: bool,
    result_is_scorable: bool,
) -> bool {
    holder_identity_is_current
        && result_is_scorable
        && (!managed_crown_cycle || (reset_phase == "Active" && is_scorable))
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
                result_is_scorable: row.result_is_scorable.unwrap_or(false),
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
    let control_status_is_visible = control_status_is_player_visible(
        holder_is_current,
        base.managed_crown_cycle,
        &view.reset_phase,
        view.is_scorable,
        base.result_is_scorable,
    );
    let status = control_status_is_visible.then_some(base.status).flatten();
    let checked_at = control_status_is_visible
        .then_some(base.checked_at)
        .flatten();
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
            get(koth_hill_token).merge(limited(
                Policy::CredentialMutation,
                post(rotate_koth_api_token),
            )),
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
        .merge(reporting_router())
    // No player score endpoint: Boot2Root hills read /koth/king, while Leaderboard
    // accepts evidence only from its challenge-scoped managed target or legacy reporter.
}

/// Narrow callback surface shared by public web replicas for compatibility and
/// the lifecycle-owning control process for private managed-target traffic.
fn reporting_router() -> Router<SharedState> {
    Router::new()
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
        control_evidence_is_current, control_status_is_player_visible,
        endpoint_identity_is_current, holder_identity_is_current, KothHillBase,
    };

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
            result_is_scorable: true,
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
    fn readiness_samples_are_not_presented_as_player_checker_failures() {
        assert!(control_status_is_player_visible(
            true, true, "Active", true, true
        ));
        assert!(!control_status_is_player_visible(
            true,
            true,
            "Readiness",
            false,
            false
        ));
        assert!(!control_status_is_player_visible(
            true, true, "Creating", false, false
        ));
        assert!(!control_status_is_player_visible(
            false, true, "Active", true, true
        ));
        assert!(!control_status_is_player_visible(
            true, true, "Active", true, false
        ));
        assert!(control_status_is_player_visible(
            true,
            false,
            "Readiness",
            false,
            true
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
