//! King-of-the-Hill (KotH) crown-cycle endpoints.
//!
//! Three read endpoints back the React KotH board + operator console (paths and
//! shapes match `web/src/hooks/useGame.ts` verbatim):
//!   * `GET  /api/game/{id}/ad/koth/scoreboard`      → [`KothScoreboardModel`]
//!   * `GET  /api/game/{id}/ad/koth/timeline`        → [`KothScoreTimelineModel`]
//!   * `GET  /api/edit/games/{id}/ad/koth/state`     → [`AdminKothStateModel`] (admin)
//!
//! plus the per-team token endpoint (the string a team writes into a hill to claim it):
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

use axum::extract::{Path, State};
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
use crate::models::data::{game, game_challenge, koth_target};
use crate::utils::enums::{
    ChallengeCategory, ChallengeReviewStatus, ChallengeType, ParticipationStatus,
};
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::RequestResponse;

mod admin;
mod board;
mod capture;
mod eligibility;
mod lifecycle;
mod listing;
mod scoring;
mod scoring_formula;
mod timeline;
pub use admin::{admin_state, audit_receipts, recover_hill};
use board::*;
pub use capture::ensure_koth_hills;
pub(crate) use eligibility::invalidate_live_hill_cache;
use eligibility::require_live_hill;
pub(crate) use lifecycle::invalidate_live_lifecycle_cache;
use lifecycle::load_lifecycle_map;
pub use lifecycle::KothCooldownParticipant;
pub use listing::{koth_hills, KothHillListItem};
pub(crate) use scoring::{
    invalidate_rollups_for_end_change, lock_epoch_rollups, refresh_epoch_rollups,
};
use scoring::{load_koth_scoring, KothScoringSnapshot};
pub use timeline::{timeline, KothScoreTimelineModel, KothTeamTimeline, KothTimelinePoint};

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
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KothScoreboardHill {
    pub challenge_id: i32,
    pub title: String,
    /// Serializes as the enum's string name (e.g. `"Web"`, `"PPC"`), matching the
    /// `category: string` the React board feeds to `useChallengeCategoryLabelMap`.
    pub category: ChallengeCategory,
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
#[derive(Debug, Serialize)]
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KothEpochScore {
    pub epoch: i32,
    pub points: f64,
    pub epoch_weight: f64,
    pub finalized: bool,
}

/// One team row on the KotH board (`KothTeamScoreRow`), shared by the player
/// scoreboard and the admin console.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KothTeamScoreRow {
    pub rank: i32,
    pub participation_id: i32,
    pub team_id: i32,
    pub team_name: String,
    pub division: Option<String>,
    pub settled_total: f64,
    pub projected_total: f64,
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
#[derive(Debug, Serialize)]
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
    pub hills: Vec<AdminKothHill>,
    pub teams: Vec<KothTeamScoreRow>,
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// `KothTokenModel` (KothChallengePanel) — the cycle-scoped capability the team
/// plants into this exact hill.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct KothTokenModel {
    pub round: i32,
    pub token: Option<String>,
    /// `"warmup"` (no round yet) | `"no-cycle-token"` | `"ready"`.
    pub status: String,
}

fn koth_token_cache_key(
    game_id: i32,
    challenge_id: i32,
    participation_id: i32,
    round: i32,
) -> String {
    format!("kothtoken:{game_id}:{challenge_id}:{participation_id}:{round}")
}

/// Authoritative short-lived round pointer shared by every player-facing KotH
/// and A&D projection. Keeping one source prevents independently cached views
/// from disagreeing for several seconds at a scoring boundary.
pub(crate) async fn load_latest_round_cached(st: &SharedState, game_id: i32) -> AppResult<i32> {
    let key = format!("latestround:{game_id}");
    if let Some(bytes) = st.cache.get(&key).await {
        if let Ok(encoded) = <[u8; 4]>::try_from(bytes.as_ref()) {
            return Ok(i32::from_le_bytes(encoded));
        }
    }

    static LATEST_ROUND_SF: std::sync::LazyLock<
        crate::utils::single_flight::SingleFlight<Option<i32>>,
    > = std::sync::LazyLock::new(crate::utils::single_flight::SingleFlight::new);
    let st = st.clone();
    let key_for_fill = key.clone();
    LATEST_ROUND_SF
        .run(&key, move || async move {
            // A second cache check lets followers reuse a value populated while
            // they joined the flight. The one-second lifetime bounds UI/token
            // boundary lag without sending a poll herd to PostgreSQL.
            if let Some(bytes) = st.cache.get(&key_for_fill).await {
                if let Ok(encoded) = <[u8; 4]>::try_from(bytes.as_ref()) {
                    return Some(i32::from_le_bytes(encoded));
                }
            }
            let round = match sqlx::query_scalar::<_, i32>(
                r#"SELECT number FROM "AdRounds"
                    WHERE game_id = $1 ORDER BY number DESC LIMIT 1"#,
            )
            .bind(game_id)
            .fetch_optional(st.pg())
            .await
            {
                Ok(round) => round.unwrap_or(0),
                Err(error) => {
                    tracing::warn!(game = game_id, %error, "KotH latest-round cache fill failed");
                    return None;
                }
            };
            st.cache
                .set(
                    &key_for_fill,
                    &round.to_le_bytes(),
                    Some(std::time::Duration::from_secs(1)),
                )
                .await;
            Some(round)
        })
        .await
        .ok_or_else(|| AppError::internal("KotH latest-round cache fill failed"))
}

/// `GET /api/game/{id}/ad/koth/{challengeId}/token` — the caller team's control
/// active-cycle capability for this hill. Polled by `KothChallengePanel`.
pub async fn koth_hill_token(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, challenge_id)): Path<(i32, i32)>,
) -> AppResult<RequestResponse<KothTokenModel>> {
    let part = resolve_participation(&st, &user, id).await?;
    require_live_hill(&st, id, challenge_id).await?;

    let latest_round = load_latest_round_cached(&st, id).await?;

    // Capabilities are exact per-hill and per-cycle. Cache identity therefore
    // includes the challenge and current round projection.
    let token_key = koth_token_cache_key(id, challenge_id, part.id, latest_round);
    if let Some(bytes) = st.cache.get(&token_key).await {
        if let Ok(model) = serde_json::from_slice::<KothTokenModel>(&bytes) {
            return Ok(RequestResponse::ok(model));
        }
    }

    let (token, status) = if latest_round == 0 {
        (None, "warmup".to_string())
    } else {
        let token: Option<String> = sqlx::query_scalar(
            r#"SELECT token.token
                 FROM "KothTokens" token
                 JOIN "KothCrownCycles" cycle ON cycle.id = token.cycle_id
                 JOIN "KothTargets" target ON target.id = token.target_id
                WHERE cycle.game_id = $1 AND cycle.challenge_id = $2
                  AND cycle.phase = 'Active'
                  AND target.container_id = cycle.replacement_container_id
                  AND token.participation_id = $3
                  AND token.challenge_id = $2
                  AND token.reset_attempt = cycle.reset_attempt
                  AND token.revoked_at IS NULL
                ORDER BY cycle.cycle_number DESC LIMIT 1"#,
        )
        .bind(id)
        .bind(challenge_id)
        .bind(part.id)
        .fetch_optional(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        let status = if token.is_some() {
            "ready"
        } else {
            "no-cycle-token"
        };
        (token, status.to_string())
    };

    let model = KothTokenModel {
        round: latest_round,
        token,
        status,
    };
    if let Ok(json) = serde_json::to_vec(&model) {
        st.cache
            .set(&token_key, &json, Some(std::time::Duration::from_secs(10)))
            .await;
    }

    Ok(RequestResponse::ok(model))
}

/// `KothHillStateModel` — the hill's live holder + latest checker verdict.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KothHillStateModel {
    pub round: i32,
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
    holder_participation_id: Option<i32>,
    holder_team_name: Option<String>,
    status: Option<String>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    checked_at: Option<DateTime<Utc>>,
}

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
            let row = sqlx::query_as::<
                _,
                (
                    Option<String>,
                    Option<i32>,
                    Option<String>,
                    Option<String>,
                    Option<i16>,
                    Option<DateTime<Utc>>,
                    bool,
                ),
            >(
                r#"SELECT
                         t.container_id,
                         p.id,
                         tm.name,
                         cr.container_id,
                         cr.status,
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
                       WHERE g.id = $1"#,
            )
            .bind(id)
            .bind(challenge_id)
            .fetch_one(st.pg())
            .await;
            let (
                container_id,
                holder_pid,
                holder_team,
                evidence_container_id,
                status_raw,
                checked_at,
                managed_crown_cycle,
            ) = match row {
                Ok(row) => row,
                Err(error) => {
                    tracing::warn!(game = id, challenge = challenge_id, %error, "KotH hill state cache fill failed");
                    return None;
                }
            };
            let evidence_is_current = control_evidence_is_current(
                managed_crown_cycle,
                evidence_container_id.as_deref(),
                container_id.as_deref(),
            );
            let base = KothHillBase {
                container_id,
                holder_participation_id: holder_pid,
                holder_team_name: holder_team,
                status: evidence_is_current
                    .then(|| status_raw.map(|status| koth_check_status_label(status).to_string()))
                    .flatten(),
                checked_at: evidence_is_current.then_some(checked_at).flatten(),
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
    let is_you_cooldown = view
        .cooldown_participants
        .iter()
        .any(|cooldown| cooldown.participation_id == part.id);
    Ok(RequestResponse::ok(KothHillStateModel {
        round,
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

/// One enabled hill's cycle-scoped capability for the caller's team
/// (`Koth/Token` list element). Each hill has an independent capability, so this
/// endpoint returns one row per hill rather than a scalar.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct KothHillTokenModel {
    pub challenge_id: i32,
    pub token: String,
}

/// `GET /api/Game/{id}/Ad/Koth/Token` — the caller team's control token for EVERY
/// enabled KotH hill in one call (KothGuideModal's `Koth/Token` example). Dual
/// auth (same as Submit): `Bearer ad_...` token or the interactive session.
pub async fn koth_token_all(
    State(st): State<SharedState>,
    maybe_user: MaybeUser,
    Path(id): Path<i32>,
    headers: HeaderMap,
    verified: Option<axum::Extension<crate::services::ad::api_token::VerifiedTeamToken>>,
    rejected: Option<axum::Extension<crate::services::ad::api_token::RejectedTeamToken>>,
) -> AppResult<RequestResponse<Vec<KothHillTokenModel>>> {
    let part = crate::controllers::game::ad::resolve_ad_attacker(
        &st,
        &headers,
        verified.as_ref().map(|extension| &extension.0),
        rejected.as_ref().map(|extension| &extension.0),
        maybe_user,
        id,
    )
    .await?;

    let latest_round = load_latest_round_cached(&st, id).await?;

    // Warmup — no tokens until the first round has been planted (mirrors the
    // per-hill token endpoint's `"warmup"` state).
    if latest_round == 0 {
        return Ok(RequestResponse::ok(Vec::new()));
    }

    let cache_key = format!("kothtokensall:{id}:{}:{latest_round}", part.id);
    if let Some(bytes) = st.cache.get(&cache_key).await {
        if let Ok(model) = serde_json::from_slice::<Vec<KothHillTokenModel>>(&bytes) {
            return Ok(RequestResponse::ok(model));
        }
    }

    // Return only active exact-hill capabilities; disabled or unreviewed hills
    // never leak a token.
    let out: Vec<KothHillTokenModel> = sqlx::query_as::<_, (i32, String)>(
        r#"SELECT token.challenge_id, token.token
                 FROM "KothTokens" token
                 JOIN "KothCrownCycles" cycle ON cycle.id = token.cycle_id
                 JOIN "KothTargets" target ON target.id = token.target_id
                 JOIN "GameChallenges" challenge
                   ON challenge.id = token.challenge_id
                  AND challenge.game_id = cycle.game_id
                WHERE cycle.game_id = $1 AND cycle.phase = 'Active'
                  AND target.container_id = cycle.replacement_container_id
                  AND token.reset_attempt = cycle.reset_attempt
                  AND challenge.is_enabled = TRUE
                  AND challenge.review_status = $3
                  AND challenge."Type" = $4
                  AND token.participation_id = $2 AND token.revoked_at IS NULL
                ORDER BY token.challenge_id"#,
    )
    .bind(id)
    .bind(part.id)
    .bind(ChallengeReviewStatus::Active as i16)
    .bind(ChallengeType::KingOfTheHill as i16)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .into_iter()
    .map(|(challenge_id, token)| KothHillTokenModel {
        challenge_id,
        token,
    })
    .collect();

    if let Ok(json) = serde_json::to_vec(&out) {
        st.cache
            .set(&cache_key, &json, Some(std::time::Duration::from_secs(10)))
            .await;
    }

    Ok(RequestResponse::ok(out))
}

pub fn router() -> Router<SharedState> {
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
            get(koth_hill_token),
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
            "/api/edit/games/{id}/ad/koth/{challengeId}/recover",
            post(recover_hill),
        )
    // No capture endpoint: a team claims a hill by writing its minted token into the
    // hill's /koth/king. The checker reads it each tick to elect the
    // king — there is no platform-side capture call.
}

#[cfg(test)]
mod token_cache_tests {
    use super::{
        control_evidence_is_current, holder_identity_is_current, koth_token_cache_key, KothHillBase,
    };

    #[test]
    fn bearer_capabilities_are_cached_per_hill() {
        assert_ne!(
            koth_token_cache_key(1, 10, 7, 3),
            koth_token_cache_key(1, 11, 7, 3)
        );
    }

    #[test]
    fn lifecycle_round_is_not_part_of_cached_hill_state() {
        let cached = serde_json::to_value(KothHillBase {
            container_id: Some("container-a".to_string()),
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

// ---------------------------------------------------------------------------
// Handlers — read
// ---------------------------------------------------------------------------

/// Cache + coalesce the KotH board like the jeopardy + A&D boards. Its recompute
/// (`compute_koth_board` — a per-hill/-team scan of the control-result history)
/// otherwise ran on EVERY poll (measured ~26× slower than the cached boards, with
/// Postgres pinned at ~216% under a poll flood). Keyed on `(game, is_monitor)` and
/// freeze-aware (the frozen variant bakes the cutoff), so a cached copy is only
/// ever `KOTH_CACHE_TTL` stale across the freeze/end boundary — the same tradeoff
/// the other cached boards accept.
static KOTH_SF: std::sync::LazyLock<
    crate::utils::single_flight::SingleFlight<Option<bytes::Bytes>>,
> = std::sync::LazyLock::new(crate::utils::single_flight::SingleFlight::new);
const KOTH_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(5);

fn koth_cache_key(game_id: i32, is_monitor: bool) -> String {
    if is_monitor {
        format!("_KothScoreBoard_{game_id}")
    } else {
        format!("_KothScoreBoardFrozen_{game_id}")
    }
}

/// Compute the rendered KotH board for `(game, is_monitor)`: derive the ICPC
/// freeze / post-end cutoff, run [`compute_koth_board`], and shape the wire model.
async fn build_koth_scoreboard(
    st: &SharedState,
    game: &game::Model,
    is_monitor: bool,
) -> AppResult<KothScoreboardModel> {
    // ICPC freeze: a non-monitor inside `[FreezeTimeUtc, EndTimeUtc)` sees the
    // FROZEN board; monitors always see it live.
    let now = Utc::now();
    let mut cutoff: Option<DateTime<Utc>> = match game.freeze_time_utc {
        Some(freeze) if !is_monitor && now >= freeze && now < game.end_time_utc => Some(freeze),
        _ => None,
    };
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
        is_frozen_view: cutoff.is_some(),
        freeze: board.freeze,
        hills,
        teams,
    })
}

/// The KotH board's wire body as raw JSON, from the two-tier cache or freshly
/// built, with single-flight coalescing — mirrors `build_scoreboard_json`.
async fn koth_scoreboard_json(
    st: &SharedState,
    game: &game::Model,
    is_monitor: bool,
) -> AppResult<bytes::Bytes> {
    let key = koth_cache_key(game.id, is_monitor);
    if let Some(bytes) = st.cache.get(&key).await {
        return Ok(bytes);
    }
    let (st2, game2, key2) = (st.clone(), game.clone(), key.clone());
    let coalesced = KOTH_SF
        .run(&key, move || async move {
            if let Some(bytes) = st2.cache.get(&key2).await {
                return Some(bytes);
            }
            let model = build_koth_scoreboard(&st2, &game2, is_monitor).await.ok()?;
            let json = serde_json::to_vec(&model).ok()?;
            st2.cache.set(&key2, &json, Some(KOTH_CACHE_TTL)).await;
            Some(bytes::Bytes::from(json))
        })
        .await;
    match coalesced {
        Some(bytes) => Ok(bytes),
        None => Err(AppError::internal("KotH scoreboard cache fill failed")),
    }
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
    // A hidden game 404s for everyone (no monitor exemption; the monitor flag only
    // lifts the freeze cutoff, applied inside the board build). 1s-cached game row.
    let game = super::load_game_cached(&st, game_id).await?;
    if game.hidden {
        return Err(AppError::not_found("Game not found"));
    }
    let is_monitor = maybe.as_ref().is_some_and(|u| u.is_monitor());
    let json = koth_scoreboard_json(&st, &game, is_monitor).await?;
    Ok(([(header::CONTENT_TYPE, "application/json")], json).into_response())
}

// ---------------------------------------------------------------------------
// Handlers — write
// ---------------------------------------------------------------------------
