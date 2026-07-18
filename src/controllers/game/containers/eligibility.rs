use sea_orm::EntityTrait;

use crate::app_state::SharedState;
use crate::models::data::{game_challenge, game_challenge::Entity as GameChallenge};
use crate::utils::enums::{
    ChallengeBuildStatus, ChallengeReviewStatus, ChallengeType, GamePermission,
    ParticipationStatus, Role,
};
use crate::utils::error::{AppError, AppResult};

use super::uses_shared_container;

#[derive(Clone, Copy)]
pub(super) enum ContainerRequestMode {
    PerTeam,
    Shared,
}

/// Re-check every mutable authorization input while the matching lifecycle lock is
/// held. The normal play-context helpers intentionally use short-lived caches; those
/// caches are unsuitable for a create/delete exclusion boundary because an operator
/// can reject a participation, disable a challenge, or change its container mode while
/// a request waits for the lock or for the backend runtime.
pub(super) async fn player_container_request_is_eligible(
    st: &SharedState,
    user_id: uuid::Uuid,
    game_id: i32,
    participation_id: i32,
    challenge_id: i32,
    mode: ContainerRequestMode,
) -> AppResult<bool> {
    sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS(
               SELECT 1
                 FROM "Participations" participation
                 JOIN "UserParticipations" link
                   ON link.participation_id = participation.id
                  AND link.game_id = participation.game_id
                  AND link.team_id = participation.team_id
                 JOIN "AspNetUsers" account ON account.id = link.user_id
                 JOIN "Games" game ON game.id = participation.game_id
                 JOIN "GameChallenges" challenge
                   ON challenge.game_id = game.id
                  AND challenge.id = $5
            LEFT JOIN "Divisions" division
                   ON division.id = participation.division_id
                  AND division.game_id = game.id
            LEFT JOIN "DivisionChallengeConfigs" permission
                   ON permission.division_id = participation.division_id
                  AND permission.challenge_id = challenge.id
                WHERE link.user_id = $1
                  AND game.id = $2
                  AND participation.id = $3
                  AND participation.status = $6
                  AND account.role <> $7
                  AND game.start_time_utc <= CURRENT_TIMESTAMP
                  AND (game.practice_mode OR game.end_time_utc >= CURRENT_TIMESTAMP)
                  AND challenge.is_enabled
                  AND challenge.review_status = $8
                  AND (challenge.workload_spec IS NOT NULL OR (
                       challenge.build_status = $15
                       AND NULLIF(BTRIM(challenge.build_image_digest), '') IS NOT NULL))
                  AND (
                        participation.division_id IS NULL
                        OR (COALESCE(permission.permissions, division.default_permissions, $9) & $10) = $10
                  )
                  AND (
                       ($4 AND
                            game.end_time_utc >= CURRENT_TIMESTAMP
                        AND challenge."Type" = $11
                        AND challenge.enable_shared_container
                        AND (challenge.workload_spec IS NOT NULL OR (
                             COALESCE(challenge.container_image, '') <> ''
                             AND challenge.expose_port IS NOT NULL)))
                       OR
                       (NOT $4 AND (
                            (challenge."Type" IN ($11, $12)
                             AND NOT (
                                  challenge."Type" = $11
                              AND challenge.enable_shared_container
                              AND (challenge.workload_spec IS NOT NULL OR (
                                   COALESCE(challenge.container_image, '') <> ''
                                   AND challenge.expose_port IS NOT NULL))))
                            OR
                            (challenge."Type" IN ($13, $14)
                             AND game.practice_mode
                             AND game.end_time_utc < CURRENT_TIMESTAMP
                             AND COALESCE(challenge.container_image, '') <> ''
                             AND challenge.expose_port IS NOT NULL)
                       ))
                  )
           )"#,
    )
    .bind(user_id)
    .bind(game_id)
    .bind(participation_id)
    .bind(matches!(mode, ContainerRequestMode::Shared))
    .bind(challenge_id)
    .bind(ParticipationStatus::Accepted as i16)
    .bind(Role::Banned as i16)
    .bind(ChallengeReviewStatus::Active as i16)
    .bind(GamePermission::ALL)
    .bind(GamePermission::VIEW_CHALLENGE)
    .bind(ChallengeType::StaticContainer as i16)
    .bind(ChallengeType::DynamicContainer as i16)
    .bind(ChallengeType::AttackDefense as i16)
    .bind(ChallengeType::KingOfTheHill as i16)
    .bind(ChallengeBuildStatus::Success as i16)
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

fn is_shared_container_mode(challenge: &game_challenge::Model) -> bool {
    uses_shared_container(challenge) || challenge.challenge_type == ChallengeType::KingOfTheHill
}

pub(super) async fn load_eligible_shared_challenge(
    st: &SharedState,
    challenge_id: i32,
) -> AppResult<game_challenge::Model> {
    // The provisioning caller needs the complete enum-rich entity to build its
    // ContainerSpec; duplicating that hydration in a large sqlx tuple is more brittle.
    let challenge = GameChallenge::find_by_id(challenge_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Challenge not found"))?;
    let game_is_live = sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS(
               SELECT 1
                 FROM "Games"
                WHERE id = $1 AND end_time_utc >= CURRENT_TIMESTAMP
           )"#,
    )
    .bind(challenge.game_id)
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !game_is_live
        || !challenge.is_enabled
        || challenge.review_status != ChallengeReviewStatus::Active
        || !is_shared_container_mode(&challenge)
    {
        return Err(AppError::bad_request(
            "Shared container provisioning is no longer allowed",
        ));
    }
    Ok(challenge)
}
