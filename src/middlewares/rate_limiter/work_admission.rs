//! Direct admission checks for work that is not expressed as route middleware.

use super::{check_async, check_weighted_async, Policy};

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
