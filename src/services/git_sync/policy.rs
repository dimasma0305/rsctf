use chrono::{DateTime, Utc};
use sea_orm::Set;
use uuid::Uuid;

use crate::models::data::game_challenge;
use crate::utils::enums::{ChallengeReviewStatus, ChallengeType};
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
    for flag in model.flags.as_deref().unwrap_or_default() {
        crate::utils::flag_policy::validate_normal(flag)
            .map_err(|error| AppError::bad_request(error.to_string()))?;
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

pub(super) fn validate_flag_definition(
    challenge_type: ChallengeType,
    flag_template: Option<&str>,
    static_flags: &[String],
) -> AppResult<()> {
    if challenge_type == ChallengeType::DynamicContainer {
        if let Some(template) = flag_template {
            crate::utils::flag_policy::validate_dynamic_template(template)
                .map_err(|error| AppError::bad_request(error.to_string()))?;
        }
    }
    for flag in static_flags {
        crate::utils::flag_policy::validate_normal(flag)
            .map_err(|error| AppError::bad_request(error.to_string()))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_manifests_share_the_normal_submission_byte_policy() {
        let mut manifest = ChallengeYaml {
            flags: Some(vec!["x".repeat(127), "界".repeat(42)]),
            ..Default::default()
        };
        assert!(validate_pending_manifest(&manifest).is_ok());

        manifest.flags = Some(vec!["x".repeat(128)]);
        assert!(validate_pending_manifest(&manifest).is_err());
        manifest.flags = Some(vec!["界".repeat(43)]);
        assert!(validate_pending_manifest(&manifest).is_err());
        manifest.flags = Some(vec![" flag{not-canonical}".to_string()]);
        assert!(validate_pending_manifest(&manifest).is_err());
    }

    #[test]
    fn trusted_imports_share_static_and_expanded_template_boundaries() {
        assert!(validate_flag_definition(
            ChallengeType::StaticAttachment,
            None,
            &["x".repeat(127), "界".repeat(42)],
        )
        .is_ok());
        assert!(validate_flag_definition(
            ChallengeType::StaticAttachment,
            None,
            &["x".repeat(128)],
        )
        .is_err());
        assert!(validate_flag_definition(
            ChallengeType::StaticAttachment,
            None,
            &["flag{not-canonical} ".to_string()],
        )
        .is_err());
        assert!(validate_flag_definition(
            ChallengeType::DynamicContainer,
            Some(&format!("flag{{{}}}", "[UUID]".repeat(4))),
            &[],
        )
        .is_err());
    }
}
