//! Authoritative crown-cycle KotH checker and immutable evidence writer.

use std::collections::HashMap;
use std::time::Duration;

use chrono::Utc;
use futures::StreamExt;
use sea_orm::DatabaseConnection;

use super::{bounded_diagnostic, bounded_optional_diagnostic, checker_concurrency, run_check};
use crate::models::data::ad_round;
use crate::services::ad::engine::{
    koth_auth,
    koth_cycle::{self, ClaimObservation, ObservedToken},
    koth_marker::{observation_precedes_deadline, read_koth_marker, stable_koth_marker},
    AdCheckStatus, RoundFinishLease,
};
use crate::services::container::{ContainerLiveness, ContainerManager};
use crate::utils::enums::ParticipationStatus;
use crate::utils::error::{AppError, AppResult};

#[derive(Debug, PartialEq, Eq)]
enum ManagedHillLiveness {
    Running,
    Dead(String),
    Unknown(String),
}

async fn inspect_liveness(
    containers: &dyn ContainerManager,
    container_id: &str,
) -> ManagedHillLiveness {
    match containers.inspect_liveness(container_id).await {
        Ok(ContainerLiveness::Running) => ManagedHillLiveness::Running,
        Ok(ContainerLiveness::Stopped) => ManagedHillLiveness::Dead(container_id.to_string()),
        Ok(ContainerLiveness::Unknown) => {
            ManagedHillLiveness::Unknown("backend is in a transitional state".to_string())
        }
        Err(error) => ManagedHillLiveness::Unknown(error.to_string()),
    }
}

#[derive(Clone, Debug, sqlx::FromRow)]
struct LiveHill {
    target_id: i32,
    challenge_id: i32,
    host: String,
    port: i32,
    container_id: String,
    cycle_id: i64,
    token_window_attempt: i32,
    phase: String,
    claim_confirmation_ticks: i32,
    token_count: i64,
    roster_count: i64,
    game_start: chrono::DateTime<Utc>,
    game_end: chrono::DateTime<Utc>,
    round_start: chrono::DateTime<Utc>,
    round_end: chrono::DateTime<Utc>,
}

async fn load_live_hill(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
    round: &ad_round::Model,
) -> AppResult<Option<LiveHill>> {
    sqlx::query_as::<_, LiveHill>(
        r#"SELECT target.id AS target_id, target.challenge_id,
                  target.host, target.port,
                  COALESCE(target.container_id, cycle.replacement_container_id,
                           cycle.old_container_id, '') AS container_id,
                  cycle.id AS cycle_id,
                  cycle.reset_attempt AS token_window_attempt,
                  cycle.phase, config.claim_confirmation_ticks,
                  (SELECT COUNT(*) FROM "KothTokens" token
                    WHERE token.cycle_id = cycle.id
                      AND token.challenge_id = target.challenge_id
                      AND token.target_id = target.id
                      AND token.reset_attempt = cycle.reset_attempt
                      AND token.revoked_at IS NULL) AS token_count,
                  jsonb_array_length(config.roster_snapshot)::bigint AS roster_count,
                  game.start_time_utc AS game_start,
                  game.end_time_utc AS game_end,
                  scoring_round.start_time_utc AS round_start,
                  scoring_round.end_time_utc AS round_end
             FROM "KothTargets" target
             JOIN "Games" game ON game.id = target.game_id
             JOIN "KothOfficialConfigs" config
               ON config.game_id = target.game_id
             JOIN "AdRounds" scoring_round
               ON scoring_round.id = $3 AND scoring_round.game_id = target.game_id
             JOIN LATERAL (
               SELECT crown.* FROM "KothCrownCycles" crown
                WHERE crown.game_id = target.game_id
                  AND crown.challenge_id = target.challenge_id
                  AND scoring_round.number BETWEEN crown.planned_start_round
                                               AND crown.planned_end_round
                  AND crown.replacement_container_id = target.container_id
                ORDER BY crown.cycle_number DESC LIMIT 1
             ) cycle ON TRUE
            WHERE target.game_id = $1 AND target.challenge_id = $2
              AND scoring_round.finalized = FALSE"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(round.id)
    .fetch_optional(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

async fn insert_missing_cycle_void(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
    round: &ad_round::Model,
) -> AppResult<()> {
    sqlx::query(
        r#"INSERT INTO "KothControlResults"
             (game_id, challenge_id, ad_round_id,
              controlling_participation_id, responsible_participation_id,
              marker_observed, status, error_message,
              checked_at, cycle_id, container_id, confirmation_streak,
              is_scorable, void_reason, token_window_attempt)
           SELECT scope.game_id, scope.challenge_id, $3,
                  NULL, NULL, FALSE, $4, $5, clock_timestamp(), cycle.id,
                  COALESCE(cycle.replacement_container_id, cycle.old_container_id),
                  CASE WHEN cycle.id IS NULL THEN NULL ELSE 0 END,
                  FALSE, $5, COALESCE(cycle.reset_attempt, 0)
             FROM (VALUES ($1::integer, $2::integer)) scope(game_id, challenge_id)
             LEFT JOIN LATERAL (
               SELECT crown.id, crown.game_id, crown.challenge_id,
                      crown.replacement_container_id, crown.old_container_id,
                      crown.reset_attempt
                 FROM "KothCrownCycles" crown
                 JOIN "KothOfficialConfigs" config
                   ON config.game_id = crown.game_id
                WHERE crown.game_id = scope.game_id
                  AND crown.challenge_id = scope.challenge_id
                  AND $6 BETWEEN crown.planned_start_round
                                     AND crown.planned_end_round
                ORDER BY crown.cycle_number DESC
                LIMIT 1
             ) cycle ON TRUE
           ON CONFLICT (game_id, challenge_id, ad_round_id) DO NOTHING"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(round.id)
    .bind(AdCheckStatus::InternalError as i16)
    .bind("crown-cycle backend is unpublished; reset/readiness sample void")
    .bind(round.number)
    .execute(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

async fn insert_void(
    connection: &mut sqlx::PgConnection,
    hill: &LiveHill,
    game_id: i32,
    round: &ad_round::Model,
    reason: &str,
) -> AppResult<()> {
    sqlx::query(
        r#"INSERT INTO "KothControlResults"
             (game_id, challenge_id, ad_round_id,
              controlling_participation_id, responsible_participation_id,
              marker_observed, status, error_message,
              checked_at, cycle_id, container_id, confirmation_streak,
              is_scorable, void_reason, token_window_attempt)
           VALUES ($1,$2,$3,NULL,NULL,FALSE,$4,$5,clock_timestamp(),
                   $6,$7,0,FALSE,$5,$8)
           ON CONFLICT (game_id, challenge_id, ad_round_id) DO NOTHING"#,
    )
    .bind(game_id)
    .bind(hill.challenge_id)
    .bind(round.id)
    .bind(AdCheckStatus::InternalError as i16)
    .bind(reason)
    .bind(hill.cycle_id)
    .bind(&hill.container_id)
    .bind(hill.token_window_attempt)
    .execute(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

async fn check_one_hill(
    db: &DatabaseConnection,
    containers: &dyn ContainerManager,
    game_id: i32,
    challenge_id: i32,
    round: &ad_round::Model,
    checker_dir: Option<&str>,
    timeout: Duration,
    lease: &RoundFinishLease,
) -> AppResult<()> {
    // Runtime reset and checker ownership use the exact same hill lock. The
    // game-control lock is taken only for short SQL sections and always after
    // this lock, matching the lifecycle path's lock order.
    let key = format!("shared-container:{challenge_id}");
    let _local = crate::utils::single_flight::coalesce(&key).await;
    let lifecycle = crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(
        db.get_postgres_connection_pool(),
        &key,
    )
    .await?;
    let mut control = koth_auth::acquire_game_lock(db, game_id).await?;
    crate::services::ad_engine::lock_owned_round_finish(
        control.transaction_mut(),
        game_id,
        round.id,
        lease,
    )
    .await?;
    let Some(hill) = load_live_hill(
        &mut *control.transaction_mut(),
        game_id,
        challenge_id,
        round,
    )
    .await?
    else {
        insert_missing_cycle_void(
            &mut *control.transaction_mut(),
            game_id,
            challenge_id,
            round,
        )
        .await?;
        control
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        lifecycle.release().await?;
        return Ok(());
    };
    let complete_tokens = hill.roster_count >= 2 && hill.token_count == hill.roster_count;
    if hill.phase != "Active" || !complete_tokens {
        let reason = if hill.phase != "Active" {
            std::borrow::Cow::Owned(format!(
                "crown cycle is {}; reset/readiness sample void",
                hill.phase
            ))
        } else {
            std::borrow::Cow::Borrowed("crown-cycle token issuance is incomplete; sample void")
        };
        insert_void(
            &mut *control.transaction_mut(),
            &hill,
            game_id,
            round,
            reason.as_ref(),
        )
        .await?;
        control
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        lifecycle.release().await?;
        return Ok(());
    }
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    let liveness = inspect_liveness(containers, &hill.container_id).await;
    let (marker, marker_observed, status, message, dead_container_id) = match liveness {
        ManagedHillLiveness::Dead(container_id) => (
            None,
            false,
            AdCheckStatus::Offline,
            Some("managed hill container is not running".to_string()),
            Some(container_id),
        ),
        ManagedHillLiveness::Unknown(error) => (
            None,
            false,
            AdCheckStatus::InternalError,
            Some(format!("managed hill liveness is unknown: {error}")),
            None,
        ),
        ManagedHillLiveness::Running => {
            let before = read_koth_marker(containers, Some(&hill.container_id)).await;
            let (status, message) = run_check(
                checker_dir,
                &hill.host,
                hill.port,
                round.number,
                0,
                challenge_id,
                None,
                timeout,
            )
            .await;
            let after = read_koth_marker(containers, Some(&hill.container_id)).await;
            let (marker, observed, error) = stable_koth_marker(before, after);
            if let Some(error) = error {
                let error = bounded_diagnostic(error);
                tracing::warn!(challenge = challenge_id, %error, "KotH marker was unstable");
            }
            (marker, observed, status, message, None)
        }
    };
    let message = bounded_optional_diagnostic(message);
    let observed_at = Utc::now();

    let mut control = koth_auth::acquire_game_lock(db, game_id).await?;
    crate::services::ad_engine::lock_owned_round_finish(
        control.transaction_mut(),
        game_id,
        round.id,
        lease,
    )
    .await?;
    let Some(current) = load_live_hill(
        &mut *control.transaction_mut(),
        game_id,
        challenge_id,
        round,
    )
    .await?
    else {
        insert_missing_cycle_void(
            &mut *control.transaction_mut(),
            game_id,
            challenge_id,
            round,
        )
        .await?;
        control
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        lifecycle.release().await?;
        return Ok(());
    };
    let duplicate: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(SELECT 1 FROM "KothControlResults"
                          WHERE game_id = $1 AND challenge_id = $2
                            AND ad_round_id = $3)"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(round.id)
    .fetch_one(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if duplicate
        || current.cycle_id != hill.cycle_id
        || current.container_id != hill.container_id
        || current.phase != "Active"
        || current.game_start > observed_at
        || current.round_start > observed_at
        || !observation_precedes_deadline(observed_at, current.round_end)
        || !observation_precedes_deadline(observed_at, current.game_end)
    {
        control
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        lifecycle.release().await?;
        return Ok(());
    }

    let observed_capability = if let Some(marker) = marker.as_deref() {
        sqlx::query_as::<_, (i32, i32, i32, bool)>(
            r#"SELECT token.id, token.participation_id, token.round_number,
                      NOT EXISTS(
                        SELECT 1 FROM "KothCycleCooldowns" cooldown
                         WHERE cooldown.cycle_id = token.cycle_id
                           AND cooldown.participation_id = token.participation_id
                           AND cooldown.starts_round <= $7
                           AND cooldown.expires_after_round >= $7
                           AND cooldown.network_enforced = TRUE
                      ) AS claimant_is_eligible
                 FROM "KothTokens" token
                 JOIN "Participations" participation
                   ON participation.id = token.participation_id
                  AND participation.game_id = $1
                  AND participation.status = $2
                WHERE token.cycle_id = $3 AND token.challenge_id = $4
                  AND token.target_id = $5 AND token.token = $6
                  AND token.reset_attempt = $8
                  AND token.revoked_at IS NULL
                LIMIT 1"#,
        )
        .bind(game_id)
        .bind(ParticipationStatus::Accepted as i16)
        .bind(current.cycle_id)
        .bind(challenge_id)
        .bind(current.target_id)
        .bind(marker)
        .bind(round.number)
        .bind(current.token_window_attempt)
        .fetch_optional(&mut **control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .map(|row| {
            (
                ObservedToken {
                    id: row.0,
                    participation_id: row.1,
                    window_round: row.2,
                },
                row.3,
            )
        })
    } else {
        None
    };
    let (observed_token, claimant_is_eligible) = observed_capability
        .map(|(token, eligible)| (Some(token), eligible))
        .unwrap_or((None, true));
    let outcome = koth_cycle::apply_observation(
        &mut *control.transaction_mut(),
        ClaimObservation {
            game_id,
            challenge_id,
            target_id: current.target_id,
            cycle_id: current.cycle_id,
            container_id: &current.container_id,
            ad_round_id: round.id,
            token: observed_token,
            status,
            confirmation_ticks: current.claim_confirmation_ticks,
            token_window_complete: current.token_count == current.roster_count,
            claimant_is_eligible,
        },
    )
    .await?;

    let inserted = sqlx::query(
        r#"INSERT INTO "KothControlResults"
             (game_id, challenge_id, ad_round_id,
              controlling_participation_id, responsible_participation_id,
              marker_observed, status, error_message,
              checked_at, dead_container_id, cycle_id, container_id, token_id,
              token_window_round, provisional_participation_id,
              confirmed_participation_id, confirmation_streak,
              is_scorable, void_reason, token_window_attempt)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,
                   $15,$16,$17,$18,$19,$20)
           ON CONFLICT (game_id, challenge_id, ad_round_id) DO NOTHING"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(round.id)
    .bind(outcome.controller)
    .bind(outcome.responsible)
    .bind(marker_observed)
    .bind(status as i16)
    .bind(&message)
    .bind(observed_at)
    .bind(&dead_container_id)
    .bind(current.cycle_id)
    .bind(&current.container_id)
    .bind(outcome.token_id)
    .bind(outcome.token_window_round)
    .bind(outcome.provisional)
    .bind(outcome.confirmed)
    .bind(outcome.confirmation_streak)
    .bind(outcome.is_scorable)
    .bind((!outcome.is_scorable).then_some("platform-attributed checker evidence"))
    .bind(current.token_window_attempt)
    .execute(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    if inserted == 1 {
        sqlx::query(
            r#"WITH holder AS (
               UPDATE "KothTargets"
                  SET holder_participation_id = $2,
                      held_since = CASE
                        WHEN $2::integer IS NULL THEN NULL
                        WHEN holder_participation_id IS DISTINCT FROM $2 THEN $3
                        ELSE held_since END
                WHERE id = $1 AND container_id = $4
                RETURNING id
             )
             UPDATE "KothCrownCycles"
                  SET provisional_participation_id = $5,
                      confirmed_participation_id = $2,
                      confirmation_progress = $6,
                      updated_at = clock_timestamp(),
                      phase = CASE WHEN $9 THEN 'DestroyPending' ELSE phase END,
                      old_container_id = CASE WHEN $9 THEN $4 ELSE old_container_id END,
                      replacement_container_id = CASE WHEN $9 THEN NULL ELSE replacement_container_id END,
                      replacement_host = CASE WHEN $9 THEN NULL ELSE replacement_host END,
                      replacement_port = CASE WHEN $9 THEN NULL ELSE replacement_port END,
                      reset_attempt = reset_attempt + CASE WHEN $9 THEN 1 ELSE 0 END,
                      last_error = CASE
                        WHEN $9 THEN $10
                        WHEN $7::text IS NULL THEN last_error ELSE $7 END
                WHERE id = $8 AND phase = 'Active'
                  AND replacement_container_id = $4"#,
        )
        .bind(current.target_id)
        .bind(outcome.confirmed)
        .bind(observed_at)
        .bind(&current.container_id)
        .bind(outcome.provisional)
        .bind(outcome.confirmation_streak)
        .bind(message.as_deref())
        .bind(current.cycle_id)
        .bind(dead_container_id.is_some())
        .bind("active hill container stopped; recovery reset scheduled")
        .execute(&mut **control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    lifecycle.release().await?;
    Ok(())
}

pub(super) async fn check_hills(
    db: &DatabaseConnection,
    containers: &dyn ContainerManager,
    game_id: i32,
    round: &ad_round::Model,
    checker_dirs: &HashMap<i32, Option<String>>,
    timeout: Duration,
    lease: &RoundFinishLease,
) -> AppResult<()> {
    let challenge_ids: Vec<i32> = sqlx::query_scalar(
        r#"SELECT (frozen.item->>'challengeId')::integer
             FROM "KothOfficialConfigs" config
             CROSS JOIN LATERAL jsonb_array_elements(config.hills_snapshot) frozen(item)
            WHERE config.game_id = $1
            ORDER BY (frozen.item->>'challengeId')::integer"#,
    )
    .bind(game_id)
    .fetch_all(db.get_postgres_connection_pool())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let outcomes = futures::stream::iter(challenge_ids)
        .map(|challenge_id| {
            let checker_dir = checker_dirs.get(&challenge_id).and_then(Option::as_deref);
            async move {
                check_one_hill(
                    db,
                    containers,
                    game_id,
                    challenge_id,
                    round,
                    checker_dir,
                    timeout,
                    lease,
                )
                .await
            }
        })
        .buffer_unordered(checker_concurrency())
        .collect::<Vec<_>>()
        .await;
    let first_error = outcomes.into_iter().find_map(Result::err);
    crate::services::ad_engine::complete_missing_koth_results(db, game_id, round.id, lease).await?;
    first_error.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::{Connection, PgConnection};

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn missing_cycle_still_records_one_platform_void() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
        let mut connection = PgConnection::connect(&database_url).await.unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TEMP TABLE "KothOfficialConfigs" (
              game_id INTEGER NOT NULL
            );
            CREATE TEMP TABLE "KothCrownCycles" (
              id BIGINT PRIMARY KEY, game_id INTEGER NOT NULL,
              challenge_id INTEGER NOT NULL,
              cycle_number INTEGER NOT NULL, planned_start_round INTEGER NOT NULL,
              planned_end_round INTEGER NOT NULL, replacement_container_id TEXT,
              old_container_id TEXT, reset_attempt INTEGER NOT NULL
            );
            CREATE TEMP TABLE "KothControlResults" (
              id BIGSERIAL PRIMARY KEY, game_id INTEGER NOT NULL,
              challenge_id INTEGER NOT NULL, ad_round_id INTEGER NOT NULL,
              controlling_participation_id INTEGER,
              responsible_participation_id INTEGER,
              marker_observed BOOLEAN NOT NULL, status SMALLINT NOT NULL,
              error_message TEXT, checked_at TIMESTAMPTZ NOT NULL,
              cycle_id BIGINT, container_id TEXT, confirmation_streak INTEGER,
              is_scorable BOOLEAN NOT NULL, void_reason TEXT,
              token_window_attempt INTEGER NOT NULL,
              UNIQUE (game_id, challenge_id, ad_round_id)
            );
            "#,
        )
        .execute(&mut connection)
        .await
        .unwrap();
        let now = Utc::now();
        let round = ad_round::Model {
            id: 101,
            game_id: 7,
            number: 4,
            start_time_utc: now,
            end_time_utc: now + chrono::Duration::seconds(30),
            finalized: false,
        };

        insert_missing_cycle_void(&mut connection, 7, 9, &round)
            .await
            .unwrap();
        let void: (Option<i64>, Option<String>, Option<i32>, bool, i16) = sqlx::query_as(
            r#"SELECT cycle_id, container_id, confirmation_streak, is_scorable, status
                 FROM "KothControlResults" WHERE ad_round_id = 101"#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(
            void,
            (None, None, None, false, AdCheckStatus::InternalError as i16)
        );

        sqlx::query(r#"INSERT INTO "KothOfficialConfigs" VALUES (7)"#)
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO "KothCrownCycles" VALUES
                 (41,7,9,2,4,6,'replacement-41','old-41',3)"#,
        )
        .execute(&mut connection)
        .await
        .unwrap();
        let scoped_round = ad_round::Model { id: 102, ..round };
        insert_missing_cycle_void(&mut connection, 7, 9, &scoped_round)
            .await
            .unwrap();
        insert_missing_cycle_void(&mut connection, 7, 9, &scoped_round)
            .await
            .unwrap();
        let scoped: (Option<i64>, Option<String>, Option<i32>, i32) = sqlx::query_as(
            r#"SELECT cycle_id, container_id, confirmation_streak, token_window_attempt
                 FROM "KothControlResults" WHERE ad_round_id = 102"#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(
            scoped,
            (Some(41), Some("replacement-41".to_string()), Some(0), 3)
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                r#"SELECT COUNT(*) FROM "KothControlResults" WHERE ad_round_id = 102"#,
            )
            .fetch_one(&mut connection)
            .await
            .unwrap(),
            1
        );
    }
}
