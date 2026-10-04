//! Side-effect-free challenge-update policy projections.

use super::*;

/// Whether a challenge is in shared-container mode (RSCTF
/// `GameChallenge.UsesSharedContainer`).
pub(super) fn uses_shared_container(challenge: &game_challenge::Model) -> bool {
    challenge.challenge_type == ChallengeType::StaticContainer
        && challenge.enable_shared_container
        && crate::services::challenge_workloads::has_runtime(challenge)
}

pub(super) fn scoring_fields_changed(
    model: &ChallengeUpdateModel,
    challenge: &game_challenge::Model,
) -> bool {
    let deadline_changed = model.deadline_utc.is_some_and(|deadline| {
        let requested = (deadline.timestamp() != 0).then_some(deadline);
        requested != challenge.deadline_utc
    });
    let flag_template_changed = model.flag_template.as_ref().is_some_and(|template| {
        let requested =
            (!crate::utils::flag_policy::is_blank(template)).then_some(template.as_str());
        requested != challenge.flag_template.as_deref()
    });
    deadline_changed
        || flag_template_changed
        || model
            .submission_limit
            .is_some_and(|value| value != challenge.submission_limit)
        || model
            .original_score
            .is_some_and(|value| value != challenge.original_score)
        || model
            .min_score_rate
            .is_some_and(|value| value != challenge.min_score_rate)
        || model
            .difficulty
            .is_some_and(|value| value != challenge.difficulty)
        || model
            .score_curve
            .is_some_and(|value| value != challenge.score_curve)
        || model
            .disable_blood_bonus
            .is_some_and(|value| value != challenge.disable_blood_bonus)
        || model
            .ad_scoring_weight
            .is_some_and(|value| (value - challenge.ad_scoring_weight).abs() > f64::EPSILON)
}
