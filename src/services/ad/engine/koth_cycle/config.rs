use sqlx::{Postgres, Transaction};

use crate::utils::enums::{
    ChallengeBuildStatus, ChallengeReviewStatus, ChallengeType, ParticipationStatus,
};
use crate::utils::error::{AppError, AppResult};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CrownShapeError {
    Epoch,
    Cycle,
    ChampionCooldown,
    ClaimConfirmation,
}

pub(crate) fn validate_crown_shape(
    epoch_ticks: i32,
    cycle_ticks: i32,
    cooldown_ticks: i32,
    confirmation_ticks: i32,
) -> Result<(), CrownShapeError> {
    if !(2..=64).contains(&epoch_ticks) {
        return Err(CrownShapeError::Epoch);
    }
    if !(1..=epoch_ticks / 2).contains(&cycle_ticks) || epoch_ticks % cycle_ticks != 0 {
        return Err(CrownShapeError::Cycle);
    }
    if !(0..cycle_ticks).contains(&cooldown_ticks) {
        return Err(CrownShapeError::ChampionCooldown);
    }
    if !(1..=cycle_ticks).contains(&confirmation_ticks) {
        return Err(CrownShapeError::ClaimConfirmation);
    }
    Ok(())
}

pub(crate) fn valid_crown_shape(
    epoch_ticks: i32,
    cycle_ticks: i32,
    cooldown_ticks: i32,
    confirmation_ticks: i32,
) -> bool {
    validate_crown_shape(epoch_ticks, cycle_ticks, cooldown_ticks, confirmation_ticks).is_ok()
}

/// Freeze crown-cycle configuration, roster, enabled hills, service weights,
/// images, and the official boundary in the transaction opening scoring.
pub(crate) async fn snapshot_official_config(
    transaction: &mut Transaction<'static, Postgres>,
    game_id: i32,
    scoring_start_round: i32,
) -> AppResult<()> {
    let unsnapshotted_cooldown: Option<i32> = sqlx::query_scalar(
        r#"SELECT game.koth_champion_cooldown_ticks
             FROM "Games" game
            WHERE game.id = $1
              AND NOT EXISTS (
                    SELECT 1 FROM "KothOfficialConfigs" config
                     WHERE config.game_id = game.id
              )"#,
    )
    .bind(game_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if unsnapshotted_cooldown.is_some_and(|ticks| ticks > 0) && !crate::services::ad_vpn::enabled()
    {
        return Err(AppError::bad_request(
            "KotH champion cooldown requires the managed A&D VPN; enable it or configure zero cooldown ticks before scoring starts",
        ));
    }
    let exists = sqlx::query_scalar::<_, bool>(
        r#"WITH inserted AS (
           INSERT INTO "KothOfficialConfigs"
             (game_id, scoring_start_round, epoch_ticks,
              cycle_ticks, champion_cooldown_ticks, claim_confirmation_ticks,
              roster_snapshot, hills_snapshot)
           SELECT game.id, $2,
                  game.koth_epoch_ticks, game.koth_cycle_ticks,
                  game.koth_champion_cooldown_ticks,
                  game.koth_claim_confirmation_ticks,
                  COALESCE((
                    SELECT jsonb_agg(participation.id ORDER BY participation.id)
                      FROM "Participations" participation
                     WHERE participation.game_id = game.id
                       AND participation.status = $3
                  ), '[]'::jsonb),
                  COALESCE((
                    SELECT jsonb_agg(jsonb_build_object(
                             'challengeId', challenge.id,
                             'serviceWeight', LEAST(1.2, GREATEST(0.8, challenge.ad_scoring_weight)),
                             'image', challenge.build_image_digest
                           ) ORDER BY challenge.id)
                      FROM "GameChallenges" challenge
                      JOIN "KothTargets" target
                        ON target.game_id = challenge.game_id
                       AND target.challenge_id = challenge.id
                     WHERE challenge.game_id = game.id
                       AND challenge.is_enabled = TRUE
                       AND challenge.review_status = $4
                       AND challenge."Type" = $5
                       AND challenge.build_status = $6
                       AND NULLIF(BTRIM(challenge.build_image_digest), '') IS NOT NULL
                       AND NULLIF(BTRIM(target.container_id), '') IS NOT NULL
                  ), '[]'::jsonb)
             FROM "Games" game
            WHERE game.id = $1
              AND game.koth_scoring_start_round = $2
              AND game.koth_epoch_ticks BETWEEN 2 AND 64
              AND game.koth_cycle_ticks BETWEEN 1 AND game.koth_epoch_ticks / 2
              AND MOD(game.koth_epoch_ticks, game.koth_cycle_ticks) = 0
              AND game.koth_champion_cooldown_ticks BETWEEN 0 AND game.koth_cycle_ticks - 1
              AND game.koth_claim_confirmation_ticks BETWEEN 1 AND game.koth_cycle_ticks
              AND (SELECT COUNT(*) FROM "Participations" participation
                    WHERE participation.game_id = game.id
                      AND participation.status = $3) >= 2
              AND NOT EXISTS (
                    SELECT 1 FROM "GameChallenges" challenge
                    LEFT JOIN "KothTargets" target
                      ON target.game_id = challenge.game_id
                     AND target.challenge_id = challenge.id
                   WHERE challenge.game_id = game.id
                     AND challenge.is_enabled = TRUE
                     AND challenge.review_status = $4
                     AND challenge."Type" = $5
                     AND (challenge.build_status <> $6
                          OR NULLIF(BTRIM(challenge.build_image_digest), '') IS NULL
                          OR NULLIF(BTRIM(target.container_id), '') IS NULL)
              )
           ON CONFLICT (game_id) DO NOTHING
           RETURNING 1
           )
           SELECT EXISTS(SELECT 1 FROM inserted)
               OR EXISTS(SELECT 1 FROM "KothOfficialConfigs"
                          WHERE game_id = $1 AND scoring_start_round = $2)"#,
    )
    .bind(game_id)
    .bind(scoring_start_round)
    .bind(ParticipationStatus::Accepted as i16)
    .bind(ChallengeReviewStatus::Active as i16)
    .bind(ChallengeType::KingOfTheHill as i16)
    .bind(ChallengeBuildStatus::Success as i16)
    .fetch_one(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !exists {
        return Err(AppError::bad_request(
            "KotH crown scoring requires a complete platform-hosted roster and valid cycle configuration",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crown_shape_enforces_divisibility_and_bounded_rules() {
        assert!(valid_crown_shape(12, 3, 1, 2));
        assert!(!valid_crown_shape(12, 5, 1, 2));
        assert!(!valid_crown_shape(12, 12, 1, 2));
        assert!(!valid_crown_shape(12, 3, 3, 2));
        assert!(!valid_crown_shape(12, 3, 1, 4));
    }

    #[test]
    fn crown_shape_reports_the_first_invalid_field() {
        assert_eq!(
            validate_crown_shape(1, 1, 0, 1),
            Err(CrownShapeError::Epoch)
        );
        assert_eq!(
            validate_crown_shape(12, 5, 1, 2),
            Err(CrownShapeError::Cycle)
        );
        assert_eq!(
            validate_crown_shape(12, 3, 3, 2),
            Err(CrownShapeError::ChampionCooldown)
        );
        assert_eq!(
            validate_crown_shape(12, 3, 1, 4),
            Err(CrownShapeError::ClaimConfirmation)
        );
    }
}
