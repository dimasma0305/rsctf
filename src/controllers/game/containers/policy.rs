//! Small lifecycle policy checks shared by create, delete, and extend paths.

use super::*;

/// RSCTF `ContainerPolicy.RenewalWindow` — a container may only be extended once it
/// is within this many minutes of its expiry.
pub(super) const CONTAINER_RENEWAL_WINDOW_MINUTES: i64 = 10;

/// Two per-instance container operations inside this window are throttled.
const CONTAINER_OPERATION_COOLDOWN_SECONDS: i64 = 10;

/// Port of RSCTF `GameInstance.IsContainerOperationTooFrequent`.
pub(super) fn container_op_too_frequent(instance: &game_instance::Model) -> Option<AppError> {
    if Utc::now() - instance.last_container_operation
        < chrono::Duration::seconds(CONTAINER_OPERATION_COOLDOWN_SECONDS)
    {
        Some(AppError::Coded {
            http: StatusCode::TOO_MANY_REQUESTS,
            code: 429,
            title: "Container operation too often".into(),
        })
    } else {
        None
    }
}

/// A&D/KotH use the Jeopardy-style per-team lifecycle only for ended practice games.
pub(super) fn allows_practice_container(
    challenge: &game_challenge::Model,
    game: &game::Model,
) -> bool {
    challenge.challenge_type.uses_ad_engine()
        && game.practice_mode
        && Utc::now() > game.end_time_utc
        && challenge
            .container_image
            .as_deref()
            .is_some_and(|image| !image.is_empty())
        && challenge.expose_port.is_some()
}
