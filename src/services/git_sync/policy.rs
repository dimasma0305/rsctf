use chrono::{DateTime, Utc};
use sea_orm::Set;
use uuid::Uuid;

use crate::models::data::game_challenge;
use crate::utils::enums::ChallengeReviewStatus;
use crate::utils::error::{AppError, AppResult};

use super::ChallengeYaml;

/// Whether an import may run executable preparation while ingesting its manifest.
/// User submissions carry their durable submitter identity and remain inert until
/// approval; trusted manager/repository imports preserve the inline flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportPolicy {
    PendingReview { submitted_by_user_id: Uuid },
    Trusted,
}

pub(super) const MAX_PENDING_MANIFEST_BYTES: u64 = 1024 * 1024;
pub(super) const MAX_PENDING_STATIC_FLAGS: usize = 64;
pub(super) const MAX_PENDING_HINTS: usize = 64;
const MAX_PENDING_FLAG_BYTES: usize = 4 * 1024;
const MAX_PENDING_HINT_BYTES: usize = 16 * 1024;

impl ImportPolicy {
    pub(super) fn review_status(self) -> ChallengeReviewStatus {
        match self {
            Self::PendingReview { .. } => ChallengeReviewStatus::Pending,
            Self::Trusted => ChallengeReviewStatus::Active,
        }
    }

    pub(super) fn reviewed_at(self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        matches!(self, Self::Trusted).then_some(now)
    }

    pub(super) fn may_execute(self) -> bool {
        matches!(self, Self::Trusted)
    }

    pub(super) fn submitted_by_user_id(self) -> Option<Uuid> {
        match self {
            Self::PendingReview {
                submitted_by_user_id,
            } => Some(submitted_by_user_id),
            Self::Trusted => None,
        }
    }

    pub(super) fn is_pending(self) -> bool {
        matches!(self, Self::PendingReview { .. })
    }
}

pub(super) fn validate_pending_manifest(model: &ChallengeYaml) -> AppResult<()> {
    if model
        .flags
        .as_ref()
        .is_some_and(|flags| flags.len() > MAX_PENDING_STATIC_FLAGS)
    {
        return Err(AppError::bad_request(format!(
            "user-submitted manifests may define at most {MAX_PENDING_STATIC_FLAGS} flags"
        )));
    }
    if model
        .flags
        .as_ref()
        .is_some_and(|flags| flags.iter().any(|flag| flag.len() > MAX_PENDING_FLAG_BYTES))
    {
        return Err(AppError::bad_request(format!(
            "user-submitted flags may be at most {MAX_PENDING_FLAG_BYTES} bytes"
        )));
    }
    if model
        .hints
        .as_ref()
        .is_some_and(|hints| hints.len() > MAX_PENDING_HINTS)
    {
        return Err(AppError::bad_request(format!(
            "user-submitted manifests may define at most {MAX_PENDING_HINTS} hints"
        )));
    }
    if model
        .hints
        .as_ref()
        .is_some_and(|hints| hints.iter().any(|hint| hint.len() > MAX_PENDING_HINT_BYTES))
    {
        return Err(AppError::bad_request(format!(
            "user-submitted hints may be at most {MAX_PENDING_HINT_BYTES} bytes"
        )));
    }
    Ok(())
}

pub(super) fn initialize_new_import_review(
    challenge: &mut game_challenge::ActiveModel,
    policy: ImportPolicy,
    now: DateTime<Utc>,
) {
    challenge.is_enabled = Set(false);
    challenge.accepted_count = Set(0);
    challenge.submission_count = Set(0);
    // Establish review state and attribution at INSERT time. An untrusted row
    // must never be transiently active or detached from its submitter.
    challenge.review_status = Set(policy.review_status());
    challenge.reviewed_at_utc = Set(policy.reviewed_at(now));
    challenge.submitted_at_utc = Set(Some(now));
    challenge.submitted_by_user_id = Set(policy.submitted_by_user_id());
}
