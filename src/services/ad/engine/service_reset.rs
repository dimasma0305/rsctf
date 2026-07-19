//! Identity fencing for platform-hosted A&D service replacement.

use super::*;

const RETIRE_SERVICE_ENDPOINT_SQL: &str = r#"UPDATE "AdTeamServices"
       SET host = '', port = 0, status = $3
     WHERE id = $1 AND game_id = $2"#;

const PUBLISH_SERVICE_ENDPOINT_SQL: &str = r#"UPDATE "AdTeamServices" service
       SET host = $3, port = $4, container_id = $5,
           last_reset_at = clock_timestamp(), status = $8
      FROM "Participations" participation,
           "GameChallenges" challenge, "Games" game
     WHERE service.id = $1
       AND service.game_id = $2
       AND service.host = ''
       AND service.port = 0
       AND service.container_id IS NULL
       AND participation.id = service.participation_id
       AND participation.game_id = service.game_id
       AND participation.status = $6
       AND challenge.id = service.challenge_id
       AND challenge.game_id = service.game_id
       AND challenge.is_enabled = TRUE
       AND challenge.review_status = $7
       AND challenge."Type" = $9
       AND game.id = service.game_id
       AND (
         SELECT latest.id
           FROM "AdRounds" latest
          WHERE latest.game_id = service.game_id
            AND latest.finalized = FALSE
          ORDER BY latest.number DESC, latest.id DESC
          LIMIT 1
       ) IS NOT DISTINCT FROM $11
       AND (
         NOT $10
         OR (
           game.start_time_utc <= clock_timestamp()
           AND clock_timestamp() < game.end_time_utc
         )
       )"#;

fn valid_replacement_endpoint(host: &str, port: i32, container_id: &str) -> bool {
    !host.trim().is_empty() && (1..=65_535).contains(&port) && !container_id.trim().is_empty()
}

pub(crate) struct ServiceResetPreparation {
    /// Exact unfinalized round observed while the old endpoint was fenced.
    /// `None` is a real warmup identity, not a wildcard.
    pub(crate) prepared_round_id: Option<i32>,
    /// The official current-round flag. `None` means the game is still in warmup
    /// and the caller may use the legacy per-team bootstrap flag.
    pub(crate) current_flag: Option<String>,
    /// Backend identity retained on the now-blank service row until the traffic
    /// capture owner acknowledges removal of the old filter.
    pub(crate) retired_container_id: Option<String>,
}

/// Fence the old endpoint before Docker work begins.
///
/// Checker persistence and this mutation share the per-game control lock. If a
/// real verdict committed first it remains evidence; otherwise reset downtime is
/// an explicit zero-credit Offline sample, never a carry-forward InternalError.
pub(crate) async fn prepare_service_reset(
    db: &DatabaseConnection,
    game_id: i32,
    service_id: i32,
    reason: &str,
) -> AppResult<ServiceResetPreparation> {
    let mut control = super::koth_auth::acquire_game_lock(db, game_id).await?;
    let tx = control.transaction_mut();
    let current: Option<(Option<i32>, Option<String>, Option<String>)> = sqlx::query_as(
        r#"SELECT round.id, flag.flag, service.container_id
             FROM "AdTeamServices" service
             JOIN "Participations" participation
               ON participation.id = service.participation_id
              AND participation.game_id = service.game_id
             JOIN "GameChallenges" challenge
               ON challenge.id = service.challenge_id
              AND challenge.game_id = service.game_id
             JOIN "Games" game ON game.id = service.game_id
             LEFT JOIN LATERAL (
               SELECT id
                 FROM "AdRounds"
                WHERE game_id = service.game_id AND finalized = FALSE
                ORDER BY number DESC, id DESC
                LIMIT 1
             ) round ON TRUE
             LEFT JOIN "AdFlags" flag
               ON flag.round_id = round.id
              AND flag.team_service_id = service.id
            WHERE service.id = $1
              AND service.game_id = $2
              AND participation.status = $3
              AND challenge.is_enabled = TRUE
              AND challenge.review_status = $4
              AND challenge."Type" = $5
            FOR UPDATE OF service, participation, challenge, game"#,
    )
    .bind(service_id)
    .bind(game_id)
    .bind(ParticipationStatus::Accepted as i16)
    .bind(ChallengeReviewStatus::Active as i16)
    .bind(ChallengeType::AttackDefense as i16)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((round_id, current_flag, retired_container_id)) = current else {
        return Err(AppError::conflict(
            "A&D service is no longer eligible for replacement",
        ));
    };
    if round_id.is_some() && current_flag.is_none() {
        return Err(AppError::conflict(
            "The current round flag is not prepared; retry after round recovery",
        ));
    }

    if let Some(round_id) = round_id {
        sqlx::query(
            r#"UPDATE "AdCheckResults"
                  SET status = $3,
                      message = $4,
                      checked_at = LEAST(
                        clock_timestamp(),
                        (SELECT end_time_utc FROM "Games" WHERE id = $5)
                      ),
                      sla_credit = 0.0,
                      flag_verified = FALSE
                WHERE round_id = $1
                  AND team_service_id = $2
                  AND sla_credit IS NULL"#,
        )
        .bind(round_id)
        .bind(service_id)
        .bind(AdCheckStatus::Offline as i16)
        .bind(reason)
        .bind(game_id)
        .execute(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }

    let revoked = sqlx::query(RETIRE_SERVICE_ENDPOINT_SQL)
        .bind(service_id)
        .bind(game_id)
        .bind(AdCheckStatus::Offline as i16)
        .execute(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if revoked.rows_affected() != 1 {
        return Err(AppError::conflict(
            "A&D service changed while replacement was starting",
        ));
    }
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(ServiceResetPreparation {
        prepared_round_id: round_id,
        current_flag,
        retired_container_id,
    })
}

/// Publish the new endpoint under the same identity fence used by checker
/// persistence. It must still be the blank placeholder installed above.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn publish_service_reset(
    db: &DatabaseConnection,
    game_id: i32,
    service_id: i32,
    host: &str,
    port: i32,
    container_id: &str,
    prepared_round_id: Option<i32>,
    require_running_game: bool,
) -> AppResult<bool> {
    if !valid_replacement_endpoint(host, port, container_id) {
        return Err(AppError::internal(
            "Container runtime returned an invalid A&D service endpoint",
        ));
    }
    let mut control = super::koth_auth::acquire_game_lock(db, game_id).await?;
    let published = sqlx::query(PUBLISH_SERVICE_ENDPOINT_SQL)
        .bind(service_id)
        .bind(game_id)
        .bind(host)
        .bind(port)
        .bind(container_id)
        .bind(ParticipationStatus::Accepted as i16)
        .bind(ChallengeReviewStatus::Active as i16)
        // A replacement remains Offline until the checker verifies the new identity.
        .bind(AdCheckStatus::Offline as i16)
        .bind(ChallengeType::AttackDefense as i16)
        .bind(require_running_game)
        .bind(prepared_round_id)
        .execute(&mut **control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .rows_affected()
        == 1;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(published)
}

#[cfg(test)]
mod tests {
    use super::{
        valid_replacement_endpoint, PUBLISH_SERVICE_ENDPOINT_SQL, RETIRE_SERVICE_ENDPOINT_SQL,
    };

    fn same_prepared_round(prepared: Option<i32>, latest: Option<i32>) -> bool {
        prepared == latest
    }

    #[test]
    fn replacement_endpoint_must_be_probeable_and_identity_bound() {
        assert!(!valid_replacement_endpoint("", 31337, "container"));
        assert!(!valid_replacement_endpoint("10.13.37.2", 0, "container"));
        assert!(!valid_replacement_endpoint("10.13.37.2", 31337, ""));
        assert!(valid_replacement_endpoint("10.13.37.2", 31337, "container"));
    }

    #[test]
    fn replacement_requires_exact_round_identity_including_warmup() {
        assert!(same_prepared_round(None, None));
        assert!(same_prepared_round(Some(17), Some(17)));
        assert!(!same_prepared_round(None, Some(1)));
        assert!(!same_prepared_round(Some(17), Some(18)));
        assert!(!same_prepared_round(Some(17), None));
    }

    #[test]
    fn reset_retains_old_identity_until_capture_fence_clears_it() {
        assert!(!RETIRE_SERVICE_ENDPOINT_SQL.contains("container_id = NULL"));
        assert!(PUBLISH_SERVICE_ENDPOINT_SQL.contains("service.container_id IS NULL"));
    }
}
