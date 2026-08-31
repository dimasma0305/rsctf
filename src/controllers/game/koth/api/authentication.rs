//! Public token exchange used by a Leaderboard challenge coordinator.

use axum::extract::{Json, State};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use tokio::sync::{Semaphore, SemaphorePermit};

use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::RequestResponse;

/// Keep invalid capability floods from occupying the shared PostgreSQL pool.
/// Sixteen concurrent lookups absorb the measured short tail of the maintained
/// 100-authentication/second arena profile while still leaving eighteen
/// connections for scoring, reporter, and operator work in the default
/// 34-connection pool. A lower ceiling rejected valid roster capabilities when
/// eight slow lookups overlapped even though sustained source admission passed.
const DATABASE_LOOKUP_CONCURRENCY: usize = 16;
static DATABASE_LOOKUP_SLOTS: Semaphore = Semaphore::const_new(DATABASE_LOOKUP_CONCURRENCY);

fn try_database_lookup_slot() -> Option<SemaphorePermit<'static>> {
    DATABASE_LOOKUP_SLOTS.try_acquire().ok()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KothCapabilityAuthenticationRequest {
    token: String,
    game_id: i32,
    challenge_id: i32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KothCapabilityIdentityModel {
    /// Challenge-local pseudonym. It is deliberately the capability digest so
    /// the managed reporter can map arena evidence without learning RSCTF IDs.
    team_id: String,
    team_name: String,
}

fn validated_token(raw: &str) -> Option<&str> {
    let token = raw.trim();
    crate::services::ad::koth_api_capability::is_well_formed(token).then_some(token)
}

pub async fn authenticate_capability(
    State(st): State<SharedState>,
    Json(request): Json<KothCapabilityAuthenticationRequest>,
) -> AppResult<Response> {
    // Reject ambiguous and attacker-sized values before acquiring PostgreSQL.
    // The service repeats this check as defense in depth for non-HTTP callers.
    let token = validated_token(&request.token).ok_or(AppError::Unauthorized)?;
    let Some(database_slot) = try_database_lookup_slot() else {
        return Ok(crate::middlewares::rate_limiter::too_many_requests(1));
    };
    let mut connection = st
        .pg()
        .acquire()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let identity = crate::services::ad::koth_api_capability::authenticate(
        &mut connection,
        token,
        request.game_id,
        request.challenge_id,
    )
    .await?
    .ok_or(AppError::Unauthorized)?;
    drop(connection);
    drop(database_slot);

    if let Some(response) = crate::middlewares::rate_limiter::admit_koth_capability_auth(
        identity.game_id,
        identity.challenge_id,
        identity.participation_id,
    )
    .await
    {
        return Ok(response);
    }

    Ok(RequestResponse::ok(KothCapabilityIdentityModel {
        team_id: crate::services::ad::koth_api_capability::token_hash_hex(token),
        team_name: identity.team_name,
    })
    .into_response())
}

#[cfg(test)]
mod tests {
    use axum::http::{header, StatusCode};

    use super::{validated_token, KothCapabilityAuthenticationRequest};

    #[test]
    fn scope_and_token_are_all_required_and_unknown_fields_fail() {
        let valid = serde_json::from_str::<KothCapabilityAuthenticationRequest>(
            r#"{"token":"koth_12345678","gameId":7,"challengeId":9}"#,
        );
        assert!(valid.is_ok());
        assert!(serde_json::from_str::<KothCapabilityAuthenticationRequest>(
            r#"{"token":"koth_12345678","gameId":7}"#
        )
        .is_err());
        assert!(serde_json::from_str::<KothCapabilityAuthenticationRequest>(
            r#"{"token":"koth_12345678","gameId":7,"challengeId":9,"teamId":1}"#
        )
        .is_err());
    }

    #[test]
    fn token_shape_is_normalized_before_database_authentication() {
        assert_eq!(
            validated_token("  koth_exampleToken-123456  "),
            Some("koth_exampleToken-123456")
        );
        assert_eq!(validated_token("koth_short"), None);
        assert_eq!(validated_token(&format!("koth_{}", "a".repeat(129))), None);
    }

    #[test]
    fn database_lookup_admission_is_bounded_and_retryable() {
        let permits = (0..super::DATABASE_LOOKUP_CONCURRENCY)
            .map(|_| super::try_database_lookup_slot().expect("configured lookup slot"))
            .collect::<Vec<_>>();
        assert!(super::try_database_lookup_slot().is_none());

        let response = crate::middlewares::rate_limiter::too_many_requests(1);
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers().get(header::RETRY_AFTER).unwrap(), "1");

        drop(permits);
        assert!(super::try_database_lookup_slot().is_some());
    }
}
