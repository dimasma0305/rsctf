//! Preserve extensionless public image URLs, including existing stored images.

use futures::TryStreamExt;

use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};

pub(super) async fn content_type(
    st: &SharedState,
    hash: &str,
    size: u64,
) -> AppResult<Option<&'static str>> {
    if size == 0 {
        return Ok(None);
    }
    // Read the signature independently of the requested download range. Never
    // collect the whole blob or trust a caller-selected filename/MIME type.
    let mut stream = st.storage.stream_range(hash, 0..size.min(32)).await?;
    let mut prefix = Vec::with_capacity(32);
    while let Some(chunk) = stream
        .try_next()
        .await
        .map_err(|_| AppError::not_found("File not found"))?
    {
        if prefix.len() + chunk.len() > 32 {
            return Err(AppError::internal(
                "Storage exceeded the image signature range",
            ));
        }
        prefix.extend_from_slice(&chunk);
    }
    Ok(crate::utils::public_image::content_type(&prefix))
}
