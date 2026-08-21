use crate::services::git_sync::ImportPolicy;
use crate::utils::error::{AppError, AppResult};

pub(super) fn validate_import_batch(policy: ImportPolicy, manifest_count: usize) -> AppResult<()> {
    if matches!(policy, ImportPolicy::PendingReview { .. }) && manifest_count != 1 {
        return Err(AppError::bad_request(
            "A user submission must contain exactly one challenge manifest.",
        ));
    }
    Ok(())
}
