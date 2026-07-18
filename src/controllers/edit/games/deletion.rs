use super::*;

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
