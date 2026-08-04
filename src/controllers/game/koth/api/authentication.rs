//! Public token exchange used by a Leaderboard challenge coordinator.

use axum::extract::{Json, State};
use serde::{Deserialize, Serialize};

use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::RequestResponse;

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
    /// the signed referee can map arena evidence without learning RSCTF IDs.
    team_id: String,
    team_name: String,
}

pub async fn authenticate_capability(
    State(st): State<SharedState>,
    Json(request): Json<KothCapabilityAuthenticationRequest>,
) -> AppResult<RequestResponse<KothCapabilityIdentityModel>> {
    let token = request.token.trim();
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

    Ok(RequestResponse::ok(KothCapabilityIdentityModel {
        team_id: crate::services::ad::koth_api_capability::token_hash_hex(token),
        team_name: identity.team_name,
    }))
}

#[cfg(test)]
mod tests {
    use super::KothCapabilityAuthenticationRequest;

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
}
