use super::*;

/// Establish a durable deny-new-play marker and prove that hard deletion cannot
/// erase competition history. The caller owns the per-game control lock.
/// Updating the game row before taking every challenge JFLG fence matches
/// submit's lock order: an in-flight submit either commits first and becomes
/// visible to the evidence query, or observes the committed end marker and
/// cannot publish afterward. A committed marker is also the authorization for
/// teardown to finish after the scheduled start instant; retries still repeat
/// every evidence check.
pub(super) async fn fence_game_for_deletion(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    game_id: i32,
) -> AppResult<()> {
    let already_pending = sqlx::query_scalar::<_, bool>(
        r#"SELECT deletion_pending
              FROM "Games"
             WHERE id = $1
             FOR UPDATE"#,
    )
    .bind(game_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Game not found"))?;
    let fenced = sqlx::query(
        r#"UPDATE "Games"
              SET end_time_utc = LEAST(
                      end_time_utc,
                      clock_timestamp() - interval '1 microsecond'
                  ),
                  practice_mode = FALSE,
                  hidden = TRUE,
                  deletion_pending = TRUE
            WHERE id = $1"#,
    )
    .bind(game_id)
    .execute(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if fenced.rows_affected() != 1 {
        return Err(AppError::not_found("Game not found"));
    }

    let challenge_ids = sqlx::query_scalar::<_, i32>(
        r#"SELECT id FROM "GameChallenges" WHERE game_id = $1 ORDER BY id"#,
    )
    .bind(game_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    for challenge_id in challenge_ids {
        crate::utils::scoring::lock_jeopardy_flags_exclusive(tx, challenge_id).await?;
    }

    let participation_ids: Vec<i32> = sqlx::query_scalar(
        r#"SELECT id
              FROM "Participations"
             WHERE game_id = $1
             ORDER BY id
             FOR UPDATE"#,
    )
    .bind(game_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let mut protected = sqlx::query_scalar::<_, bool>(
        r#"SELECT (NOT $2 AND game.start_time_utc <= clock_timestamp())
                  OR game.ad_scoring_start_round IS NOT NULL
                  OR game.koth_scoring_start_round IS NOT NULL
                  OR EXISTS (
                        SELECT 1 FROM "GameChallenges" challenge
                         WHERE challenge.game_id = game.id
                           AND (challenge.accepted_count <> 0
                                OR challenge.submission_count <> 0)
                  )
                  OR EXISTS (
                        SELECT 1 FROM "Submissions" submission
                         WHERE submission.game_id = game.id
                  )
                  OR EXISTS (
                        SELECT 1
                          FROM "FirstSolves" first_solve
                          JOIN "GameChallenges" challenge
                            ON challenge.id = first_solve.challenge_id
                         WHERE challenge.game_id = game.id
                  )
                  OR EXISTS (SELECT 1 FROM "AdRounds" round WHERE round.game_id = game.id)
                  OR EXISTS (SELECT 1 FROM "AdEpochRollups" rollup WHERE rollup.game_id = game.id)
                  OR EXISTS (SELECT 1 FROM "KothCrownCycles" cycle WHERE cycle.game_id = game.id)
                  OR EXISTS (SELECT 1 FROM "KothEpochRollups" rollup WHERE rollup.game_id = game.id)
                  OR EXISTS (SELECT 1 FROM "KothAcquisitions" acquisition WHERE acquisition.game_id = game.id)
                  OR EXISTS (SELECT 1 FROM "KothControlResults" result WHERE result.game_id = game.id)
                  OR EXISTS (SELECT 1 FROM "SuspicionEvents" event WHERE event.game_id = game.id)
                  OR EXISTS (SELECT 1 FROM "HoneypotHits" hit WHERE hit.game_id = game.id)
                  OR EXISTS (SELECT 1 FROM "ContainerAccessEvents" event WHERE event.game_id = game.id)
                  OR EXISTS (SELECT 1 FROM "FlagEgressEvents" event WHERE event.game_id = game.id)
                  OR EXISTS (
                        SELECT 1 FROM "TrafficCaptureFailures" failure
                         WHERE failure.challenge_id IN (
                                   SELECT id FROM "GameChallenges" WHERE game_id = game.id
                               )
                            OR failure.participation_id IN (
                                   SELECT id FROM "Participations" WHERE game_id = game.id
                               )
                  )
             FROM "Games" game
            WHERE game.id = $1"#,
    )
    .bind(game_id)
    .bind(already_pending)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Game not found"))?;
    if !protected {
        for participation_id in participation_ids {
            if crate::services::participation_evidence::has_competition_evidence(
                tx,
                participation_id,
            )
            .await?
            {
                protected = true;
                break;
            }
        }
    }
    if protected {
        return Err(AppError::bad_request(
            "A game cannot be permanently deleted after it has started or recorded competition evidence. Hide it to retain event history.",
        ));
    }
    Ok(())
}

/// Delete the A&D evidence owned by one game while the caller holds the
/// game-control transaction. The database cascades are the durable backstop;
/// spelling out the graph here also supports databases upgrading from the
/// original entity-derived schema, which had no A&D ownership foreign keys.
pub(super) async fn delete_ad_game_data(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    game_id: i32,
) -> AppResult<()> {
    sqlx::query(
        r#"DELETE FROM "AdFlagDeliveryResults"
            WHERE round_id IN (SELECT id FROM "AdRounds" WHERE game_id = $1)
               OR team_service_id IN (
                    SELECT id FROM "AdTeamServices" WHERE game_id = $1
                  )"#,
    )
    .bind(game_id)
    .execute(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"DELETE FROM "AdAttacks" attack
            WHERE attack.round_id IN (SELECT id FROM "AdRounds" WHERE game_id = $1)
               OR attack.victim_team_service_id IN (
                    SELECT id FROM "AdTeamServices" WHERE game_id = $1
                  )
               OR attack.flag_id IN (
                    SELECT flag.id FROM "AdFlags" flag
                    JOIN "AdRounds" round ON round.id = flag.round_id
                    WHERE round.game_id = $1
                  )
               OR attack.attacker_participation_id IN (
                    SELECT id FROM "Participations" WHERE game_id = $1
                  )"#,
    )
    .bind(game_id)
    .execute(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"DELETE FROM "AdCheckResults"
            WHERE round_id IN (SELECT id FROM "AdRounds" WHERE game_id = $1)
               OR team_service_id IN (
                    SELECT id FROM "AdTeamServices" WHERE game_id = $1
                  )"#,
    )
    .bind(game_id)
    .execute(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"DELETE FROM "AdFlags"
            WHERE round_id IN (SELECT id FROM "AdRounds" WHERE game_id = $1)
               OR team_service_id IN (
                    SELECT id FROM "AdTeamServices" WHERE game_id = $1
                  )"#,
    )
    .bind(game_id)
    .execute(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    for statement in [
        r#"DELETE FROM "AdEpochServiceRollups" WHERE game_id = $1"#,
        r#"DELETE FROM "AdEpochTeamRollups" WHERE game_id = $1"#,
        r#"DELETE FROM "AdEpochRollups" WHERE game_id = $1"#,
    ] {
        sqlx::query(statement)
            .bind(game_id)
            .execute(&mut **tx)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    }
    sqlx::query(
        r#"DELETE FROM "AdTeamApiTokens"
            WHERE participation_id IN (
              SELECT id FROM "Participations" WHERE game_id = $1
            )"#,
    )
    .bind(game_id)
    .execute(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"DELETE FROM "AdSshKeys"
            WHERE participation_id IN (
              SELECT id FROM "Participations" WHERE game_id = $1
            )"#,
    )
    .bind(game_id)
    .execute(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(r#"DELETE FROM "AdVpnPeers" WHERE game_id = $1"#)
        .bind(game_id)
        .execute(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(r#"DELETE FROM "AdTeamServices" WHERE game_id = $1"#)
        .bind(game_id)
        .execute(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    // KotH evidence shares the A&D round clock. Remove the token-dependent row
    // first because KothAcquisitions deliberately RESTRICTS token deletion;
    // relying on PostgreSQL's order for two sibling round cascades is unsafe.
    sqlx::query(r#"DELETE FROM "KothAcquisitions" WHERE game_id = $1"#)
        .bind(game_id)
        .execute(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(r#"DELETE FROM "KothControlResults" WHERE game_id = $1"#)
        .bind(game_id)
        .execute(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"DELETE FROM "KothTokens"
            WHERE ad_round_id IN (SELECT id FROM "AdRounds" WHERE game_id = $1)
               OR participation_id IN (
                    SELECT id FROM "Participations" WHERE game_id = $1
                  )"#,
    )
    .bind(game_id)
    .execute(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(r#"DELETE FROM "AdRounds" WHERE game_id = $1"#)
        .bind(game_id)
        .execute(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}
