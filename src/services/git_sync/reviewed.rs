//! Preparation of immutable artifacts from a pending challenge's reviewed source.

use std::path::Path;

use crate::app_state::SharedState;
use crate::models::data::game_challenge;
use crate::utils::error::{AppError, AppResult};

/// Prepare or validate the process checker from the immutable source archive.
/// A prior failed approval may already have published a valid revision while the
/// challenge remained inert; that revision is safe to reuse on retry.
pub(crate) async fn prepare_checker(
    st: &SharedState,
    challenge: &game_challenge::Model,
) -> AppResult<Option<String>> {
    if let Some(path) = challenge
        .ad_checker_image
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        super::checker::validate_prepared_checker_revision(
            Path::new(&st.config.storage_root),
            challenge.game_id,
            path,
        )
        .await?;
        return Ok(Some(path.to_string()));
    }
    let hash = challenge
        .original_archive_blob_path
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AppError::bad_request("Challenge source archive is unavailable."))?;
    let archive = st
        .storage
        .load_bounded(
            hash,
            crate::utils::upload::SOURCE_ARCHIVE_BLOB_BYTES,
        )
        .await
        .map_err(|error| {
        tracing::warn!(%error, challenge = challenge.id, %hash, "review checker source load failed");
        AppError::bad_request("Challenge source archive is unavailable.")
    })?;
    super::checker::prepare_checker_from_archive(
        Path::new(&st.config.storage_root),
        challenge.game_id,
        &challenge.title,
        archive,
    )
    .await
}
