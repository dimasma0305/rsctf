//! Live KotH capability reads and their roster-revocation fence.

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};

use super::require_live_hill;
use crate::app_state::SharedState;
use crate::controllers::game::ad::resolve_participation;
use crate::middlewares::privilege_authentication::{CurrentUser, MaybeUser};
use crate::utils::enums::{ChallengeReviewStatus, ChallengeType};
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::RequestResponse;

/// The cycle-scoped capability a team plants into one exact hill.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct KothTokenModel {
    pub round: i32,
    pub token: Option<String>,
    /// `"warmup"` (no round yet) | `"no-cycle-token"` | `"ready"`.
    pub status: String,
}

enum KothTokenCaller {
    Session(uuid::Uuid),
    TeamToken(String),
}

async fn acquire_koth_token_read_fence(
    st: &SharedState,
    team_id: i32,
) -> AppResult<crate::utils::single_flight::PgAdvisoryLock> {
    let key = format!("team-roster:{team_id}");
    crate::utils::single_flight::PgAdvisoryLock::try_acquire_shared(st.pg(), &key)
        .await?
        .ok_or_else(|| AppError::unavailable("Team credentials are changing; retry this request"))
}

async fn koth_token_caller_is_live(
    connection: &mut sqlx::PgConnection,
    caller: &KothTokenCaller,
    part: &crate::models::data::participation::Model,
) -> AppResult<bool> {
    match caller {
        KothTokenCaller::Session(user_id) => {
            crate::services::ad::roster::user_allows_shared_credentials_on(
                connection,
                *user_id,
                part.game_id,
                part.team_id,
                part.id,
            )
            .await
        }
        KothTokenCaller::TeamToken(token) => {
            let verified =
                crate::services::ad::api_token::authenticate_on(connection, token).await?;
            Ok(verified.is_some_and(|credential| {
                credential.participation.id == part.id
                    && credential.participation.game_id == part.game_id
                    && credential.participation.team_id == part.team_id
            }))
        }
    }
}

pub(super) fn koth_token_cache_key(
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

/// The caller team's active-cycle capability for one hill.
pub async fn koth_hill_token(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, challenge_id)): Path<(i32, i32)>,
) -> AppResult<RequestResponse<KothTokenModel>> {
    let part = resolve_participation(&st, &user, id).await?;
    require_live_hill(&st, id, challenge_id).await?;

    let latest_round = load_latest_round_cached(&st, id).await?;
    // Decode the cache before retaining a PostgreSQL connection. The value is
    // untrusted until the shared roster fence and live caller check below.
    let token_key = koth_token_cache_key(id, challenge_id, part.id, latest_round);
    let cached_model = match st.cache.get(&token_key).await {
        Some(bytes) => serde_json::from_slice::<KothTokenModel>(&bytes).ok(),
        None => None,
    };
    let caller = KothTokenCaller::Session(user.id);
    let mut roster = acquire_koth_token_read_fence(&st, part.team_id).await?;
    if !koth_token_caller_is_live(roster.transaction_mut(), &caller, &part).await? {
        roster.release().await?;
        return Err(AppError::Forbidden);
    }
    if let Some(model) = cached_model {
        roster.release().await?;
        return Ok(RequestResponse::ok(model));
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
        .fetch_optional(&mut **roster.transaction_mut())
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
        // Set while the read fence is retained. A waiting revoker therefore
        // evicts this value after it becomes visible, never before.
        st.cache
            .set(&token_key, &json, Some(std::time::Duration::from_secs(10)))
            .await;
    }
    roster.release().await?;
    Ok(RequestResponse::ok(model))
}

/// One enabled hill's cycle-scoped capability for the caller's team.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct KothHillTokenModel {
    pub challenge_id: i32,
    pub token: String,
}

/// The caller team's active control token for every enabled KotH hill.
pub async fn koth_token_all(
    State(st): State<SharedState>,
    maybe_user: MaybeUser,
    Path(id): Path<i32>,
    headers: HeaderMap,
    verified: Option<axum::Extension<crate::services::ad::api_token::VerifiedTeamToken>>,
    rejected: Option<axum::Extension<crate::services::ad::api_token::RejectedTeamToken>>,
) -> AppResult<RequestResponse<Vec<KothHillTokenModel>>> {
    let session_user_id = maybe_user.0.as_ref().map(|user| user.id);
    let presented_team_token = crate::services::ad::api_token::bearer_token(&headers)
        .filter(|token| crate::services::ad::api_token::is_well_formed(token))
        .map(str::to_owned);
    let token_auth_selected = verified.is_some() || presented_team_token.is_some();
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
    if latest_round == 0 {
        return Ok(RequestResponse::ok(Vec::new()));
    }

    let caller = if token_auth_selected {
        KothTokenCaller::TeamToken(presented_team_token.ok_or(AppError::Unauthorized)?)
    } else {
        KothTokenCaller::Session(session_user_id.ok_or(AppError::Unauthorized)?)
    };
    let cache_key = format!("kothtokensall:{id}:{}:{latest_round}", part.id);
    let cached_model = match st.cache.get(&cache_key).await {
        Some(bytes) => serde_json::from_slice::<Vec<KothHillTokenModel>>(&bytes).ok(),
        None => None,
    };
    let mut roster = acquire_koth_token_read_fence(&st, part.team_id).await?;
    if !koth_token_caller_is_live(roster.transaction_mut(), &caller, &part).await? {
        roster.release().await?;
        return Err(AppError::Unauthorized);
    }
    if let Some(model) = cached_model {
        roster.release().await?;
        return Ok(RequestResponse::ok(model));
    }

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
    .fetch_all(&mut **roster.transaction_mut())
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
    roster.release().await?;
    Ok(RequestResponse::ok(out))
}
