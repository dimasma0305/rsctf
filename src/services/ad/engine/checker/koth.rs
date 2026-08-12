//! Authoritative KotH checker and immutable evidence writer.
//!
//! Marker hills use scheduled crown-cycle identities. Leaderboard/API hills
//! retain one runtime generation until health supervision requires recovery.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use futures::StreamExt;
use sea_orm::DatabaseConnection;

use super::{
    bounded_diagnostic, bounded_optional_diagnostic, checker_concurrency, checker_probe_can_start,
    deadline_limited_probe_budget, koth_api::persist_api_arena_result,
    probe_budget_is_platform_limited, run_check,
};
use crate::models::data::ad_round;
use crate::services::ad::engine::{
    koth_api::{
        read_koth_api_snapshot, stable_koth_api_snapshot, KothApiSnapshot, KothApiSnapshotRead,
    },
    koth_auth,
    koth_cycle::{self, ClaimObservation, ObservedToken},
    koth_marker::{
        observation_precedes_deadline, read_koth_marker, stable_koth_marker, KothMarkerRead,
    },
    AdCheckStatus, RoundFinishLease,
};
use crate::services::container::{ContainerLiveness, ContainerManager};
use crate::utils::enums::{ParticipationStatus, Role};
use crate::utils::error::{AppError, AppResult};

mod scheduling;

use scheduling::{
    api_settlement_start_instant, api_snapshot_arrival_deadline, API_MAX_PROBE_BUDGET,
    API_SNAPSHOT_POLL_INTERVAL, KOTH_COMPLETION_MARGIN,
};

#[derive(Debug, PartialEq, Eq)]
enum ManagedHillLiveness {
    Running,
    Dead(String),
    Unknown(String),
}

const PENDING_KOTH_CHALLENGES_SQL: &str = r#"SELECT (frozen.item->>'challengeId')::integer,
              COALESCE(NULLIF(frozen.item->>'claimSource', ''), 'Marker')
         FROM "KothOfficialConfigs" config
         CROSS JOIN LATERAL jsonb_array_elements(config.hills_snapshot) frozen(item)
        WHERE config.game_id = $1
          AND NOT EXISTS (
                SELECT 1 FROM "KothControlResults" result
                 WHERE result.game_id = config.game_id
                   AND result.challenge_id = (frozen.item->>'challengeId')::integer
                   AND result.ad_round_id = $2
              )
        ORDER BY (frozen.item->>'challengeId')::integer"#;

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
pub(super) struct LiveHill {
    pub(super) target_id: i32,
    pub(super) challenge_id: i32,
    host: String,
    port: i32,
    pub(super) container_id: String,
    pub(super) cycle_id: i64,
    pub(super) token_window_attempt: i32,
    phase: String,
    claim_source: String,
    claim_confirmation_ticks: i32,
    token_count: i64,
    roster_count: i64,
    pub(super) eligible_roster: Vec<i32>,
    game_start: chrono::DateTime<Utc>,
    game_end: chrono::DateTime<Utc>,
    pub(super) round_start: chrono::DateTime<Utc>,
    pub(super) round_end: chrono::DateTime<Utc>,
}

impl LiveHill {
    fn has_complete_token_window(&self) -> bool {
        self.roster_count >= 2 && self.token_count == self.roster_count
    }
}

enum ClaimInputRead {
    Marker(KothMarkerRead),
    Api(KothApiSnapshotRead),
}

async fn read_claim_input(
    db: &DatabaseConnection,
    containers: &dyn ContainerManager,
    hill: &LiveHill,
    round_id: i32,
) -> ClaimInputRead {
    match hill.claim_source.as_str() {
        "Marker" => {
            ClaimInputRead::Marker(read_koth_marker(containers, Some(&hill.container_id)).await)
        }
        "Api" => ClaimInputRead::Api(
            read_koth_api_snapshot(
                db.get_postgres_connection_pool(),
                hill.target_id,
                hill.cycle_id,
                hill.token_window_attempt,
                &hill.container_id,
                round_id,
                hill.round_start,
                hill.round_end,
            )
            .await,
        ),
        source => ClaimInputRead::Marker(KothMarkerRead::Unavailable(format!(
            "unsupported snapshotted KotH claim source {source:?}"
        ))),
    }
}

async fn read_initial_claim_input(
    db: &DatabaseConnection,
    containers: &dyn ContainerManager,
    hill: &LiveHill,
    round_id: i32,
    planned_timeout: Duration,
    effective_deadline: tokio::time::Instant,
) -> ClaimInputRead {
    let wait_until = api_snapshot_arrival_deadline(
        effective_deadline,
        planned_timeout,
        tokio::time::Instant::now(),
    );
    loop {
        let input = read_claim_input(db, containers, hill, round_id).await;
        if hill.claim_source != "Api"
            || matches!(input, ClaimInputRead::Api(KothApiSnapshotRead::Observed(_)))
        {
            return input;
        }
        let now = tokio::time::Instant::now();
        if now >= wait_until {
            return input;
        }
        tokio::time::sleep(std::cmp::min(
            API_SNAPSHOT_POLL_INTERVAL,
            wait_until.saturating_duration_since(now),
        ))
        .await;
    }
}

fn stable_claim_input(
    before: ClaimInputRead,
    after: ClaimInputRead,
) -> (
    Option<String>,
    bool,
    Option<KothApiSnapshot>,
    Option<String>,
) {
    match (before, after) {
        (ClaimInputRead::Marker(before), ClaimInputRead::Marker(after)) => {
            let (marker, observed, error) = stable_koth_marker(before, after);
            (marker, observed, None, error)
        }
        (ClaimInputRead::Api(before), ClaimInputRead::Api(after)) => {
            let (snapshot, error) = stable_koth_api_snapshot(before, after);
            let observed = snapshot.is_some();
            (None, observed, snapshot, error)
        }
        _ => (
            None,
            false,
            None,
            Some("KotH claim source changed during the functional probe".to_string()),
        ),
    }
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
                  cycle.phase,
                  COALESCE(NULLIF(frozen.item->>'claimSource', ''), 'Marker')
                    AS claim_source,
                  config.claim_confirmation_ticks,
                  CASE
                    WHEN COALESCE(NULLIF(frozen.item->>'claimSource', ''), 'Marker') = 'Api'
                    THEN (SELECT COUNT(*) FROM "KothApiTeamTokens" token
                           WHERE token.game_id = target.game_id
                             AND token.challenge_id = target.challenge_id
                             AND token.participation_id = ANY(eligible.participation_ids))
                    ELSE (SELECT COUNT(*) FROM "KothTokens" token
                           WHERE token.cycle_id = cycle.id
                             AND token.challenge_id = target.challenge_id
                             AND token.target_id = target.id
                             AND token.reset_attempt = cycle.reset_attempt
                             AND token.participation_id = ANY(eligible.participation_ids)
                             AND token.revoked_at IS NULL)
                  END AS token_count,
                  cardinality(eligible.participation_ids)::bigint AS roster_count,
                  eligible.participation_ids AS eligible_roster,
                  game.start_time_utc AS game_start,
                  game.end_time_utc AS game_end,
                  scoring_round.start_time_utc AS round_start,
                  scoring_round.end_time_utc AS round_end
             FROM "KothTargets" target
             JOIN "Games" game ON game.id = target.game_id
             JOIN "KothOfficialConfigs" config
               ON config.game_id = target.game_id
             JOIN LATERAL (
               SELECT item
                 FROM jsonb_array_elements(config.hills_snapshot) item
                WHERE (item->>'challengeId')::integer = target.challenge_id
                LIMIT 1
             ) frozen ON TRUE
             JOIN "AdRounds" scoring_round
               ON scoring_round.id = $3 AND scoring_round.game_id = target.game_id
             JOIN LATERAL (
               SELECT COALESCE(
                          array_agg(participation.id ORDER BY participation.id),
                          ARRAY[]::integer[]
                      ) AS participation_ids
                 FROM jsonb_array_elements(config.roster_snapshot) frozen(item)
                 JOIN "Participations" participation
                   ON participation.id = CASE jsonb_typeof(frozen.item)
                        WHEN 'number' THEN (frozen.item #>> '{}')::integer
                        WHEN 'object' THEN
                          NULLIF(frozen.item->>'participationId', '')::integer
                        ELSE NULL
                      END
                  AND participation.game_id = target.game_id
                  AND participation.status = $4
                 JOIN "Teams" team ON team.id = participation.team_id
                WHERE NOT team.deletion_pending
                  AND NOT EXISTS (
                      SELECT 1
                        FROM (
                            SELECT team.captain_id AS user_id
                            UNION
                            SELECT member.user_id
                              FROM "TeamMembers" member
                             WHERE member.team_id = team.id
                        ) roster
                        LEFT JOIN "AspNetUsers" account
                          ON account.id = roster.user_id
                       WHERE account.id IS NULL OR account.role = $5
                  )
             ) eligible ON TRUE
             JOIN LATERAL (
               SELECT crown.* FROM "KothCrownCycles" crown
                WHERE crown.game_id = target.game_id
                  AND crown.challenge_id = target.challenge_id
                  AND (
                    COALESCE(NULLIF(frozen.item->>'claimSource', ''), 'Marker') = 'Api'
                    OR scoring_round.number BETWEEN crown.planned_start_round
                                                AND crown.planned_end_round
                  )
                  AND crown.replacement_container_id = target.container_id
                ORDER BY crown.cycle_number DESC LIMIT 1
             ) cycle ON TRUE
            WHERE target.game_id = $1 AND target.challenge_id = $2
              AND scoring_round.finalized = FALSE"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(round.id)
    .bind(ParticipationStatus::Accepted as i16)
    .bind(Role::Banned as i16)
    .fetch_optional(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

async fn reconcile_ineligible_incumbents(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    hill: &LiveHill,
) -> AppResult<()> {
    let ineligible_incumbents: Vec<i32> = sqlx::query_scalar(
        r#"SELECT DISTINCT incumbent.participation_id
             FROM (
                 SELECT target.holder_participation_id AS participation_id
                   FROM "KothTargets" target
                  WHERE target.id = $1
                 UNION ALL
                 SELECT cycle.provisional_participation_id
                   FROM "KothCrownCycles" cycle WHERE cycle.id = $2
                 UNION ALL
                 SELECT cycle.confirmed_participation_id
                   FROM "KothCrownCycles" cycle WHERE cycle.id = $2
                 UNION ALL
                 SELECT claim.provisional_participation_id
                   FROM "KothClaimStates" claim
                  WHERE claim.target_id = $1 AND claim.cycle_id = $2
                 UNION ALL
                 SELECT claim.confirmed_participation_id
                   FROM "KothClaimStates" claim
                  WHERE claim.target_id = $1 AND claim.cycle_id = $2
                 UNION ALL
                 SELECT token.participation_id
                   FROM "KothClaimStates" claim
                   JOIN "KothTokens" token ON token.id = claim.token_id
                  WHERE claim.target_id = $1 AND claim.cycle_id = $2
                 UNION ALL
                 SELECT token.participation_id
                   FROM "KothApiTeamTokens" token
                  WHERE token.game_id = $4
                    AND token.challenge_id = $5
             ) incumbent
            WHERE incumbent.participation_id IS NOT NULL
              AND NOT (incumbent.participation_id = ANY($3))"#,
    )
    .bind(hill.target_id)
    .bind(hill.cycle_id)
    .bind(&hill.eligible_roster)
    .bind(game_id)
    .bind(hill.challenge_id)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    koth_auth::revoke_game_capabilities(connection, game_id, &ineligible_incumbents).await
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
                 JOIN LATERAL jsonb_array_elements(config.hills_snapshot) frozen(item)
                   ON (frozen.item->>'challengeId')::integer = crown.challenge_id
                WHERE crown.game_id = scope.game_id
                  AND crown.challenge_id = scope.challenge_id
                  AND (
                    COALESCE(NULLIF(frozen.item->>'claimSource', ''), 'Marker') = 'Api'
                    OR $6 BETWEEN crown.planned_start_round
                                      AND crown.planned_end_round
                  )
                ORDER BY crown.cycle_number DESC
                LIMIT 1
             ) cycle ON TRUE
           ON CONFLICT (game_id, challenge_id, ad_round_id) DO NOTHING"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(round.id)
    .bind(AdCheckStatus::InternalError as i16)
    .bind("KotH backend is unpublished; lifecycle/readiness sample void")
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

#[allow(clippy::too_many_arguments)]
async fn check_one_hill(
    db: &DatabaseConnection,
    containers: &dyn ContainerManager,
    game_id: i32,
    challenge_id: i32,
    round: &ad_round::Model,
    checker_dir: Option<&str>,
    planned_timeout: Option<Duration>,
    configured_timeout: Duration,
    effective_deadline: tokio::time::Instant,
    lease: &RoundFinishLease,
) -> AppResult<()> {
    // Runtime lifecycle and checker ownership use the exact same hill lock. The
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
    reconcile_ineligible_incumbents(&mut *control.transaction_mut(), game_id, &hill).await?;
    let complete_tokens = hill.has_complete_token_window();
    if hill.phase != "Active" || !complete_tokens {
        let reason = if hill.phase != "Active" {
            std::borrow::Cow::Owned(format!(
                "KotH lifecycle is {}; reset/readiness sample void",
                hill.phase
            ))
        } else {
            std::borrow::Cow::Borrowed("eligible KotH capability field is incomplete; sample void")
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
    let (marker, marker_observed, api_snapshot, status, message, dead_container_id) = match liveness
    {
        ManagedHillLiveness::Dead(container_id) => (
            None,
            false,
            None,
            AdCheckStatus::Offline,
            Some("managed hill container is not running".to_string()),
            Some(container_id),
        ),
        ManagedHillLiveness::Unknown(error) => (
            None,
            false,
            None,
            AdCheckStatus::InternalError,
            Some(format!("managed hill liveness is unknown: {error}")),
            None,
        ),
        ManagedHillLiveness::Running => {
            let can_start = planned_timeout.is_some_and(|budget| {
                checker_probe_can_start(
                    effective_deadline,
                    budget,
                    KOTH_COMPLETION_MARGIN,
                    tokio::time::Instant::now(),
                )
            });
            if !can_start {
                (
                    None,
                    false,
                    None,
                    AdCheckStatus::InternalError,
                    Some("KotH checker has no safe execution and persistence runway".to_string()),
                    None,
                )
            } else {
                let timeout = planned_timeout.expect("checked above");
                let before = read_initial_claim_input(
                    db,
                    containers,
                    &hill,
                    round.id,
                    timeout,
                    effective_deadline,
                )
                .await;
                if !checker_probe_can_start(
                    effective_deadline,
                    timeout,
                    KOTH_COMPLETION_MARGIN,
                    tokio::time::Instant::now(),
                ) {
                    (
                        None,
                        false,
                        None,
                        AdCheckStatus::InternalError,
                        Some(
                            "KotH evidence read consumed the safe checker execution runway"
                                .to_string(),
                        ),
                        None,
                    )
                } else {
                    let (status, message) = run_check(
                        checker_dir,
                        &hill.host,
                        hill.port,
                        round.number,
                        0,
                        challenge_id,
                        None,
                        timeout,
                        probe_budget_is_platform_limited(timeout, configured_timeout),
                    )
                    .await;
                    let after = read_claim_input(db, containers, &hill, round.id).await;
                    let (marker, observed, snapshot, evidence_error) =
                        stable_claim_input(before, after);
                    let message = match (message, evidence_error) {
                        (Some(checker), Some(evidence)) => {
                            Some(format!("{checker}; {}", bounded_diagnostic(evidence)))
                        }
                        (Some(checker), None) => Some(checker),
                        (None, Some(evidence)) => Some(bounded_diagnostic(evidence)),
                        (None, None) => None,
                    };
                    (marker, observed, snapshot, status, message, None)
                }
            }
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
    reconcile_ineligible_incumbents(&mut *control.transaction_mut(), game_id, &current).await?;
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
        || current.claim_source != hill.claim_source
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
    if current.phase != "Active" || !current.has_complete_token_window() {
        let reason = if current.phase != "Active" {
            format!(
                "KotH lifecycle changed to {}; post-probe sample void",
                current.phase
            )
        } else {
            "live KotH capability field changed during probe; sample void".to_string()
        };
        insert_void(
            &mut *control.transaction_mut(),
            &current,
            game_id,
            round,
            &reason,
        )
        .await?;
        control
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        lifecycle.release().await?;
        return Ok(());
    }

    if current.claim_source == "Api" {
        persist_api_arena_result(
            &mut *control.transaction_mut(),
            &current,
            game_id,
            round,
            status,
            message.as_deref(),
            observed_at,
            dead_container_id.as_deref(),
            api_snapshot.as_ref(),
        )
        .await?;
        control
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        lifecycle.release().await?;
        return Ok(());
    }
    if current.claim_source != "Marker" {
        insert_void(
            &mut *control.transaction_mut(),
            &current,
            game_id,
            round,
            "unsupported snapshotted KotH claim source",
        )
        .await?;
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
                  AND token.participation_id = ANY($9)
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
        .bind(&current.eligible_roster)
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
            token_window_complete: current.has_complete_token_window(),
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

#[allow(clippy::too_many_arguments)]
pub(super) async fn check_hills(
    db: &DatabaseConnection,
    containers: &dyn ContainerManager,
    game_id: i32,
    round: &ad_round::Model,
    checker_dirs: &HashMap<i32, Option<String>>,
    timeout: Duration,
    lease: &RoundFinishLease,
    effective_deadline: tokio::time::Instant,
    tick_seconds: i32,
) -> AppResult<()> {
    // Recovery must never re-run a completed hill. The immutable duplicate
    // fence in `check_one_hill` remains necessary for concurrent owners, while
    // this prefilter preserves capacity for genuinely unresolved hills.
    let challenges: Vec<(i32, String)> = sqlx::query_as(PENDING_KOTH_CHALLENGES_SQL)
        .bind(game_id)
        .bind(round.id)
        .fetch_all(db.get_postgres_connection_pool())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    // All hills receive the same planned budget. A hill delayed behind local
    // locks or bounded concurrency must still fit that complete budget at its
    // actual start; otherwise its sample is platform-attributed and void.
    let budget_now = tokio::time::Instant::now();
    let nominal_timeout = deadline_limited_probe_budget(
        budget_now + Duration::from_secs(u64::try_from(tick_seconds.clamp(30, 600)).unwrap_or(60)),
        timeout,
        KOTH_COMPLETION_MARGIN,
        budget_now,
    );
    let planned_timeout = nominal_timeout.and_then(|nominal| {
        deadline_limited_probe_budget(
            effective_deadline,
            nominal,
            KOTH_COMPLETION_MARGIN,
            budget_now,
        )
    });
    let challenge_count = challenges.len();
    let permits = Arc::new(tokio::sync::Semaphore::new(checker_concurrency()));
    let outcomes = futures::stream::iter(challenges)
        .map(|(challenge_id, claim_source)| {
            let checker_dir = checker_dirs.get(&challenge_id).and_then(Option::as_deref);
            let is_api = claim_source == "Api";
            let permits = Arc::clone(&permits);
            let challenge_timeout = if is_api {
                planned_timeout.map(|timeout| timeout.min(API_MAX_PROBE_BUDGET))
            } else {
                planned_timeout
            };
            async move {
                // Leaderboard evidence remains appendable until the fixed
                // settlement cutoff. Waiting here, before any hill/lifecycle
                // lock is acquired, lets the referee publish the complete
                // contiguous window without blocking resets or other hills.
                if is_api {
                    tokio::time::sleep_until(api_settlement_start_instant(
                        round.end_time_utc,
                        Utc::now(),
                        tokio::time::Instant::now(),
                    ))
                    .await;
                }
                // API hills wait without consuming checker capacity. This
                // keeps marker hills responsive even when more than one
                // concurrency batch is waiting on the settlement cutoff.
                let _permit = permits
                    .acquire_owned()
                    .await
                    .map_err(|_| AppError::internal("KotH checker capacity closed unexpectedly"))?;
                check_one_hill(
                    db,
                    containers,
                    game_id,
                    challenge_id,
                    round,
                    checker_dir,
                    challenge_timeout,
                    nominal_timeout.unwrap_or(timeout),
                    effective_deadline,
                    lease,
                )
                .await
            }
        })
        .buffer_unordered(challenge_count.max(1))
        .collect::<Vec<_>>()
        .await;
    let first_error = outcomes.into_iter().find_map(Result::err);
    crate::services::ad_engine::complete_missing_koth_results(db, game_id, round.id, lease).await?;
    first_error.map_or(Ok(()), Err)
}

#[cfg(test)]
#[path = "koth_tests.rs"]
mod tests;
