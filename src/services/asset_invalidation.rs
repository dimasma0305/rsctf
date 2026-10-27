//! Bounded asset-discovery cache invalidation after owner mutations.

use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const ASSET_GATE_GENERATION_TTL: Duration = Duration::from_secs(300);

pub(crate) fn valid_content_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(crate) fn asset_gate_window() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(crate) fn asset_gate_generation_key(hash: &str) -> String {
    format!("assetgate-generation:{hash}")
}

pub(crate) fn decode_generation(value: Option<&[u8]>) -> String {
    value
        .and_then(|value| std::str::from_utf8(value).ok())
        .filter(|value| value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .unwrap_or("0")
        .to_string()
}

pub(crate) async fn asset_gate_generation(st: &SharedState, hash: &str) -> String {
    let value = st.cache.get(&asset_gate_generation_key(hash)).await;
    decode_generation(value.as_deref())
}

pub(crate) fn asset_gate_cache_key(hash: &str, generation: &str, window: u64) -> String {
    format!("assetgate:{hash}:{generation}:{window:016x}")
}

/// Evict the bounded grant windows touched by an owner publication/revocation.
/// Redis removes the shared entry; the adjacent windows also cap stale L1
/// copies on other replicas at the one-second generation boundary.
pub(crate) async fn invalidate_asset_gate(st: &SharedState, hash: &str) {
    if !valid_content_hash(hash) {
        return;
    }
    let old_generation = asset_gate_generation(st, hash).await;
    let new_generation = Uuid::new_v4().simple().to_string();
    // Publish the new generation before evicting old entries. A cache fill on
    // another replica compares this token both before and after its query/set,
    // so an invalidate-then-late-set race cannot resurrect the stale grant.
    st.cache
        .set(
            &asset_gate_generation_key(hash),
            new_generation.as_bytes(),
            Some(ASSET_GATE_GENERATION_TTL),
        )
        .await;
    let current = asset_gate_window();
    for generation in [&old_generation, &new_generation] {
        for window in [
            current.saturating_sub(1),
            current,
            current.saturating_add(1),
        ] {
            st.cache
                .remove(&asset_gate_cache_key(hash, generation, window))
                .await;
        }
    }
}

/// Delete an attachment row and, for a local attachment, release its blob.
/// Mirrors RSCTF `BlobRepository.DeleteAttachment`. Idempotent when the row is
/// already gone.
pub(crate) async fn delete_attachment(st: &SharedState, attachment_id: i32) -> AppResult<()> {
    let affected_hash = sqlx::query_scalar::<_, String>(
        r#"SELECT file.hash
             FROM "Attachments" attachment
             JOIN "Files" file ON file.id = attachment.local_file_id
            WHERE attachment.id = $1"#,
    )
    .bind(attachment_id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let deleted_hash =
        crate::services::blob_refs::delete_attachment(st.pg(), attachment_id).await?;
    if let Some(hash) = affected_hash.as_deref() {
        invalidate_asset_gate(st, hash).await;
    }
    if let Some(hash) = deleted_hash {
        if let Err(error) =
            crate::services::blob_refs::purge_if_unreferenced(st.pg(), st.storage.as_ref(), &hash)
                .await
        {
            tracing::warn!(%error, %hash, "deleted attachment blob purge failed");
        }
    }
    Ok(())
}
