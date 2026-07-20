//! Transactional persistence for checker verdicts.

use super::*;
use std::collections::HashMap;

const CHECK_RESULT_BATCH_SIZE: usize = 64;
const CHECK_RESULT_FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);
const AD_CHECK_RESULT_UPSERT_SQL: &str = r#"INSERT INTO "AdCheckResults"
     (round_id, team_service_id, status, message, checked_at, sla_credit,
      flag_verified)
   SELECT $1, service.id, $3, $4, $5, $6, $16
     FROM "AdTeamServices" service
     JOIN "Participations" participation
       ON participation.id = service.participation_id
      AND participation.game_id = service.game_id
     JOIN "GameChallenges" challenge
       ON challenge.id = service.challenge_id
      AND challenge.game_id = service.game_id
    WHERE service.id = $2
      AND service.game_id = $10
      AND service.participation_id = $11
      AND service.challenge_id = $12
      AND service.host = $13
      AND service.port = $14
      AND service.container_id IS NOT DISTINCT FROM $15
      AND participation.status = $7
      AND challenge.is_enabled = TRUE
      AND challenge.review_status = $8
      AND challenge."Type" = $9
   ON CONFLICT (round_id, team_service_id) DO UPDATE SET
     status = EXCLUDED.status, message = EXCLUDED.message,
     checked_at = EXCLUDED.checked_at, sla_credit = EXCLUDED.sla_credit,
     flag_verified = EXCLUDED.flag_verified
   WHERE "AdCheckResults".sla_credit IS NULL"#;

#[derive(Debug)]
pub(super) struct AdProbeResult {
    pub(super) service_id: i32,
    pub(super) participation_id: i32,
    pub(super) challenge_id: i32,
    pub(super) host: String,
    pub(super) port: i32,
    pub(super) container_id: Option<String>,
    pub(super) status: AdCheckStatus,
    pub(super) message: Option<String>,
    pub(super) flag_verified: bool,
    /// Wall time immediately after the service probe completed. Persistence may
    /// wait on the game lock, so its transaction time is not scoring evidence.
    pub(super) observed_at: chrono::DateTime<Utc>,
}

impl AdProbeResult {
    fn bind_to_authoritative_identity(
        &mut self,
        participation_id: i32,
        challenge_id: i32,
        host: &str,
        port: i32,
        container_id: &Option<String>,
    ) -> bool {
        if participation_id != self.participation_id || challenge_id != self.challenge_id {
            return false;
        }
        if host != self.host || port != self.port || container_id != &self.container_id {
            self.host = host.to_string();
            self.port = port;
            self.container_id.clone_from(container_id);
            self.status = AdCheckStatus::Offline;
            self.message = Some(
                "service endpoint changed after flag publication; participant sample offline"
                    .to_string(),
            );
            self.flag_verified = false;
        }
        true
    }
}

/// Drain completed probes into small transactions owned by the caller's round
/// pipeline. Closing the channel flushes the final partial batch; cancellation
/// drops this future and leaves unresolved rows available to the next owner.
pub(super) async fn record_check_result_batches(
    db: &DatabaseConnection,
    game_id: i32,
    round_id: i32,
    lease: &RoundFinishLease,
    receiver: tokio::sync::mpsc::UnboundedReceiver<AdProbeResult>,
) -> AppResult<()> {
    drain_check_result_batches(
        receiver,
        CHECK_RESULT_BATCH_SIZE,
        CHECK_RESULT_FLUSH_INTERVAL,
        |results| record_check_results(db, game_id, round_id, lease, results),
    )
    .await?;
    // Channel closure after a successful drain means every probe produced a
    // result. Only then seal preparation placeholders; a cancellation or writer
    // error must leave them recoverable by the next pipeline lease owner.
    // Convert only still-NULL preparation placeholders into explicit infrastructure
    // completions so the epoch cannot remain unsettled forever.
    complete_unresolved_check_results(db, game_id, round_id, lease).await
}

async fn complete_unresolved_check_results(
    db: &DatabaseConnection,
    game_id: i32,
    round_id: i32,
    lease: &RoundFinishLease,
) -> AppResult<()> {
    let mut control = super::koth_auth::acquire_game_lock(db, game_id).await?;
    super::lock_owned_round_finish(control.transaction_mut(), game_id, round_id, lease).await?;
    complete_unresolved_check_results_transaction(control.transaction_mut(), game_id, round_id)
        .await?;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

pub(super) async fn complete_unresolved_check_results_transaction(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    game_id: i32,
    round_id: i32,
) -> AppResult<()> {
    sqlx::query(
        r#"UPDATE "AdCheckResults" result
              SET status = $3,
                  message = 'checker pass cancelled before completion',
                  checked_at = LEAST(clock_timestamp(), game.end_time_utc,
                                     round.end_time_utc),
                  sla_credit = 0.0,
                  flag_verified = FALSE
             FROM "AdRounds" round
             JOIN "Games" game ON game.id = round.game_id
            WHERE round.id = result.round_id
              AND round.game_id = $1
              AND round.id = $2
              AND result.sla_credit IS NULL"#,
    )
    .bind(game_id)
    .bind(round_id)
    .bind(AdCheckStatus::InternalError as i16)
    .execute(&mut **tx)
    .await
    .map(|_| ())
    .map_err(|error| AppError::internal(error.to_string()))
}

/// Fill one immutable platform-void KotH observation for every configured hill
/// that did not produce a checker result. Called only after all hill futures
/// have completed or been cancelled, so a late real result cannot be masked by
/// the fallback row.
pub(crate) async fn complete_missing_koth_results(
    db: &DatabaseConnection,
    game_id: i32,
    round_id: i32,
    lease: &RoundFinishLease,
) -> AppResult<()> {
    let mut control = super::koth_auth::acquire_game_lock(db, game_id).await?;
    super::lock_owned_round_finish(control.transaction_mut(), game_id, round_id, lease).await?;
    complete_missing_koth_results_transaction(control.transaction_mut(), game_id, round_id).await?;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

pub(super) async fn complete_missing_koth_results_transaction(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    game_id: i32,
    round_id: i32,
) -> AppResult<()> {
    sqlx::query(
        r#"INSERT INTO "KothControlResults"
             (game_id, challenge_id, ad_round_id,
              controlling_participation_id, responsible_participation_id,
              marker_observed, status, error_message, checked_at,
              is_scorable, void_reason, cycle_id, container_id,
              confirmation_streak, confirmed_participation_id,
              token_window_attempt)
           SELECT target.game_id, target.challenge_id, round.id,
                  NULL, participation.id, FALSE, $3, $4,
                  LEAST(clock_timestamp(), game.end_time_utc, round.end_time_utc),
                  FALSE, $4, cycle.id,
                  COALESCE(cycle.replacement_container_id,
                           cycle.old_container_id, target.container_id),
                  CASE WHEN cycle.id IS NULL THEN NULL ELSE 0 END,
                  target.holder_participation_id,
                  COALESCE(cycle.reset_attempt, 0)
             FROM "KothTargets" target
             JOIN "Games" game ON game.id = target.game_id
             JOIN "GameChallenges" challenge
               ON challenge.id = target.challenge_id
              AND challenge.game_id = target.game_id
             JOIN "AdRounds" round
               ON round.id = $2 AND round.game_id = target.game_id
             LEFT JOIN "Participations" participation
               ON participation.id = target.holder_participation_id
              AND participation.game_id = target.game_id
              AND participation.status = $5
             LEFT JOIN LATERAL (
               SELECT crown.id, crown.replacement_container_id,
                      crown.old_container_id, crown.reset_attempt
                 FROM "KothCrownCycles" crown
                WHERE crown.game_id = target.game_id
                  AND crown.challenge_id = target.challenge_id
                  AND round.number BETWEEN crown.planned_start_round
                                       AND crown.planned_end_round
                ORDER BY crown.cycle_number DESC
                LIMIT 1
             ) cycle ON TRUE
            WHERE target.game_id = $1
              AND round.finalized = FALSE
              AND challenge.is_enabled = TRUE
              AND challenge.review_status = $6
              AND challenge."Type" = $7
           ON CONFLICT (game_id, challenge_id, ad_round_id) DO NOTHING"#,
    )
    .bind(game_id)
    .bind(round_id)
    .bind(AdCheckStatus::InternalError as i16)
    .bind("checker pass did not produce a hill observation; scoring sample void")
    .bind(ParticipationStatus::Accepted as i16)
    .bind(ChallengeReviewStatus::Active as i16)
    .bind(ChallengeType::KingOfTheHill as i16)
    .execute(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

/// Close the final round of an ended game after the checker grace window.
///
/// Checker result batches, KotH persistence, and round preparation all take the
/// same per-game lock. Taking it here makes closeout a fence: real late results
/// either commit first, or observe the finalized round and cannot overwrite the
/// explicit infrastructure fallback after an immutable epoch is materialized.
pub(crate) async fn finalize_ended_round_checks(
    db: &DatabaseConnection,
    game_id: i32,
    grace_seconds: i64,
) -> AppResult<bool> {
    let mut control_lock = super::koth_auth::acquire_game_lock(db, game_id).await?;
    let round_ids = sqlx::query_scalar::<_, i32>(
        r#"SELECT round.id
             FROM "AdRounds" round
             JOIN "Games" game ON game.id = round.game_id
            WHERE game.id = $1
              AND game.end_time_utc <= clock_timestamp()
              AND (
                    NOT EXISTS (
                        SELECT 1 FROM "AdCheckResults" pending
                         WHERE pending.round_id = round.id
                           AND pending.sla_credit IS NULL
                    )
                    AND NOT EXISTS (
                        SELECT 1
                          FROM "KothTargets" target
                          JOIN "GameChallenges" challenge
                            ON challenge.id = target.challenge_id
                           AND challenge.game_id = target.game_id
                         WHERE target.game_id = game.id
                           AND challenge.is_enabled = TRUE
                           AND challenge.review_status = $3
                           AND challenge."Type" = $4
                           AND NOT EXISTS (
                                SELECT 1 FROM "KothControlResults" result
                                 WHERE result.game_id = target.game_id
                                   AND result.challenge_id = target.challenge_id
                                   AND result.ad_round_id = round.id
                           )
                    )
                    OR game.end_time_utc <=
                       clock_timestamp() - ($2 * interval '1 second')
              )
              AND round.finalized = FALSE
            ORDER BY round.number, round.id
            FOR UPDATE OF round"#,
    )
    .bind(game_id)
    .bind(grace_seconds.max(0))
    .bind(ChallengeReviewStatus::Active as i16)
    .bind(ChallengeType::KingOfTheHill as i16)
    .fetch_all(&mut **control_lock.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    if round_ids.is_empty() {
        control_lock
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(false);
    }

    for round_id in &round_ids {
        super::flag_delivery::complete_missing_flag_delivery_outcomes_transaction(
            control_lock.transaction_mut(),
            game_id,
            *round_id,
        )
        .await?;
    }

    sqlx::query(
        r#"UPDATE "AdCheckResults"
              SET status = $2,
                  message = 'checker pass did not complete before event-close grace expired',
                  checked_at = game.end_time_utc,
                  sla_credit = 0.0,
                  flag_verified = FALSE
             FROM "AdRounds" round
             JOIN "Games" game ON game.id = round.game_id
            WHERE "AdCheckResults".round_id = round.id
              AND round.id = ANY($1)
              AND "AdCheckResults".sla_credit IS NULL"#,
    )
    .bind(&round_ids)
    .bind(AdCheckStatus::InternalError as i16)
    .execute(&mut **control_lock.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    // KotH's checker refuses observations at or after the event deadline. Record the same
    // conservative no-credit fallback that a normal next-round boundary would
    // have written before sealing the last round. Timestamp this synthetic
    // closeout at the deadline so the immutable end-time evidence cutoff includes
    // it without admitting a real post-deadline marker observation.
    sqlx::query(
        r#"INSERT INTO "KothControlResults"
             (game_id, challenge_id, ad_round_id, controlling_participation_id,
              responsible_participation_id, marker_observed, status,
              error_message, checked_at,
              is_scorable, void_reason, cycle_id, confirmation_streak,
              confirmed_participation_id, token_window_attempt)
           SELECT $1, target.challenge_id, round.id, NULL, participation.id,
                  FALSE, $3,
                  'checker result unavailable; scoring sample void',
                  game.end_time_utc, FALSE,
                  'checker result unavailable; scoring sample void',
                  cycle.id, CASE WHEN cycle.id IS NULL THEN NULL ELSE 0 END,
                  target.holder_participation_id,
                  COALESCE(cycle.reset_attempt, 0)
             FROM "KothTargets" target
             JOIN "GameChallenges" challenge
               ON challenge.id = target.challenge_id
              AND challenge.game_id = target.game_id
             JOIN "Games" game ON game.id = target.game_id
             CROSS JOIN "AdRounds" round
             LEFT JOIN LATERAL (
               SELECT crown.id, crown.reset_attempt
                 FROM "KothCrownCycles" crown
                WHERE crown.game_id = target.game_id
                  AND crown.challenge_id = target.challenge_id
                  AND round.number BETWEEN crown.planned_start_round
                                       AND crown.planned_end_round
                ORDER BY crown.cycle_number DESC
                LIMIT 1
             ) cycle ON TRUE
             LEFT JOIN "Participations" participation
               ON participation.id = target.holder_participation_id
              AND participation.game_id = target.game_id
              AND participation.status = $4
            WHERE target.game_id = $1
              AND round.game_id = target.game_id
              AND round.id = ANY($2)
              AND challenge.is_enabled = TRUE
              AND challenge.review_status = $5
              AND challenge."Type" = $6
           ON CONFLICT (game_id, challenge_id, ad_round_id) DO NOTHING"#,
    )
    .bind(game_id)
    .bind(&round_ids)
    .bind(AdCheckStatus::InternalError as i16)
    .bind(ParticipationStatus::Accepted as i16)
    .bind(ChallengeReviewStatus::Active as i16)
    .bind(ChallengeType::KingOfTheHill as i16)
    .execute(&mut **control_lock.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    sqlx::query(
        r#"UPDATE "KothCrownCycles" cycle
              SET phase = 'Completed',
                  actual_end_round = COALESCE(
                    cycle.actual_end_round,
                    CASE WHEN cycle.actual_start_round IS NULL THEN NULL ELSE GREATEST(
                      cycle.actual_start_round,
                      COALESCE((
                        SELECT MAX(round.number) FROM "AdRounds" round
                         JOIN "Games" game ON game.id = round.game_id
                        WHERE round.game_id = cycle.game_id
                          AND round.start_time_utc < game.end_time_utc
                      ), cycle.actual_start_round)
                    ) END
                  ),
                  finalized_at = COALESCE(finalized_at, clock_timestamp()),
                  completed_at = COALESCE(completed_at, clock_timestamp()),
                  updated_at = clock_timestamp()
            WHERE cycle.game_id = $1
              AND cycle.phase IN ('Active','CooldownReleasePending')"#,
    )
    .bind(game_id)
    .execute(&mut **control_lock.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    sqlx::query(r#"UPDATE "AdRounds" SET finalized = TRUE WHERE id = ANY($1)"#)
        .bind(&round_ids)
        .execute(&mut **control_lock.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    control_lock
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(true)
}

async fn drain_check_result_batches<F, Fut>(
    mut receiver: tokio::sync::mpsc::UnboundedReceiver<AdProbeResult>,
    batch_size: usize,
    flush_interval: std::time::Duration,
    mut persist: F,
) -> AppResult<()>
where
    F: FnMut(Vec<AdProbeResult>) -> Fut,
    Fut: std::future::Future<Output = AppResult<()>>,
{
    debug_assert!(batch_size > 0);
    let mut closed = false;
    while !closed {
        let Some(first) = receiver.recv().await else {
            break;
        };
        let mut batch = Vec::with_capacity(batch_size);
        batch.push(first);
        let deadline = tokio::time::Instant::now() + flush_interval;

        while batch.len() < batch_size {
            match tokio::time::timeout_at(deadline, receiver.recv()).await {
                Ok(Some(result)) => batch.push(result),
                Ok(None) => {
                    closed = true;
                    break;
                }
                Err(_) => break,
            }
        }
        persist(batch).await?;
    }
    Ok(())
}

/// Persist a checker tick only while its game, round, and exact probed roster are
/// still authoritative. The same advisory transaction serializes round creation.
pub(super) async fn record_check_results(
    db: &DatabaseConnection,
    game_id: i32,
    round_id: i32,
    lease: &RoundFinishLease,
    results: Vec<AdProbeResult>,
) -> AppResult<()> {
    if results.is_empty() {
        return Ok(());
    }

    let mut control_lock = super::koth_auth::acquire_game_lock(db, game_id).await?;
    let result = record_check_results_transaction(
        control_lock.transaction_mut(),
        game_id,
        round_id,
        lease,
        results,
    )
    .await;
    match result {
        Err(error) => Err(error),
        Ok(()) => control_lock
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string())),
    }
}

async fn record_check_results_transaction(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    game_id: i32,
    round_id: i32,
    lease: &RoundFinishLease,
    mut results: Vec<AdProbeResult>,
) -> AppResult<()> {
    super::lock_owned_round_finish(tx, game_id, round_id, lease).await?;
    let current_round = sqlx::query_as::<
        _,
        (
            chrono::DateTime<Utc>,
            chrono::DateTime<Utc>,
            chrono::DateTime<Utc>,
            chrono::DateTime<Utc>,
        ),
    >(
        r#"SELECT game.start_time_utc, game.end_time_utc,
                  round.start_time_utc, round.end_time_utc
             FROM "AdRounds" round
             JOIN "Games" game ON game.id = round.game_id
            WHERE game.id = $1
              AND round.id = $2
              AND round.finalized = FALSE
              AND round.id = (
                  SELECT latest.id
                    FROM "AdRounds" latest
                   WHERE latest.game_id = game.id
                   ORDER BY latest.number DESC, latest.id DESC
                   LIMIT 1
              )
            FOR SHARE OF game, round"#,
    )
    .bind(game_id)
    .bind(round_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((game_start, game_end, round_start, round_end)) = current_round else {
        return Err(AppError::conflict(
            "Checker round is no longer the authoritative live round",
        ));
    };
    results.retain(|result| {
        checker_result_time_is_valid(
            game_start,
            game_end,
            round_start,
            round_end,
            result.observed_at,
        )
    });
    if results.is_empty() {
        return Ok(());
    }

    // Lock every existing service-side roster row, including rejected/disabled
    // rows. A concurrent reject, challenge disable, or service repoint must wait
    // until this tick commits, and a change that won the race is observed here.
    #[allow(clippy::type_complexity)]
    let roster: Vec<(
        i32,
        i32,
        i32,
        String,
        i32,
        Option<String>,
        i16,
        bool,
        i16,
        i16,
    )> = sqlx::query_as(
        r#"SELECT service.id, service.participation_id, service.challenge_id,
                  service.host, service.port, service.container_id,
                  participation.status, challenge.is_enabled,
                  challenge.review_status, challenge."Type"
             FROM "AdTeamServices" service
             JOIN "Participations" participation
               ON participation.id = service.participation_id
              AND participation.game_id = service.game_id
             JOIN "GameChallenges" challenge
               ON challenge.id = service.challenge_id
              AND challenge.game_id = service.game_id
            WHERE service.game_id = $1
            ORDER BY service.id
            FOR SHARE OF service, participation, challenge"#,
    )
    .bind(game_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let mut authorized = HashMap::new();
    for (sid, pid, cid, host, port, container_id, status, enabled, review, kind) in roster {
        if status == ParticipationStatus::Accepted as i16
            && enabled
            && review == ChallengeReviewStatus::Active as i16
            && kind == ChallengeType::AttackDefense as i16
        {
            authorized.insert(sid, (pid, cid, host, port, container_id));
        }
    }
    results = results
        .into_iter()
        .filter_map(|mut result| {
            let (participation_id, challenge_id, host, port, container_id) =
                authorized.get(&result.service_id)?;
            result
                .bind_to_authoritative_identity(
                    *participation_id,
                    *challenge_id,
                    host,
                    *port,
                    container_id,
                )
                .then_some(result)
        })
        .collect();
    if results.is_empty() {
        return Ok(());
    }
    let sids: Vec<i32> = results.iter().map(|result| result.service_id).collect();

    // One backward index seek per service avoids scanning and de-duplicating
    // every historical result for the roster as the event grows.
    let prev_rows: Vec<(i32, i16)> = sqlx::query_as(
        r#"SELECT service.sid, previous.status
             FROM unnest($2::int[]) AS service(sid)
             JOIN LATERAL (
               SELECT result.status
                 FROM "AdCheckResults" result
                WHERE result.team_service_id = service.sid
                  AND result.round_id < $1
                ORDER BY result.round_id DESC
                LIMIT 1
             ) previous ON TRUE"#,
    )
    .bind(round_id)
    .bind(&sids)
    .fetch_all(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let prev: HashMap<i32, AdCheckStatus> = prev_rows
        .into_iter()
        .map(|(sid, status)| (sid, AdCheckStatus::from_i16(status)))
        .collect();

    for result in results {
        let credit = stored_tick_credit(result.status, prev.get(&result.service_id).copied());
        let message = super::checker::bounded_optional_diagnostic(result.message);
        let persisted = sqlx::query(AD_CHECK_RESULT_UPSERT_SQL)
            .bind(round_id)
            .bind(result.service_id)
            .bind(result.status as i16)
            .bind(message)
            .bind(result.observed_at)
            .bind(credit)
            .bind(ParticipationStatus::Accepted as i16)
            .bind(ChallengeReviewStatus::Active as i16)
            .bind(ChallengeType::AttackDefense as i16)
            .bind(game_id)
            .bind(result.participation_id)
            .bind(result.challenge_id)
            .bind(&result.host)
            .bind(result.port)
            .bind(&result.container_id)
            .bind(result.flag_verified)
            .execute(&mut **tx)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        if persisted.rows_affected() == 0 || result.status == AdCheckStatus::InternalError {
            continue;
        }

        sqlx::query(
            r#"UPDATE "AdTeamServices" service SET status = $2
                 FROM "Participations" participation, "GameChallenges" challenge
                WHERE service.id = $1
                  AND service.game_id = $6
                  AND service.participation_id = $7
                  AND service.challenge_id = $8
                  AND service.host = $9
                  AND service.port = $10
                  AND service.container_id IS NOT DISTINCT FROM $11
                  AND participation.id = service.participation_id
                  AND participation.game_id = service.game_id
                  AND participation.status = $3
                  AND challenge.id = service.challenge_id
                  AND challenge.game_id = service.game_id
                  AND challenge.is_enabled = TRUE
                  AND challenge.review_status = $4
                  AND challenge."Type" = $5"#,
        )
        .bind(result.service_id)
        .bind(result.status as i16)
        .bind(ParticipationStatus::Accepted as i16)
        .bind(ChallengeReviewStatus::Active as i16)
        .bind(ChallengeType::AttackDefense as i16)
        .bind(game_id)
        .bind(result.participation_id)
        .bind(result.challenge_id)
        .bind(&result.host)
        .bind(result.port)
        .bind(&result.container_id)
        .execute(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }

    Ok(())
}

fn checker_result_time_is_valid(
    game_start: chrono::DateTime<Utc>,
    game_end: chrono::DateTime<Utc>,
    round_start: chrono::DateTime<Utc>,
    round_end: chrono::DateTime<Utc>,
    observed_at: chrono::DateTime<Utc>,
) -> bool {
    game_start <= observed_at
        && round_start <= observed_at
        && observed_at < round_end
        && observed_at < game_end
}

#[cfg(test)]
mod batch_tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use sea_orm::{ConnectOptions, Database};

    #[test]
    fn checker_replay_upsert_only_fills_unresolved_placeholders() {
        assert!(AD_CHECK_RESULT_UPSERT_SQL
            .trim_end()
            .ends_with(r#"WHERE "AdCheckResults".sla_credit IS NULL"#));
    }

    #[test]
    fn endpoint_churn_becomes_participant_offline_on_the_current_identity() {
        let mut probe = result(7);
        assert!(probe.bind_to_authoritative_identity(7, 1, "", 0, &None));
        assert_eq!(probe.status, AdCheckStatus::Offline);
        assert_eq!((probe.host.as_str(), probe.port), ("", 0));
        assert!(!probe.flag_verified);
        assert!(probe.message.unwrap().contains("endpoint changed"));
    }

    fn result(service_id: i32) -> AdProbeResult {
        AdProbeResult {
            service_id,
            participation_id: service_id,
            challenge_id: 1,
            host: "127.0.0.1".to_string(),
            port: 31337,
            container_id: None,
            status: AdCheckStatus::Ok,
            message: None,
            flag_verified: true,
            observed_at: Utc::now(),
        }
    }

    #[test]
    fn checker_observation_has_a_strict_event_end_fence() {
        let game_start = Utc::now() - chrono::Duration::hours(2);
        let round_start = game_start + chrono::Duration::hours(1);
        let game_end = round_start + chrono::Duration::hours(1);

        assert!(checker_result_time_is_valid(
            game_start,
            game_end,
            round_start,
            game_end,
            game_end - chrono::Duration::milliseconds(1),
        ));
        assert!(!checker_result_time_is_valid(
            game_start,
            game_end,
            round_start,
            game_end,
            game_end,
        ));
        assert!(!checker_result_time_is_valid(
            game_start,
            game_end,
            round_start,
            game_end,
            game_end + chrono::Duration::milliseconds(1),
        ));
    }

    #[tokio::test]
    async fn closed_channel_flushes_bounded_batches_and_partial_tail() {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        for service_id in 1..=10 {
            sender.send(result(service_id)).unwrap();
        }
        drop(sender);

        let persisted = Arc::new(Mutex::new(Vec::<Vec<i32>>::new()));
        let captured = Arc::clone(&persisted);
        drain_check_result_batches(
            receiver,
            4,
            std::time::Duration::from_secs(60),
            move |batch| {
                let captured = Arc::clone(&captured);
                async move {
                    captured
                        .lock()
                        .unwrap()
                        .push(batch.into_iter().map(|row| row.service_id).collect());
                    Ok(())
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(
            *persisted.lock().unwrap(),
            vec![vec![1, 2, 3, 4], vec![5, 6, 7, 8], vec![9, 10]]
        );
    }

    #[tokio::test]
    async fn flush_deadline_persists_partial_batch_before_sender_closes() {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        sender.send(result(7)).unwrap();
        sender.send(result(8)).unwrap();

        let (persisted_tx, mut persisted_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i32>>();
        let writer = tokio::spawn(drain_check_result_batches(
            receiver,
            64,
            std::time::Duration::from_millis(10),
            move |batch| {
                let persisted_tx = persisted_tx.clone();
                async move {
                    persisted_tx
                        .send(batch.into_iter().map(|row| row.service_id).collect())
                        .unwrap();
                    Ok(())
                }
            },
        ));

        let first = tokio::time::timeout(std::time::Duration::from_secs(1), persisted_rx.recv())
            .await
            .expect("partial batch should flush on its deadline")
            .expect("writer should report the persisted batch");
        assert_eq!(first, vec![7, 8]);

        drop(sender);
        writer.await.unwrap().unwrap();
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn event_closeout_binds_void_to_the_exact_crown_cycle() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
        let mut options = ConnectOptions::new(database_url);
        options.max_connections(1).min_connections(1);
        let db = Database::connect(options).await.unwrap();
        let pool = db.get_postgres_connection_pool();
        sqlx::raw_sql(
            r#"
            CREATE TEMP TABLE "Games" (
              id INTEGER PRIMARY KEY, start_time_utc TIMESTAMPTZ,
              end_time_utc TIMESTAMPTZ
            );
            CREATE TEMP TABLE "AdRounds" (
              id INTEGER PRIMARY KEY, game_id INTEGER, number INTEGER,
              start_time_utc TIMESTAMPTZ, end_time_utc TIMESTAMPTZ,
              finalized BOOLEAN, flags_published_at TIMESTAMPTZ,
              flag_delivery_failures INTEGER
            );
            CREATE TEMP TABLE "AdTeamServices" (
              id INTEGER PRIMARY KEY, game_id INTEGER, challenge_id INTEGER,
              container_id TEXT
            );
            CREATE TEMP TABLE "AdFlags" (
              round_id INTEGER, team_service_id INTEGER,
              PRIMARY KEY (round_id, team_service_id)
            );
            CREATE TEMP TABLE "AdFlagDeliveryResults" (
              round_id INTEGER, team_service_id INTEGER,
              delivery_kind TEXT, container_id TEXT, delivered BOOLEAN,
              attempts SMALLINT, failure_reason TEXT, completed_at TIMESTAMPTZ,
              PRIMARY KEY (round_id, team_service_id)
            );
            CREATE TEMP TABLE "AdCheckResults" (
              round_id INTEGER, team_service_id INTEGER, status SMALLINT, message TEXT,
              checked_at TIMESTAMPTZ, sla_credit DOUBLE PRECISION,
              flag_verified BOOLEAN
            );
            CREATE TEMP TABLE "GameChallenges" (
              id INTEGER PRIMARY KEY, game_id INTEGER, is_enabled BOOLEAN,
              review_status SMALLINT, "Type" SMALLINT,
              ad_self_hosted BOOLEAN
            );
            CREATE TEMP TABLE "Participations" (
              id INTEGER PRIMARY KEY, game_id INTEGER, status SMALLINT
            );
            CREATE TEMP TABLE "KothTargets" (
              game_id INTEGER, challenge_id INTEGER,
              holder_participation_id INTEGER
            );
            CREATE TEMP TABLE "KothCrownCycles" (
              id BIGINT PRIMARY KEY, game_id INTEGER, challenge_id INTEGER,
              cycle_number INTEGER,
              planned_start_round INTEGER, planned_end_round INTEGER,
              phase TEXT, actual_start_round INTEGER, actual_end_round INTEGER,
              finalized_at TIMESTAMPTZ,
              completed_at TIMESTAMPTZ, updated_at TIMESTAMPTZ,
              reset_attempt INTEGER,
              CONSTRAINT ck_koth_crown_cycles_rounds CHECK (
                planned_start_round >= 1
                AND planned_end_round >= planned_start_round
                AND (actual_start_round IS NULL
                     OR actual_start_round >= planned_start_round)
                AND (actual_end_round IS NULL OR (
                  actual_start_round IS NOT NULL
                  AND actual_end_round >= actual_start_round
                ))
              )
            );
            CREATE TEMP TABLE "KothControlResults" (
              game_id INTEGER, challenge_id INTEGER, ad_round_id INTEGER,
              controlling_participation_id INTEGER,
              responsible_participation_id INTEGER, marker_observed BOOLEAN,
              status SMALLINT, error_message TEXT, checked_at TIMESTAMPTZ,
              is_scorable BOOLEAN,
              void_reason TEXT, cycle_id BIGINT, confirmation_streak INTEGER,
              confirmed_participation_id INTEGER, token_window_attempt INTEGER
            );
            CREATE UNIQUE INDEX ON "KothControlResults"
              (game_id, challenge_id, ad_round_id);
            "#,
        )
        .execute(pool)
        .await
        .unwrap();
        let end: chrono::DateTime<Utc> =
            sqlx::query_scalar("SELECT clock_timestamp() - interval '1 minute'")
                .fetch_one(pool)
                .await
                .unwrap();
        sqlx::query(r#"INSERT INTO "Games" VALUES (41,$1,$2)"#)
            .bind(end - chrono::Duration::hours(1))
            .bind(end)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO "AdRounds"
                 VALUES (3,41,3,$1,$2,FALSE,NULL,0)"#,
        )
        .bind(end - chrono::Duration::seconds(30))
        .bind(end)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(r#"INSERT INTO "GameChallenges" VALUES (5,41,TRUE,$1,$2,FALSE)"#)
            .bind(ChallengeReviewStatus::Active as i16)
            .bind(ChallengeType::KingOfTheHill as i16)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "Participations" VALUES (7,41,$1)"#)
            .bind(ParticipationStatus::Accepted as i16)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "KothTargets" VALUES (41,5,7)"#)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO "KothCrownCycles"
               (id, game_id, challenge_id, cycle_number,
                planned_start_round, planned_end_round, phase,
                actual_start_round, actual_end_round, finalized_at,
                completed_at, updated_at, reset_attempt)
               VALUES (11,41,5,1,1,3,'Active',5,NULL,NULL,NULL,clock_timestamp(),4)"#,
        )
        .execute(pool)
        .await
        .unwrap();

        assert!(finalize_ended_round_checks(&db, 41, 0).await.unwrap());
        let receipt: (Option<i64>, Option<i32>, bool, Option<i32>, i32) = sqlx::query_as(
            r#"SELECT cycle_id, confirmation_streak, is_scorable,
                      confirmed_participation_id, token_window_attempt
                 FROM "KothControlResults" WHERE ad_round_id = 3"#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(receipt, (Some(11), Some(0), false, Some(7), 4));
        let cycle: (String, Option<i32>, Option<i32>) = sqlx::query_as(
            r#"SELECT phase, actual_start_round, actual_end_round
                 FROM "KothCrownCycles" WHERE id = 11"#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(cycle, ("Completed".to_string(), Some(5), Some(5)));
    }
}
