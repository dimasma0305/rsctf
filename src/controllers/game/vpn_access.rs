//! Player enrollment and tunnel-only proof endpoints for event VPN access.

use std::sync::LazyLock;

use axum::extract::{ConnectInfo, Path, State};
use axum::http::{header, HeaderMap};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::*;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventVpnChallengeModel {
    pub challenge: String,
    pub proof_url: String,
    pub proof_header: &'static str,
    #[serde(with = "crate::utils::datetime::millis")]
    pub expires_at_utc: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventVpnProofRequest {
    pub challenge: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventVpnProofModel {
    pub proof: String,
    pub proof_header: &'static str,
    #[serde(with = "crate::utils::datetime::millis")]
    pub expires_at_utc: DateTime<Utc>,
}

static VPN_PROOF_MINT_FLIGHT: LazyLock<
    crate::utils::single_flight::SingleFlight<Option<EventVpnProofModel>>,
> = LazyLock::new(crate::utils::single_flight::SingleFlight::new);

pub(super) enum VpnProofEndpointError {
    Application(AppError),
    VpnProof,
}

impl From<AppError> for VpnProofEndpointError {
    fn from(error: AppError) -> Self {
        Self::Application(error)
    }
}

impl IntoResponse for VpnProofEndpointError {
    fn into_response(self) -> Response {
        match self {
            Self::Application(error) => error.into_response(),
            Self::VpnProof => crate::middlewares::event_vpn::unauthorized_response(),
        }
    }
}

fn proof_mint_flight_key(
    user_id: uuid::Uuid,
    game_id: i32,
    participation_id: i32,
    peer_id: uuid::Uuid,
    peer_generation: i32,
    policy_revision: i64,
    security_stamp_hash: &str,
) -> String {
    format!(
        "{user_id}:{game_id}:{participation_id}:{peer_id}:{peer_generation}:{policy_revision}:{security_stamp_hash}"
    )
}

async fn accepted_participation(
    st: &SharedState,
    user: &CurrentUser,
    game_id: i32,
) -> AppResult<participation::Model> {
    let part = super::ad::resolve_participation(st, user, game_id).await?;
    if part.status != ParticipationStatus::Accepted {
        return Err(AppError::bad_request("Participation not accepted"));
    }
    Ok(part)
}

pub async fn vpn_challenge(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(game_id): Path<i32>,
) -> AppResult<RequestResponse<EventVpnChallengeModel>> {
    let policy = crate::services::event_security::load_policy(&st, game_id).await?;
    if !policy.access_required {
        return Err(AppError::bad_request(
            "This event does not require VPN access",
        ));
    }
    let part = accepted_participation(&st, &user, game_id).await?;
    let (challenge, claims) = crate::services::event_security::issue_challenge(
        &st.config.event_vpn_credential_key,
        user.id,
        game_id,
        part.id,
        &user.security_stamp,
    )?;
    let expires_at_utc = DateTime::from_timestamp(claims.expires_at, 0)
        .ok_or_else(|| AppError::internal("invalid VPN challenge expiry"))?;
    Ok(RequestResponse::ok(EventVpnChallengeModel {
        challenge,
        proof_url: crate::services::event_security::proof_url(game_id)?,
        proof_header: crate::services::event_security::VPN_PROOF_HEADER,
        expires_at_utc,
    }))
}

pub(super) async fn vpn_proof(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(game_id): Path<i32>,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    Json(request): Json<EventVpnProofRequest>,
) -> Result<RequestResponse<EventVpnProofModel>, VpnProofEndpointError> {
    let claims = crate::services::event_security::verify_challenge(
        &st.config.event_vpn_credential_key,
        &request.challenge,
    )
    .map_err(|_| VpnProofEndpointError::VpnProof)?;
    if claims.game_id != game_id
        || claims.user_id != user.id
        || claims.security_stamp_hash
            != crate::services::event_security::stamp_hash(&user.security_stamp)
    {
        return Err(VpnProofEndpointError::VpnProof);
    }
    let part = accepted_participation(&st, &user, game_id).await?;
    if claims.participation_id != part.id {
        return Err(VpnProofEndpointError::VpnProof);
    }
    let policy = crate::services::event_security::load_policy(&st, game_id).await?;
    if !policy.gate_active_at(Utc::now()) {
        return Err(AppError::bad_request("The event VPN gate is not currently active").into());
    }
    let source = crate::services::anti_cheat::client_ip(&headers, Some(peer.ip()))
        .and_then(|value| value.parse::<std::net::Ipv4Addr>().ok())
        .ok_or(VpnProofEndpointError::VpnProof)?;
    let source = crate::services::event_security::verified_peer_source(st.pg(), game_id, source)
        .await?
        .ok_or(VpnProofEndpointError::VpnProof)?;
    if source.user_id != user.id
        || source.participation_id != part.id
        || source.policy_revision != policy.revision
        || source.security_stamp != user.security_stamp
    {
        return Err(VpnProofEndpointError::VpnProof);
    }
    let flight_key = proof_mint_flight_key(
        user.id,
        game_id,
        part.id,
        source.peer_id,
        source.generation,
        policy.revision,
        &claims.security_stamp_hash,
    );
    let credential_key = st.config.event_vpn_credential_key.clone();
    let user_id = user.id;
    let participation_id = part.id;
    let peer_id = source.peer_id;
    let peer_generation = source.generation;
    let policy_revision = policy.revision;
    let security_stamp_hash = claims.security_stamp_hash;
    let model = VPN_PROOF_MINT_FLIGHT
        .run(&flight_key, move || async move {
            let (proof, proof_claims) = crate::services::event_security::issue_proof(
                &credential_key,
                user_id,
                game_id,
                participation_id,
                peer_id,
                peer_generation,
                policy_revision,
                security_stamp_hash,
            )
            .ok()?;
            let expires_at_utc = DateTime::from_timestamp(proof_claims.expires_at, 0)?;
            Some(EventVpnProofModel {
                proof,
                proof_header: crate::services::event_security::VPN_PROOF_HEADER,
                expires_at_utc,
            })
        })
        .await
        .ok_or_else(|| AppError::internal("issue VPN proof"))?;
    Ok(RequestResponse::ok(model))
}

#[cfg(test)]
mod tests {
    use super::proof_mint_flight_key;
    use uuid::Uuid;

    #[test]
    fn proof_flight_identity_changes_at_every_revocation_boundary() {
        let user = Uuid::new_v4();
        let peer = Uuid::new_v4();
        let baseline = proof_mint_flight_key(user, 7, 9, peer, 2, 11, "stamp-a");
        for changed in [
            proof_mint_flight_key(Uuid::new_v4(), 7, 9, peer, 2, 11, "stamp-a"),
            proof_mint_flight_key(user, 8, 9, peer, 2, 11, "stamp-a"),
            proof_mint_flight_key(user, 7, 10, peer, 2, 11, "stamp-a"),
            proof_mint_flight_key(user, 7, 9, Uuid::new_v4(), 2, 11, "stamp-a"),
            proof_mint_flight_key(user, 7, 9, peer, 3, 11, "stamp-a"),
            proof_mint_flight_key(user, 7, 9, peer, 2, 12, "stamp-a"),
            proof_mint_flight_key(user, 7, 9, peer, 2, 11, "stamp-b"),
        ] {
            assert_ne!(baseline, changed);
        }
    }
}

pub async fn vpn_config(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(game_id): Path<i32>,
) -> AppResult<Response> {
    let policy = crate::services::event_security::load_policy(&st, game_id).await?;
    if !policy.access_required {
        return Err(AppError::bad_request(
            "This event does not require VPN access",
        ));
    }
    let part = accepted_participation(&st, &user, game_id).await?;
    let config = crate::services::event_security::render_user_config(&st, &user, &part).await?;
    let disposition = format!("attachment; filename=rsctf-event-{game_id}.conf");
    Ok((
        [
            (
                header::CONTENT_TYPE,
                "text/plain; charset=utf-8".to_string(),
            ),
            (header::CACHE_CONTROL, "private, no-store".to_string()),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        config,
    )
        .into_response())
}
