//! Player enrollment and tunnel-only proof endpoints for event VPN access.

use axum::extract::{ConnectInfo, Path, State};
use axum::http::{header, HeaderMap};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::*;

#[derive(Debug, Serialize)]
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventVpnProofModel {
    pub proof: String,
    pub proof_header: &'static str,
    #[serde(with = "crate::utils::datetime::millis")]
    pub expires_at_utc: DateTime<Utc>,
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

pub async fn vpn_proof(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(game_id): Path<i32>,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    Json(request): Json<EventVpnProofRequest>,
) -> AppResult<RequestResponse<EventVpnProofModel>> {
    let claims = crate::services::event_security::verify_challenge(
        &st.config.event_vpn_credential_key,
        &request.challenge,
    )?;
    if claims.game_id != game_id
        || claims.user_id != user.id
        || claims.security_stamp_hash
            != crate::services::event_security::stamp_hash(&user.security_stamp)
    {
        return Err(AppError::Unauthorized);
    }
    let part = accepted_participation(&st, &user, game_id).await?;
    if claims.participation_id != part.id {
        return Err(AppError::Unauthorized);
    }
    let policy = crate::services::event_security::load_policy(&st, game_id).await?;
    if !policy.gate_active_at(Utc::now()) {
        return Err(AppError::bad_request(
            "The event VPN gate is not currently active",
        ));
    }
    let source = crate::services::anti_cheat::client_ip(&headers, Some(peer.ip()))
        .and_then(|value| value.parse::<std::net::Ipv4Addr>().ok())
        .ok_or(AppError::Unauthorized)?;
    let source = crate::services::event_security::verified_peer_source(st.pg(), game_id, source)
        .await?
        .ok_or(AppError::Unauthorized)?;
    if source.user_id != user.id
        || source.participation_id != part.id
        || source.policy_revision != policy.revision
        || source.security_stamp != user.security_stamp
    {
        return Err(AppError::Unauthorized);
    }
    let (proof, proof_claims) = crate::services::event_security::issue_proof(
        &st.config.event_vpn_credential_key,
        user.id,
        game_id,
        part.id,
        source.peer_id,
        source.generation,
        policy.revision,
        claims.security_stamp_hash,
    )?;
    let expires_at_utc = DateTime::from_timestamp(proof_claims.expires_at, 0)
        .ok_or_else(|| AppError::internal("invalid VPN proof expiry"))?;
    Ok(RequestResponse::ok(EventVpnProofModel {
        proof,
        proof_header: crate::services::event_security::VPN_PROOF_HEADER,
        expires_at_utc,
    }))
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
