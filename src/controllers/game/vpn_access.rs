//! Player enrollment and tunnel-only proof endpoints for event VPN access.

use axum::extract::{ConnectInfo, Path, State};
use axum::http::{header, HeaderMap};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

use super::*;

const EVENT_VPN_MINT_CONCURRENCY: usize = 8;
static EVENT_VPN_MINT_SLOTS: LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| {
        std::sync::Arc::new(tokio::sync::Semaphore::new(EVENT_VPN_MINT_CONCURRENCY))
    });
static CHALLENGE_MINTS: LazyLock<
    crate::utils::single_flight::SingleFlight<Option<Result<EventVpnChallengeModel, MintFailure>>>,
> = LazyLock::new(crate::utils::single_flight::SingleFlight::new);
static PROOF_MINTS: LazyLock<
    crate::utils::single_flight::SingleFlight<Option<Result<EventVpnProofModel, MintFailure>>>,
> = LazyLock::new(crate::utils::single_flight::SingleFlight::new);

#[derive(Clone, Debug)]
enum MintFailure {
    BadRequest(String),
    Unauthorized,
    Forbidden,
    NotFound(String),
    Conflict(String),
    TooManyRequests(u64),
    Unavailable(String, u64),
    Internal,
}

impl MintFailure {
    fn freeze(error: AppError) -> Self {
        match error {
            AppError::BadRequest(message) | AppError::Validation(message) => {
                Self::BadRequest(message)
            }
            AppError::Unauthorized => Self::Unauthorized,
            AppError::Forbidden => Self::Forbidden,
            AppError::NotFound(message) => Self::NotFound(message),
            AppError::Conflict(message) => Self::Conflict(message),
            AppError::TooManyRequests { retry_after } => {
                Self::TooManyRequests(retry_after.unwrap_or(1).max(1))
            }
            AppError::ServiceUnavailable(message) => Self::Unavailable(message, 1),
            AppError::RetryableUnavailable { title, retry_after } => {
                Self::Unavailable(title, retry_after.max(1))
            }
            error => {
                tracing::error!(%error, "Event VPN proof mint failed");
                Self::Internal
            }
        }
    }

    fn thaw(self) -> AppError {
        match self {
            Self::BadRequest(message) => AppError::bad_request(message),
            Self::Unauthorized => AppError::Unauthorized,
            Self::Forbidden => AppError::Forbidden,
            Self::NotFound(message) => AppError::not_found(message),
            Self::Conflict(message) => AppError::conflict(message),
            Self::TooManyRequests(retry_after) => AppError::too_many_requests(retry_after),
            Self::Unavailable(message, retry_after) => {
                AppError::retryable_unavailable(message, retry_after)
            }
            Self::Internal => AppError::internal("Event VPN proof mint failed"),
        }
    }
}

fn mint_lock_key(kind: &str, user: &CurrentUser, game_id: i32, discriminator: &str) -> String {
    format!(
        "event-vpn-mint:{kind}:{}:{game_id}:{}:{discriminator}",
        user.id,
        crate::services::event_security::stamp_hash(&user.security_stamp)
    )
}

fn mint_permit() -> AppResult<tokio::sync::OwnedSemaphorePermit> {
    EVENT_VPN_MINT_SLOTS
        .clone()
        .try_acquire_owned()
        .map_err(|_| AppError::overloaded("Event VPN verification is busy", 1))
}

async fn mint_lock(st: &SharedState, key: &str) -> AppResult<crate::utils::single_flight::PgAdvisoryLock> {
    crate::utils::single_flight::PgAdvisoryLock::try_acquire(st.pg(), key)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::too_many_requests(1))
}

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
    let key = mint_lock_key("challenge", &user, game_id, "live");
    let st = st.clone();
    let flight_key = key.clone();
    let result = CHALLENGE_MINTS
        .run(&key, move || async move {
            Some(
                mint_challenge(st, user, game_id, &flight_key)
                    .await
                    .map_err(MintFailure::freeze),
            )
        })
        .await
        .ok_or_else(|| AppError::overloaded("Event VPN verification timed out", 1))?
        .map_err(MintFailure::thaw)?;
    Ok(RequestResponse::ok(result))
}

async fn mint_challenge(
    st: SharedState,
    user: CurrentUser,
    game_id: i32,
    lock_key: &str,
) -> AppResult<EventVpnChallengeModel> {
    let _permit = mint_permit()?;
    let lock = mint_lock(&st, lock_key).await?;
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
    let model = EventVpnChallengeModel {
        challenge,
        proof_url: crate::services::event_security::proof_url(game_id)?,
        proof_header: crate::services::event_security::VPN_PROOF_HEADER,
        expires_at_utc,
    };
    lock.release().await.map_err(AppError::from)?;
    Ok(model)
}

pub async fn vpn_proof(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(game_id): Path<i32>,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    Json(request): Json<EventVpnProofRequest>,
) -> AppResult<RequestResponse<EventVpnProofModel>> {
    let source = crate::services::anti_cheat::client_ip(&headers, Some(peer.ip()))
        .and_then(|value| value.parse::<std::net::Ipv4Addr>().ok())
        .ok_or(AppError::Forbidden)?;
    let discriminator = format!(
        "{}:{}",
        source,
        crate::services::event_security::stamp_hash(&request.challenge)
    );
    let key = mint_lock_key("proof", &user, game_id, &discriminator);
    let st = st.clone();
    let flight_key = key.clone();
    let result = PROOF_MINTS
        .run(&key, move || async move {
            Some(
                mint_proof(st, user, game_id, source, request, &flight_key)
                    .await
                    .map_err(MintFailure::freeze),
            )
        })
        .await
        .ok_or_else(|| AppError::overloaded("Event VPN verification timed out", 1))?
        .map_err(MintFailure::thaw)?;
    Ok(RequestResponse::ok(result))
}

async fn mint_proof(
    st: SharedState,
    user: CurrentUser,
    game_id: i32,
    source: std::net::Ipv4Addr,
    request: EventVpnProofRequest,
    lock_key: &str,
) -> AppResult<EventVpnProofModel> {
    let _permit = mint_permit()?;
    let lock = mint_lock(&st, lock_key).await?;
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
    let source = crate::services::event_security::verified_peer_source(st.pg(), game_id, source)
        .await?
        .ok_or(AppError::Forbidden)?;
    if source.user_id != user.id
        || source.participation_id != part.id
        || source.policy_revision != policy.revision
        || source.security_stamp != user.security_stamp
    {
        return Err(AppError::Forbidden);
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
    let model = EventVpnProofModel {
        proof,
        proof_header: crate::services::event_security::VPN_PROOF_HEADER,
        expires_at_utc,
    };
    lock.release().await.map_err(AppError::from)?;
    Ok(model)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_failures_preserve_auth_and_retry_boundaries() {
        assert!(matches!(
            MintFailure::freeze(AppError::Unauthorized).thaw(),
            AppError::Unauthorized
        ));
        assert!(matches!(
            MintFailure::freeze(AppError::Forbidden).thaw(),
            AppError::Forbidden
        ));
        assert!(matches!(
            MintFailure::freeze(AppError::too_many_requests(7)).thaw(),
            AppError::TooManyRequests {
                retry_after: Some(7)
            }
        ));
    }

    #[test]
    fn mint_keys_bind_session_without_exposing_the_security_stamp() {
        let user = CurrentUser {
            id: uuid::Uuid::nil(),
            role: crate::utils::enums::Role::User,
            name: "player".to_string(),
            security_stamp: "do-not-expose-this-stamp".to_string(),
        };
        let key = mint_lock_key("proof", &user, 19, "source-and-challenge-digest");
        assert!(key.starts_with("event-vpn-mint:proof:"));
        assert!(key.contains(":19:"));
        assert!(key.contains("source-and-challenge-digest"));
        assert!(!key.contains(&user.security_stamp));
    }

    #[test]
    fn process_mint_concurrency_remains_bounded() {
        assert_eq!(EVENT_VPN_MINT_CONCURRENCY, 8);
        let permits = (0..EVENT_VPN_MINT_CONCURRENCY)
            .map(|_| mint_permit().expect("configured mint permit"))
            .collect::<Vec<_>>();
        assert!(matches!(mint_permit(), Err(AppError::RetryableUnavailable { .. })));
        drop(permits);
        assert!(mint_permit().is_ok());
    }
}
