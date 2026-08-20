//! Live KotH capability reads and their roster-revocation fence.

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use super::require_live_hill;
use crate::app_state::SharedState;
use crate::controllers::game::ad::resolve_participation;
use crate::middlewares::privilege_authentication::{CurrentUser, MaybeUser};
use crate::utils::enums::{ChallengeReviewStatus, ChallengeType};
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::RequestResponse;

/// The capability a team uses on one exact hill. Marker tokens are scoped to a
/// crown cycle; Leaderboard/API tokens are stable until explicit rotation.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct KothTokenModel {
    pub round: i32,
    pub token: Option<String>,
    /// `"warmup"` (no round yet) | `"no-cycle-token"` | `"ready"`.
    pub status: String,
}

enum KothTokenCaller {
    Session {
        user_id: uuid::Uuid,
        security_stamp: String,
    },
    TeamToken(String),
}

fn no_store_token_response<T: Serialize>(model: T) -> Response {
    let mut response = RequestResponse::ok(model).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
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
        KothTokenCaller::Session {
            user_id,
            security_stamp,
        } => {
            crate::services::ad::roster::user_allows_shared_credentials_on(
                connection,
                *user_id,
                security_stamp,
                part.game_id,
                part.team_id,
                part.id,
            )
            .await
        }
        KothTokenCaller::TeamToken(token) => {
            if !crate::services::ad::roster::lock_team_shared_credentials_on(
                connection,
                part.team_id,
            )
            .await?
            {
                return Ok(false);
            }
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

/// The caller team's current capability for one hill.
pub async fn koth_hill_token(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, challenge_id)): Path<(i32, i32)>,
) -> AppResult<Response> {
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
    let caller = KothTokenCaller::Session {
        user_id: user.id,
        security_stamp: user.security_stamp.clone(),
    };
    let mut roster = acquire_koth_token_read_fence(&st, part.team_id).await?;
    if !koth_token_caller_is_live(roster.transaction_mut(), &caller, &part).await? {
        roster.release().await?;
        return Err(AppError::Forbidden);
    }
    if let Some(model) = cached_model {
        roster.release().await?;
        return Ok(no_store_token_response(model));
    }

    // API arenas take the primary-key fast path. The second value distinguishes
    // a missing token on an already-issued API hill from a Marker hill without
    // reparsing the frozen JSON snapshot on every cache fill.
    let stable: (Option<String>, bool) = sqlx::query_as(
        r#"SELECT
             (SELECT token
                FROM "KothApiTeamTokens"
               WHERE game_id = $1 AND challenge_id = $2
                 AND participation_id = $3),
             EXISTS (
               SELECT 1 FROM "KothApiTeamTokens"
                WHERE game_id = $1 AND challenge_id = $2
             )"#,
    )
    .bind(id)
    .bind(challenge_id)
    .bind(part.id)
    .fetch_one(&mut **roster.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let (token, status) = if let Some(token) = stable.0 {
        (Some(token), "ready".to_string())
    } else if stable.1 {
        (None, "no-cycle-token".to_string())
    } else if latest_round == 0 {
        (None, "warmup".to_string())
    } else {
        let marker: Option<String> = sqlx::query_scalar(
            r#"SELECT capability.token
                 FROM "KothTokens" capability
                 JOIN "KothCrownCycles" cycle ON cycle.id = capability.cycle_id
                 JOIN "KothTargets" target ON target.id = capability.target_id
                WHERE cycle.game_id = $1 AND cycle.challenge_id = $2
                  AND cycle.phase = 'Active'
                  AND target.container_id = cycle.replacement_container_id
                  AND capability.participation_id = $3
                  AND capability.challenge_id = $2
                  AND capability.reset_attempt = cycle.reset_attempt
                  AND capability.revoked_at IS NULL
                ORDER BY cycle.cycle_number DESC LIMIT 1"#,
        )
        .bind(id)
        .bind(challenge_id)
        .bind(part.id)
        .fetch_optional(&mut **roster.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        match marker {
            Some(token) => (Some(token), "ready".to_string()),
            None => (None, "no-cycle-token".to_string()),
        }
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
    Ok(no_store_token_response(model))
}

/// One enabled hill's current capability for the caller's team.
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
) -> AppResult<Response> {
    let session_user_id = maybe_user.0.as_ref().map(|user| user.id);
    let session_security_stamp = maybe_user
        .0
        .as_ref()
        .map(|user| user.security_stamp.clone());
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
        return Ok(no_store_token_response(Vec::<KothHillTokenModel>::new()));
    }

    let caller = if token_auth_selected {
        KothTokenCaller::TeamToken(presented_team_token.ok_or(AppError::Unauthorized)?)
    } else {
        KothTokenCaller::Session {
            user_id: session_user_id.ok_or(AppError::Unauthorized)?,
            security_stamp: session_security_stamp.ok_or(AppError::Unauthorized)?,
        }
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
        return Ok(no_store_token_response(model));
    }

    let out: Vec<KothHillTokenModel> = sqlx::query_as::<_, (i32, String)>(
        r#"WITH frozen_hills AS (
             SELECT (hill->>'challengeId')::integer AS challenge_id,
                    COALESCE(NULLIF(hill->>'claimSource', ''), 'Marker') AS claim_source
               FROM "KothOfficialConfigs" config,
                    LATERAL jsonb_array_elements(config.hills_snapshot) hill
              WHERE config.game_id = $1
           ), enabled_hills AS (
             SELECT challenge.id AS challenge_id, frozen.claim_source
               FROM "GameChallenges" challenge
               JOIN frozen_hills frozen ON frozen.challenge_id = challenge.id
              WHERE challenge.game_id = $1
                AND challenge.is_enabled = TRUE
                AND challenge.review_status = $3
                AND challenge."Type" = $4
           )
           SELECT stable.challenge_id, stable.token
             FROM "KothApiTeamTokens" stable
             JOIN enabled_hills hill
               ON hill.challenge_id = stable.challenge_id
              AND hill.claim_source = 'Api'
            WHERE stable.game_id = $1 AND stable.participation_id = $2
           UNION ALL
           SELECT token.challenge_id, token.token
             FROM "KothTokens" token
             JOIN "KothCrownCycles" cycle ON cycle.id = token.cycle_id
             JOIN "KothTargets" target ON target.id = token.target_id
             JOIN enabled_hills hill
               ON hill.challenge_id = token.challenge_id
              AND hill.claim_source = 'Marker'
            WHERE cycle.game_id = $1 AND cycle.phase = 'Active'
              AND target.container_id = cycle.replacement_container_id
              AND token.reset_attempt = cycle.reset_attempt
              AND token.participation_id = $2 AND token.revoked_at IS NULL
            ORDER BY challenge_id"#,
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
    Ok(no_store_token_response(out))
}

/// Replace one event-scoped Leaderboard capability. The old value and this
/// team's unsettled snapshot row are invalid by the time this returns.
pub async fn rotate_koth_api_token(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, challenge_id)): Path<(i32, i32)>,
) -> AppResult<Response> {
    let part = resolve_participation(&st, &user, id).await?;
    require_live_hill(&st, id, challenge_id).await?;
    let latest_round = load_latest_round_cached(&st, id).await?;
    let mut roster = crate::controllers::game::ad::acquire_roster_access(&st, &user, &part).await?;
    if !crate::services::ad::koth_api_capability::is_api_hill(
        roster.transaction_mut(),
        id,
        challenge_id,
    )
    .await?
    {
        roster.release().await?;
        return Err(AppError::bad_request(
            "Only Leaderboard/API KotH capabilities can be rotated manually",
        ));
    }

    crate::utils::single_flight::acquire_transaction_advisory_lock(
        roster.transaction_mut(),
        &crate::services::ad::engine::game_lock_key(id),
    )
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let token = format!("koth_{}", crate::utils::codec::random_token(18));
    let token: String = sqlx::query_scalar(
        r#"INSERT INTO "KothApiTeamTokens"
               (game_id, challenge_id, participation_id, token)
           VALUES ($1, $2, $3, $4)
           ON CONFLICT (game_id, challenge_id, participation_id) DO UPDATE
             SET token = EXCLUDED.token,
                 generation = "KothApiTeamTokens".generation + 1,
                 rotated_at = clock_timestamp(),
                 last_used_at = NULL
           RETURNING token"#,
    )
    .bind(id)
    .bind(challenge_id)
    .bind(part.id)
    .bind(token)
    .fetch_one(&mut **roster.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    crate::services::ad::koth_api_capability::clear_unsettled_score(
        roster.transaction_mut(),
        id,
        challenge_id,
        part.id,
    )
    .await?;
    roster.release().await?;

    st.cache
        .remove(&koth_token_cache_key(
            id,
            challenge_id,
            part.id,
            latest_round,
        ))
        .await;
    st.cache
        .remove(&format!("kothtokensall:{id}:{}:{latest_round}", part.id))
        .await;
    Ok(no_store_token_response(KothTokenModel {
        round: latest_round,
        token: Some(token),
        status: "ready".to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use axum::http::header;

    use super::{no_store_token_response, KothTokenModel};

    #[test]
    fn plaintext_capability_responses_cannot_be_cached() {
        let response = no_store_token_response(KothTokenModel {
            round: 7,
            token: Some("koth_example_token".to_string()),
            status: "ready".to_string(),
        });
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("private, no-store")
        );
        assert_eq!(
            response
                .headers()
                .get(header::PRAGMA)
                .and_then(|value| value.to_str().ok()),
            Some("no-cache")
        );
    }
}
