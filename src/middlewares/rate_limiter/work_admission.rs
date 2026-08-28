//! Direct admission checks for work that is not expressed as route middleware.

use axum::response::Response;

use super::{check_async, check_weighted_async, too_many_requests, Policy};

/// Silent source admission for decoy routes. Multi-replica deployments use
/// the configured Redis limiter; Redis failure falls back to the bounded local
/// policy without changing the plausible honeypot response.
pub(crate) async fn admit_honeypot_source(source: &str, tcp: bool) -> bool {
    let policy = if tcp {
        Policy::HoneypotTcp
    } else {
        Policy::HoneypotHttp
    };
    if check_async(policy, source.to_owned()).await.is_err() {
        return false;
    }
    check_async(Policy::HoneypotAggregate, "telemetry".to_owned())
        .await
        .is_ok()
}

/// Asset downloads bypass the `/api` middleware, so enforce their source,
/// account, and deployment-wide request budgets before cache, SQL, or storage.
pub(crate) async fn admit_asset_request(
    source: &str,
    user_id: Option<uuid::Uuid>,
) -> Result<(), u64> {
    check_async(Policy::AssetRequestSource, source.to_owned()).await?;
    if let Some(user_id) = user_id {
        check_async(Policy::AssetRequestIdentity, user_id.to_string()).await?;
    }
    check_async(
        Policy::AssetRequestWork,
        "asset-download-deployment".to_string(),
    )
    .await
}

/// Charge only actual authorization-cache misses. Same-hash single flight
/// coalesces SQL; rotating hashes cannot bypass this shared budget.
pub(crate) async fn admit_asset_gate_miss() -> Result<(), u64> {
    check_async(
        Policy::AssetGateMiss,
        "asset-gate-miss-deployment".to_string(),
    )
    .await
}

/// Charge response bytes before opening storage. One unit is 64 KiB.
pub(crate) async fn admit_asset_response_bytes(bytes: u64) -> Result<(), u64> {
    if bytes == 0 {
        return Ok(());
    }
    let units = bytes.div_ceil(64 * 1024);
    let cost = u32::try_from(units).unwrap_or(u32::MAX);
    check_weighted_async(
        Policy::AssetResponseBytes,
        "asset-response-bytes-deployment".to_string(),
        cost,
    )
    .await
}

/// Enforce the team-scoped A&D lookup/byte work budget after authentication.
pub(crate) async fn admit_ad_submit(
    game_id: i32,
    participation_id: i32,
    work_units: usize,
) -> Option<Response> {
    let cost = u32::try_from(work_units.max(1)).unwrap_or(u32::MAX);
    let key = format!("game:{game_id}:participation:{participation_id}");
    check_weighted_async(Policy::AdSubmit, key, cost)
        .await
        .err()
        .map(too_many_requests)
}
