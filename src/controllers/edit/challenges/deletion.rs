use sqlx::{Executor, Postgres, Transaction};

use crate::utils::enums::ChallengeType;
use crate::utils::error::{AppError, AppResult};
use crate::utils::single_flight::PgAdvisoryLock;

type DeletionState = (i16, bool, bool, i32, i32, bool, bool, bool);

const PENDING_MUTATION_SQL: &str = r#"SELECT challenge.deletion_pending, game.deletion_pending
         FROM "GameChallenges" challenge
         JOIN "Games" game ON game.id = challenge.game_id
        WHERE challenge.id = $1 AND challenge.game_id = $2
        FOR SHARE OF challenge, game"#;

fn validate_pending_mutation(state: Option<(bool, bool)>) -> AppResult<()> {
    match state {
        None => Err(AppError::not_found("Challenge not found")),
        Some((true, _)) | Some((_, true)) => {
            Err(AppError::conflict("Challenge or game is being deleted"))
        }
        Some((false, false)) => Ok(()),
    }
}

pub(super) async fn acquire_definition_lock(
    pool: &sqlx::PgPool,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<PgAdvisoryLock> {
    crate::services::challenge_workloads::acquire_definition_lock(pool, game_id, challenge_id).await
}

/// Refuse every definition/review mutation once either the challenge or its
/// parent game has committed a durable hard-delete fence. A caller that passes
/// a retained transaction also keeps both rows stable until that transaction
/// commits; pool callers get the same snapshot check without retaining a row
/// lock across slow or separately-transactional work.
pub(crate) async fn reject_pending_mutation<'e, E>(
    executor: E,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<()>
where
    E: Executor<'e, Database = Postgres>,
{
    let state = sqlx::query_as::<_, (bool, bool)>(PENDING_MUTATION_SQL)
        .bind(challenge_id)
        .bind(game_id)
        .fetch_optional(executor)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    validate_pending_mutation(state)
}

/// Atomically deny new work and protect immutable Jeopardy scoring history.
///
/// The caller holds the game-control and definition advisory fences. The JFLG
/// lock linearizes this predicate with an in-flight flag submission: either the
/// submission commits first and makes deletion ineligible, or disabling commits
/// first and the submit path's final policy update rolls its transaction back.
/// The durable pending bit lets teardown finish after the scheduled start time
/// while every retry continues to reject newly committed evidence.
pub(super) async fn fence_challenge_deletion(
    transaction: &mut Transaction<'_, Postgres>,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<()> {
    let game_deletion_pending = sqlx::query_scalar::<_, bool>(
        r#"SELECT deletion_pending FROM "Games" WHERE id = $1 FOR SHARE"#,
    )
    .bind(game_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    match game_deletion_pending {
        None => return Err(AppError::not_found("Game not found")),
        Some(true) => return Err(AppError::conflict("Game is being deleted")),
        Some(false) => {}
    }
    crate::utils::scoring::lock_jeopardy_flags_exclusive(transaction, challenge_id).await?;

    let state: Option<DeletionState> = sqlx::query_as(
        r#"SELECT challenge."Type",
                  game.start_time_utc <= clock_timestamp() AS has_started,
                  challenge.deletion_pending,
                  challenge.accepted_count,
                  challenge.submission_count,
                  EXISTS (
                      SELECT 1 FROM "Submissions" submission
                       WHERE submission.challenge_id = challenge.id
                  ) AS has_submission,
                  EXISTS (
                      SELECT 1 FROM "FirstSolves" first_solve
                       WHERE first_solve.challenge_id = challenge.id
                  ) AS has_first_solve,
                  game.ad_scoring_start_round IS NOT NULL
                    OR game.koth_scoring_start_round IS NOT NULL AS engine_scoring_started
             FROM "GameChallenges" challenge
             JOIN "Games" game ON game.id = challenge.game_id
            WHERE challenge.id = $1 AND challenge.game_id = $2
            FOR UPDATE OF challenge"#,
    )
    .bind(challenge_id)
    .bind(game_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((
        challenge_type,
        has_started,
        deletion_pending,
        accepted_count,
        submission_count,
        has_submission,
        has_first_solve,
        engine_scoring_started,
    )) = state
    else {
        return Err(AppError::not_found("Challenge not found"));
    };

    let is_jeopardy = matches!(
        challenge_type,
        value if value == ChallengeType::StaticAttachment as i16
            || value == ChallengeType::StaticContainer as i16
            || value == ChallengeType::DynamicAttachment as i16
            || value == ChallengeType::DynamicContainer as i16
    );
    let is_live_engine = challenge_type == ChallengeType::AttackDefense as i16
        || challenge_type == ChallengeType::KingOfTheHill as i16;
    if !is_jeopardy && !is_live_engine {
        return Err(AppError::internal("Challenge has an invalid type"));
    }

    // Attributed security writers lock game -> challenge -> participation in
    // this same order. These row locks make the following evidence predicate a
    // final, race-free decision rather than an early best-effort snapshot.
    let _participation_ids: Vec<i32> = sqlx::query_scalar(
        r#"SELECT id
              FROM "Participations"
             WHERE game_id = $1
             ORDER BY id
             FOR UPDATE"#,
    )
    .bind(game_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let has_durable_evidence = sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS (
              SELECT 1 FROM "Submissions" WHERE challenge_id = $1
              UNION ALL
              SELECT 1 FROM "FirstSolves" WHERE challenge_id = $1
              UNION ALL
              SELECT 1 FROM "SuspicionEvents" WHERE challenge_id = $1
              UNION ALL
              SELECT 1 FROM "ContainerAccessEvents" WHERE challenge_id = $1
              UNION ALL
              SELECT 1 FROM "FlagEgressEvents" WHERE challenge_id = $1
              UNION ALL
              SELECT 1 FROM "TrafficCaptureFailures" WHERE challenge_id = $1
              UNION ALL
              SELECT 1
                FROM "AdFlags" flag
                JOIN "AdTeamServices" service ON service.id = flag.team_service_id
               WHERE service.challenge_id = $1
              UNION ALL
              SELECT 1
                FROM "AdCheckResults" result
                JOIN "AdTeamServices" service ON service.id = result.team_service_id
               WHERE service.challenge_id = $1
              UNION ALL
              SELECT 1
                FROM "AdAttacks" attack
                JOIN "AdTeamServices" service ON service.id = attack.victim_team_service_id
               WHERE service.challenge_id = $1
              UNION ALL
              SELECT 1
                FROM "AdFlagDeliveryResults" delivery
                JOIN "AdTeamServices" service ON service.id = delivery.team_service_id
               WHERE service.challenge_id = $1
              UNION ALL
              SELECT 1 FROM "AdEpochServiceRollups" WHERE challenge_id = $1
              UNION ALL
              SELECT 1 FROM "KothTokens" WHERE challenge_id = $1
              UNION ALL
              SELECT 1 FROM "KothControlResults" WHERE challenge_id = $1
              UNION ALL
              SELECT 1 FROM "KothCrownCycles" WHERE challenge_id = $1
              UNION ALL
              SELECT 1
                FROM "KothCycleCooldowns" cooldown
                JOIN "KothCrownCycles" cycle ON cycle.id = cooldown.cycle_id
               WHERE cycle.challenge_id = $1
              UNION ALL
              SELECT 1
                FROM "KothCycleAuditReceipts" receipt
                JOIN "KothCrownCycles" cycle ON cycle.id = receipt.cycle_id
               WHERE cycle.challenge_id = $1
              UNION ALL
              SELECT 1
                FROM "KothClaimStates" claim
                JOIN "KothTargets" target ON target.id = claim.target_id
               WHERE target.challenge_id = $1
              UNION ALL
              SELECT 1 FROM "KothAcquisitions" WHERE challenge_id = $1
              UNION ALL
              SELECT 1 FROM "KothEpochHillRollups" WHERE challenge_id = $1
        )"#,
    )
    .bind(challenge_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    if is_jeopardy
        && ((!deletion_pending && has_started)
            || accepted_count != 0
            || submission_count != 0
            || has_submission
            || has_first_solve
            || has_durable_evidence)
    {
        return Err(AppError::bad_request(
            "Jeopardy challenges cannot be deleted after play has started or scoring history exists. Disable the challenge instead.",
        ));
    }
    if is_live_engine && (engine_scoring_started || has_durable_evidence) {
        return Err(AppError::bad_request(
            "A&D/KotH challenges cannot be deleted after scoring or durable gameplay/security evidence exists. Disable the challenge instead.",
        ));
    }

    sqlx::query(
        r#"UPDATE "GameChallenges"
              SET is_enabled = FALSE,
                  deletion_pending = TRUE
            WHERE id = $1 AND game_id = $2"#,
    )
    .bind(challenge_id)
    .bind(game_id)
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod pending_mutation_tests {
    use super::validate_pending_mutation;

    #[test]
    fn rejects_challenge_and_parent_game_deletion_fences() {
        assert!(validate_pending_mutation(Some((false, false))).is_ok());
        for state in [Some((true, false)), Some((false, true)), Some((true, true))] {
            assert_eq!(
                validate_pending_mutation(state).unwrap_err().status(),
                axum::http::StatusCode::CONFLICT
            );
        }
        assert_eq!(
            validate_pending_mutation(None).unwrap_err().status(),
            axum::http::StatusCode::NOT_FOUND
        );
    }
}
