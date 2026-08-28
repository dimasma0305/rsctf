//! Global authentication-aware admission, including pre-SQL A&D bearer gates.

use super::*;
use axum::extract::State as AxumState;
use tokio::sync::Semaphore;

pub(super) fn check_authenticated_local(identity: String, ip: String) -> Result<(), u64> {
    check(Policy::Global, identity)?;
    check(Policy::GlobalIpBackstop, ip)
}

async fn check_authenticated_async(identity: String, ip: String) -> Result<(), u64> {
    match DISTRIBUTED.get() {
        Some(distributed) => distributed.check_authenticated(&identity, &ip).await,
        None => check_authenticated_local(identity, ip),
    }
}

pub(super) const AD_AUTH_QUERY_TIMEOUT: Duration = Duration::from_secs(2);
pub(super) const AD_AUTH_CONCURRENCY: usize = 32;
static AD_AUTH_ADMISSION: LazyLock<std::sync::Arc<Semaphore>> =
    LazyLock::new(|| std::sync::Arc::new(Semaphore::new(AD_AUTH_CONCURRENCY)));

pub(super) fn route_supports_ad_bearer(path: &str) -> bool {
    let segments: Vec<_> = path.trim_matches('/').split('/').collect();
    matches!(
        segments.as_slice(),
        ["api", "Game", game_id, "Ad", "Submit" | "Targets"]
            | ["api", "Game", game_id, "Ad", "Koth", "Token" | "Hills"]
            | ["api", "game", game_id, "ad", "targets"]
            if game_id.parse::<i32>().is_ok_and(|id| id > 0)
    )
}

async fn authenticate_ad_bearer(
    st: &SharedState,
    token: &str,
    ip: &str,
) -> Result<Option<crate::services::ad::api_token::VerifiedTeamToken>, Response> {
    if let Err(retry_after) = check_async(Policy::AdBearerSourceAdmission, ip.to_owned()).await {
        return Err(too_many_requests(retry_after));
    }
    let digest = crate::services::ad::api_token::hash(token);
    if let Err(retry_after) =
        check_async(Policy::AdBearerAdmission, format!("presented:{digest}")).await
    {
        return Err(too_many_requests(retry_after));
    }
    // The verified partition is a pure function of the presented token hash.
    // Charge every later rejection quota before acquiring a query permit.
    if let Err(retry_after) = check_authenticated_async(format!("ad:{digest}"), ip.to_owned()).await
    {
        return Err(too_many_requests(retry_after));
    }
    let permit = AD_AUTH_ADMISSION
        .clone()
        .try_acquire_owned()
        .map_err(|_| too_many_requests(1))?;
    let result = tokio::time::timeout(
        AD_AUTH_QUERY_TIMEOUT,
        crate::services::ad::api_token::authenticate(st.pg(), token),
    )
    .await;
    drop(permit);
    match result {
        Ok(Ok(credential)) => Ok(credential),
        Ok(Err(error)) => Err(error.into_response()),
        Err(_) => Err(crate::utils::error::AppError::unavailable(
            "A&D credential verification timed out; retry later",
        )
        .into_response()),
    }
}

/// Authentication-aware global admission layered once over API and asset
/// routes. A&D bearers receive every rate decision before authentication SQL.
pub async fn global_middleware(
    AxumState(st): AxumState<SharedState>,
    mut req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path();
    if !globally_limited_path(path) {
        return next.run(req).await;
    }
    let ip = client_ip(&req);
    let credential = crate::middlewares::privilege_authentication::session_token(req.headers());
    if credential.is_some() {
        if let Err(retry_after) = check_async(Policy::CredentialIpAdmission, ip.clone()).await {
            return too_many_requests(retry_after);
        }
    }

    let attempted_ad = credential
        .as_deref()
        .is_some_and(crate::services::ad::api_token::is_well_formed)
        && route_supports_ad_bearer(req.uri().path());
    let attempted_personal = !attempted_ad
        && credential.as_deref().is_some_and(|token| {
            token.starts_with(crate::controllers::api_token::PERSONAL_TOKEN_PREFIX)
        });
    let verified_ad = if attempted_ad {
        match authenticate_ad_bearer(
            &st,
            credential
                .as_deref()
                .expect("attempted A&D token is present"),
            &ip,
        )
        .await
        {
            Ok(credential) => credential,
            Err(response) => return response,
        }
    } else {
        None
    };
    if attempted_ad && verified_ad.is_none() {
        req.extensions_mut()
            .insert(crate::services::ad::api_token::RejectedTeamToken);
    }
    let verified_personal = if attempted_personal {
        match crate::controllers::api_token::authenticate(
            &st,
            credential
                .as_deref()
                .expect("attempted personal token is present"),
        )
        .await
        {
            Ok(credential) => Some(credential),
            Err(error) => return error.into_response(),
        }
    } else {
        None
    };
    let verified_session = if attempted_ad || attempted_personal {
        None
    } else {
        credential.and_then(|token| st.token.verify(&token).ok())
    };
    if let Some(verified) = verified_ad {
        req.extensions_mut().insert(verified);
        // Every A&D bearer quota was consumed before its authentication query.
    } else if attempted_ad {
        // Preserve the definitive 401 without a post-query limiter decision.
    } else if let Some(verified) = verified_personal {
        let key = verified.partition_key.clone();
        req.extensions_mut().insert(verified);
        if let Err(retry_after) = check_authenticated_async(key, ip).await {
            return too_many_requests(retry_after);
        }
    } else if let Some(claims) = verified_session {
        let key = session_partition_key(&claims);
        req.extensions_mut()
            .insert(crate::middlewares::privilege_authentication::VerifiedSessionClaims(claims));
        req.extensions_mut()
            .insert(VerifiedSessionPartitionKey(key.clone()));
        if let Err(retry_after) = check_authenticated_async(key, ip).await {
            return too_many_requests(retry_after);
        }
    } else if let Err(retry_after) = check_async(Policy::Global, ip).await {
        return too_many_requests(retry_after);
    }
    next.run(req).await
}

pub(super) fn globally_limited_path(path: &str) -> bool {
    path.starts_with("/api") || path.starts_with("/assets/")
}
