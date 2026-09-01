//! Incremental, lease-fenced anti-cheat reconciliation.
//!
//! Evidence transactions advance source watermarks. A control-plane job then
//! claims one game without retaining a transaction, processes a bounded slice,
//! and advances only the captured cursors. Evidence committed during the pass
//! remains dirty for the next generation.

use std::future::Future;
use std::time::Duration;

use uuid::Uuid;

use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};

use super::detectors::ReconciliationSnapshot;

pub(super) const SOURCE_SUBMISSION: i16 = 0;
pub(super) const SOURCE_IDENTITY: i16 = 1;
// Reserved for schema compatibility. Exemptions are temporal intervals: a
// grant/revocation never changes an already-observed edge, and later identity
// observations advance SOURCE_IDENTITY themselves.
pub(super) const SOURCE_EXEMPTION: i16 = 2;
pub(super) const SOURCE_VPN_DNS: i16 = 3;
pub(super) const SOURCE_VPN_PEER: i16 = 4;
pub(super) const SOURCE_VPN_FLAG: i16 = 5;
pub(super) const SOURCE_CONTAINER_ACCESS: i16 = 6;
pub(super) const SOURCE_SUSPICION_EVENT: i16 = 7;
pub(super) const SOURCE_CHEAT_INFO: i16 = 8;
pub(super) const SOURCE_ROSTER: i16 = 9;

const CLAIM_SECONDS: i64 = 60;
const PASS_DEADLINE: Duration = Duration::from_secs(45);
pub(crate) const SOURCE_BATCH: i64 = 256;
const MAX_ELIGIBLE_GAMES: i64 = 32;

pub(super) const ELIGIBLE_GAMES_SQL: &str = r#"
    WITH observed_clock AS MATERIALIZED (
      SELECT clock_timestamp() AS db_now
    ), candidates AS MATERIALIZED (
      (
        SELECT queue.game_id, FALSE AS barrier_backed_final,
               queue.updated_at_utc AS priority_at_utc
          FROM "AntiCheatReconciliationQueue" queue
          JOIN "Games" game ON game.id = queue.game_id
          JOIN "SuspicionReconciliationState" reconciliation
            ON reconciliation.game_id = queue.game_id
          CROSS JOIN observed_clock
         WHERE game.deletion_pending = FALSE
           AND game.start_time_utc <= observed_clock.db_now
           AND game.end_time_utc > observed_clock.db_now
           AND reconciliation.evidence_closed_at_utc IS NULL
           AND reconciliation.sealed_at_utc IS NULL
           AND queue.desired_generation > queue.applied_generation
           AND queue.available_at_utc <= observed_clock.db_now
           AND (queue.lease_expires_at_utc IS NULL
                OR queue.lease_expires_at_utc <= observed_clock.db_now)
         ORDER BY queue.available_at_utc, queue.updated_at_utc, queue.game_id
         LIMIT $2
      )
      UNION ALL
      (
        -- Drive terminal discovery from the bounded game end-time index. Clean
        -- active/upcoming queue rows are therefore not revisited every poll.
        SELECT queue.game_id, TRUE AS barrier_backed_final,
               game.end_time_utc AS priority_at_utc
          FROM "Games" game
          JOIN "AntiCheatReconciliationQueue" queue ON queue.game_id = game.id
          JOIN "SuspicionReconciliationState" reconciliation
            ON reconciliation.game_id = game.id
          CROSS JOIN observed_clock
         WHERE game.deletion_pending = FALSE
           AND game.end_time_utc
                 + ($1::bigint * INTERVAL '1 second') <= observed_clock.db_now
           AND reconciliation.sealed_at_utc IS NULL
           AND queue.final_applied_at_utc IS NULL
           AND (
                 reconciliation.evidence_closed_at_utc IS NULL
                 OR NOT EXISTS (
                     SELECT 1
                       FROM "SuspicionEvaluationOutbox" evaluation
                      WHERE evaluation.game_id = game.id
                        AND evaluation.completed_at_utc IS NULL
                        AND evaluation.observed_at_utc >= game.start_time_utc
                        AND evaluation.observed_at_utc < game.end_time_utc
                 )
               )
           AND queue.available_at_utc <= observed_clock.db_now
           AND (queue.lease_expires_at_utc IS NULL
                OR queue.lease_expires_at_utc <= observed_clock.db_now)
         ORDER BY game.end_time_utc, game.id
         LIMIT $2
      )
    )
    SELECT game_id, barrier_backed_final
      FROM candidates
     ORDER BY barrier_backed_final DESC, priority_at_utc, game_id
     LIMIT $2
"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq, sqlx::FromRow)]
pub(crate) struct SourceCursor {
    pub kind: i16,
    pub after: i64,
    pub through: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct SourceRow {
    source_kind: i16,
    applied_version: i64,
    dirty_version: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct ClaimRow {
    desired_generation: i64,
    attempts: i32,
    final_requested: bool,
}

#[derive(Debug)]
struct ReconciliationClaim {
    game_id: i32,
    lease_token: Uuid,
    desired_generation: i64,
    attempts: i32,
    final_snapshot: bool,
    cursors: Vec<SourceCursor>,
}

fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

pub(super) async fn eligible_games(
    pool: &sqlx::PgPool,
    finalize_grace_seconds: u64,
) -> AppResult<Vec<(i32, bool)>> {
    sqlx::query_as(ELIGIBLE_GAMES_SQL)
        .bind(i64::try_from(finalize_grace_seconds).expect("validated grace fits i64"))
        .bind(MAX_ELIGIBLE_GAMES)
        .fetch_all(pool)
        .await
        .map_err(database_error)
}

pub(super) async fn request_final_if_ready(
    pool: &sqlx::PgPool,
    game_id: i32,
    finalize_grace_seconds: u64,
) -> AppResult<()> {
    if !super::outbox::close_competitive_evidence_window(pool, game_id, finalize_grace_seconds)
        .await?
    {
        return Ok(());
    }
    if super::outbox::incomplete_competitive_jobs(pool, game_id).await? != 0 {
        return Ok(());
    }
    sqlx::query(
        r#"UPDATE "AntiCheatReconciliationQueue"
              SET final_requested_at_utc = COALESCE(
                      final_requested_at_utc, clock_timestamp()
                  ),
                  desired_generation = desired_generation
                    + CASE WHEN final_requested_at_utc IS NULL THEN 1 ELSE 0 END,
                  available_at_utc = LEAST(available_at_utc, clock_timestamp()),
                  updated_at_utc = clock_timestamp()
            WHERE game_id = $1 AND final_applied_at_utc IS NULL"#,
    )
    .bind(game_id)
    .execute(pool)
    .await
    .map_err(database_error)?;
    Ok(())
}

async fn bounded_target(pool: &sqlx::PgPool, game_id: i32, source: &SourceRow) -> AppResult<i64> {
    // These sources are wakeups for the authoritative final sweep rather than
    // live row consumers. Acknowledge the captured commit-ordered high-water
    // mark in one O(1) pass instead of paging and reading rows we cannot safely
    // turn into immutable live findings.
    if final_only_source_reason(source.source_kind).is_some() {
        return Ok(source.dirty_version);
    }
    let table_filter = match source.source_kind {
        SOURCE_SUBMISSION => Some(("SuspicionEvaluationOutbox", "completed_at_utc IS NOT NULL")),
        SOURCE_IDENTITY => Some(("IdentityObservations", "TRUE")),
        SOURCE_VPN_DNS => Some(("VpnDnsProviderBuckets", "TRUE")),
        SOURCE_VPN_PEER => Some(("VpnPeerNetworkObservations", "TRUE")),
        SOURCE_VPN_FLAG => Some(("VpnFlagTransportEvents", "TRUE")),
        SOURCE_SUSPICION_EVENT => Some(("SuspicionEvents", "TRUE")),
        SOURCE_CHEAT_INFO => Some(("CheatInfo", "TRUE")),
        SOURCE_EXEMPTION | SOURCE_CONTAINER_ACCESS | SOURCE_ROSTER => None,
        _ => return Err(AppError::internal("unknown anti-cheat source kind")),
    };
    let Some((table, filter)) = table_filter else {
        return Ok(source.dirty_version);
    };
    // `table` and `filter` are closed constants, never domain input.
    let sql = format!(
        r#"SELECT MAX(reconciliation_version)::bigint FROM (
              SELECT reconciliation_version FROM "{table}"
               WHERE game_id = $1
                 AND reconciliation_version > $2
                 AND reconciliation_version <= $3
                 AND {filter}
               ORDER BY reconciliation_version LIMIT $4
           ) bounded"#
    );
    let target: Option<i64> = sqlx::query_scalar(&sql)
        .bind(game_id)
        .bind(source.applied_version)
        .bind(source.dirty_version)
        .bind(SOURCE_BATCH)
        .fetch_one(pool)
        .await
        .map_err(database_error)?;
    // A seeded or metadata-only version need not have a matching source row.
    Ok(target.unwrap_or(source.dirty_version))
}

async fn claim_reconciliation(
    pool: &sqlx::PgPool,
    game_id: i32,
) -> AppResult<Option<ReconciliationClaim>> {
    let lease_token = Uuid::new_v4();
    let claimed = sqlx::query_as::<_, ClaimRow>(
        r#"UPDATE "AntiCheatReconciliationQueue" queue
              SET lease_token = $2,
                  lease_expires_at_utc = clock_timestamp()
                    + ($3::bigint * INTERVAL '1 second'),
                  attempts = queue.attempts + 1,
                  last_started_at_utc = clock_timestamp(),
                  updated_at_utc = clock_timestamp()
             FROM "SuspicionReconciliationState" reconciliation
            WHERE queue.game_id = $1
              AND reconciliation.game_id = queue.game_id
              AND reconciliation.sealed_at_utc IS NULL
              AND queue.available_at_utc <= clock_timestamp()
              AND (queue.lease_expires_at_utc IS NULL
                   OR queue.lease_expires_at_utc <= clock_timestamp())
        RETURNING queue.desired_generation, queue.attempts,
                  queue.final_requested_at_utc IS NOT NULL
                    AND queue.final_applied_at_utc IS NULL AS final_requested"#,
    )
    .bind(game_id)
    .bind(lease_token)
    .bind(CLAIM_SECONDS)
    .fetch_optional(pool)
    .await
    .map_err(database_error)?;
    let Some(claimed) = claimed else {
        return Ok(None);
    };
    let dirty = sqlx::query_as::<_, SourceRow>(
        r#"SELECT source_kind, applied_version, dirty_version
             FROM "AntiCheatReconciliationSources"
            WHERE game_id = $1 AND dirty_version > applied_version
            ORDER BY source_kind"#,
    )
    .bind(game_id)
    .fetch_all(pool)
    .await
    .map_err(database_error)?;
    let mut cursors = Vec::with_capacity(dirty.len());
    for source in dirty {
        let through = bounded_target(pool, game_id, &source).await?;
        cursors.push(SourceCursor {
            kind: source.source_kind,
            after: source.applied_version,
            through,
        });
    }
    Ok(Some(ReconciliationClaim {
        game_id,
        lease_token,
        desired_generation: claimed.desired_generation,
        attempts: claimed.attempts,
        final_snapshot: claimed.final_requested,
        cursors,
    }))
}

fn cursor(claim: &ReconciliationClaim, kind: i16) -> Option<SourceCursor> {
    claim
        .cursors
        .iter()
        .copied()
        .find(|cursor| cursor.kind == kind)
}

fn final_only_source_reason(kind: i16) -> Option<&'static str> {
    match kind {
        // Cross-team access is already projected through the durable source-0
        // outbox. Timing and IP aggregates can be retracted by a later access
        // row, so immutable aggregate findings require the final barrier.
        SOURCE_CONTAINER_ACCESS => Some("retractable container-access aggregate"),
        // Admission/status changes can alter the eligible comparison
        // population for old evidence. Population-relative immutable findings
        // are therefore recomputed only from the authoritative final roster.
        SOURCE_ROSTER => Some("non-monotonic roster population"),
        _ => None,
    }
}

async fn within_deadline<T>(
    deadline: tokio::time::Instant,
    stage: &'static str,
    future: impl Future<Output = AppResult<T>>,
) -> AppResult<T> {
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() {
        return Err(AppError::unavailable(format!(
            "anti-cheat reconciliation deadline reached before {stage}"
        )));
    }
    tokio::time::timeout(remaining, future)
        .await
        .map_err(|_| AppError::unavailable(format!("anti-cheat {stage} exceeded its deadline")))?
}

async fn run_live_pass(
    state: &SharedState,
    claim: &ReconciliationClaim,
    deadline: tokio::time::Instant,
) -> AppResult<usize> {
    for deferred in &claim.cursors {
        let Some(reason) = final_only_source_reason(deferred.kind) else {
            continue;
        };
        tracing::debug!(
            game_id = claim.game_id,
            source_kind = deferred.kind,
            through_version = deferred.through,
            reason,
            "anti-cheat source delta is explicitly deferred to the final barrier"
        );
    }
    let submissions = cursor(claim, SOURCE_SUBMISSION);
    if let Some(submissions) = submissions {
        // Only monotonic, delta-anchored cadence work runs live. Cheat-stat's
        // population-relative rules persist immutable events and can change as
        // later solves arrive, so the unconditional final sweep owns them.
        within_deadline(
            deadline,
            "submission cadence",
            super::cheat_checks::run_abnormal_solve_checks_incremental(
                state,
                claim.game_id,
                submissions,
            ),
        )
        .await?;
    }
    let identity = cursor(claim, SOURCE_IDENTITY);
    if identity.is_some() {
        within_deadline(
            deadline,
            "identity correlation",
            super::correlation::run_correlation_checks_incremental(
                &state.db,
                claim.game_id,
                identity,
            ),
        )
        .await?;
    }
    let fusion = crate::services::event_security::FusionCursors {
        dns: cursor(claim, SOURCE_VPN_DNS),
        peer: cursor(claim, SOURCE_VPN_PEER),
        flag: cursor(claim, SOURCE_VPN_FLAG),
        suspicion: cursor(claim, SOURCE_SUSPICION_EVENT),
        cheat: cursor(claim, SOURCE_CHEAT_INFO),
    };
    if fusion.has_work() {
        within_deadline(
            deadline,
            "event-security fusion",
            crate::services::event_security::derive_context_findings_incremental(
                state,
                claim.game_id,
                fusion,
            ),
        )
        .await
    } else {
        // A clean manual request is normally aliased to the generation's
        // completed receipt. If no receipt exists (for example immediately
        // after migration), establishing it is still a zero-history no-op.
        Ok(0)
    }
}

async fn run_final_pass(
    state: &SharedState,
    claim: &ReconciliationClaim,
    deadline: tokio::time::Instant,
) -> AppResult<usize> {
    let snapshot = ReconciliationSnapshot::BarrierBackedFinal;
    within_deadline(
        deadline,
        "final abnormal-solve sweep",
        super::cheat_checks::run_abnormal_solve_checks_for_snapshot(state, claim.game_id, snapshot),
    )
    .await?;
    within_deadline(
        deadline,
        "final statistical sweep",
        super::cheat_stat::run_statistical_checks_for_snapshot(state, claim.game_id, snapshot),
    )
    .await?;
    within_deadline(
        deadline,
        "final identity sweep",
        super::correlation::run_correlation_checks_for_snapshot(&state.db, claim.game_id, snapshot),
    )
    .await?;
    within_deadline(
        deadline,
        "final container-access sweep",
        super::container_access::run_container_access_checks_for_snapshot(
            state,
            claim.game_id,
            snapshot,
        ),
    )
    .await?;
    within_deadline(
        deadline,
        "final honeypot sweep",
        super::run_honeypot_chain_checks(state, claim.game_id),
    )
    .await?;
    // Fusion runs last so relationships include SuspicionEvents emitted by
    // every preceding detector in this barrier-backed generation.
    within_deadline(
        deadline,
        "final event-security fusion",
        crate::services::event_security::derive_context_findings_full(state, claim.game_id),
    )
    .await
}

async fn finish_success(pool: &sqlx::PgPool, claim: &ReconciliationClaim) -> AppResult<()> {
    let mut transaction = pool.begin().await.map_err(database_error)?;
    // Participation admission takes this state row before its reconciliation
    // stamp takes the queue row. Preserve the same State -> Queue -> Sources
    // order here so a roster update cannot deadlock pass completion.
    let state_exists = sqlx::query_scalar::<_, bool>(
        r#"SELECT TRUE FROM "SuspicionReconciliationState"
            WHERE game_id = $1
            FOR UPDATE"#,
    )
    .bind(claim.game_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(database_error)?
    .unwrap_or(false);
    if !state_exists {
        return Err(AppError::internal(
            "anti-cheat reconciliation state not found",
        ));
    }
    let owns_lease: bool = sqlx::query_scalar(
        r#"SELECT lease_token = $2
             FROM "AntiCheatReconciliationQueue"
            WHERE game_id = $1
            FOR UPDATE"#,
    )
    .bind(claim.game_id)
    .bind(claim.lease_token)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(database_error)?
    .unwrap_or(false);
    if !owns_lease {
        return Err(AppError::conflict(
            "anti-cheat reconciliation lost its durable lease",
        ));
    }
    if claim.final_snapshot {
        sqlx::query(
            r#"UPDATE "AntiCheatReconciliationSources"
                  SET applied_version = dirty_version,
                      applied_at_utc = clock_timestamp()
                WHERE game_id = $1"#,
        )
        .bind(claim.game_id)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
    } else {
        for cursor in &claim.cursors {
            sqlx::query(
                r#"UPDATE "AntiCheatReconciliationSources"
                      SET applied_version = GREATEST(applied_version, $3),
                          applied_at_utc = clock_timestamp()
                    WHERE game_id = $1 AND source_kind = $2
                      AND dirty_version >= $3"#,
            )
            .bind(claim.game_id)
            .bind(cursor.kind)
            .bind(cursor.through)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        }
    }
    super::outbox::record_game_reconciliation(
        &mut transaction,
        claim.game_id,
        claim.final_snapshot,
        &[],
    )
    .await?;
    let affected = sqlx::query(
        r#"UPDATE "AntiCheatReconciliationQueue" queue
              SET applied_generation = CASE
                    WHEN NOT EXISTS (
                      SELECT 1 FROM "AntiCheatReconciliationSources" source
                       WHERE source.game_id = queue.game_id
                         AND source.dirty_version > source.applied_version
                    ) THEN queue.desired_generation
                    ELSE queue.applied_generation
                  END,
                  final_applied_at_utc = CASE WHEN $4
                    THEN clock_timestamp() ELSE final_applied_at_utc END,
                  lease_token = NULL, lease_expires_at_utc = NULL,
                  available_at_utc = clock_timestamp(),
                  last_completed_at_utc = clock_timestamp(),
                  last_error = NULL, updated_at_utc = clock_timestamp()
            WHERE queue.game_id = $1 AND queue.lease_token = $2
              AND queue.desired_generation >= $3"#,
    )
    .bind(claim.game_id)
    .bind(claim.lease_token)
    .bind(claim.desired_generation)
    .bind(claim.final_snapshot)
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?;
    if affected.rows_affected() != 1 {
        return Err(AppError::conflict(
            "anti-cheat reconciliation lost its durable lease",
        ));
    }
    transaction.commit().await.map_err(database_error)?;
    Ok(())
}

async fn finish_failure(
    pool: &sqlx::PgPool,
    claim: &ReconciliationClaim,
    error: &str,
) -> AppResult<()> {
    let exponent = u32::try_from(claim.attempts.clamp(1, 8)).unwrap_or(8);
    let delay = 1_i64.checked_shl(exponent).unwrap_or(300).min(300);
    let mut message = error.to_owned();
    while message.len() > 4000 {
        message.pop();
    }
    let affected = sqlx::query(
        r#"UPDATE "AntiCheatReconciliationQueue"
              SET lease_token = NULL, lease_expires_at_utc = NULL,
                  available_at_utc = clock_timestamp()
                    + ($3::bigint * INTERVAL '1 second'),
                  last_error = $4, updated_at_utc = clock_timestamp()
            WHERE game_id = $1 AND lease_token = $2"#,
    )
    .bind(claim.game_id)
    .bind(claim.lease_token)
    .bind(delay)
    .bind(&message)
    .execute(pool)
    .await
    .map_err(database_error)?;
    if affected.rows_affected() != 1 {
        return Err(AppError::conflict(
            "anti-cheat reconciliation lost its durable lease",
        ));
    }
    let mut transaction = pool.begin().await.map_err(database_error)?;
    super::outbox::record_game_reconciliation(&mut transaction, claim.game_id, false, &[message])
        .await?;
    transaction.commit().await.map_err(database_error)?;
    Ok(())
}

/// Execute the existing durable `SecurityDerivation` job against one captured
/// generation. A competing replica returns without work instead of waiting on
/// a lock or holding a pool connection.
pub(crate) async fn execute_game_reconciliation(
    state: &SharedState,
    game_id: i32,
) -> AppResult<usize> {
    let finalize_grace_seconds = super::outbox::finalization_grace_seconds();
    request_final_if_ready(state.pg(), game_id, finalize_grace_seconds).await?;
    let Some(claim) = claim_reconciliation(state.pg(), game_id).await? else {
        let sealed = sqlx::query_scalar::<_, bool>(
            r#"SELECT sealed_at_utc IS NOT NULL
                 FROM "SuspicionReconciliationState" WHERE game_id = $1"#,
        )
        .bind(game_id)
        .fetch_optional(state.pg())
        .await
        .map_err(database_error)?
        .ok_or_else(|| AppError::not_found("game reconciliation state not found"))?;
        if sealed {
            return Ok(0);
        }
        return Err(AppError::unavailable(
            "anti-cheat reconciliation is leased or waiting for retry backoff",
        ));
    };
    let deadline = tokio::time::Instant::now() + PASS_DEADLINE;
    let result = if claim.final_snapshot {
        run_final_pass(state, &claim, deadline).await
    } else {
        run_live_pass(state, &claim, deadline).await
    };
    match result {
        Ok(inserted) => {
            finish_success(state.pg(), &claim).await?;
            Ok(inserted)
        }
        Err(error) => {
            finish_failure(state.pg(), &claim, &error.to_string()).await?;
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::*;

    #[test]
    fn eligible_selection_is_dirty_or_final_and_bounded() {
        assert!(ELIGIBLE_GAMES_SQL.contains("desired_generation > queue.applied_generation"));
        assert!(ELIGIBLE_GAMES_SQL.contains("queue.final_applied_at_utc IS NULL"));
        assert!(ELIGIBLE_GAMES_SQL.contains("UNION ALL"));
        assert!(ELIGIBLE_GAMES_SQL.contains("ORDER BY queue.available_at_utc"));
        assert!(ELIGIBLE_GAMES_SQL.contains("ORDER BY game.end_time_utc"));
        assert!(ELIGIBLE_GAMES_SQL.contains("evaluation.completed_at_utc IS NULL"));
        assert!(ELIGIBLE_GAMES_SQL.contains("reconciliation.evidence_closed_at_utc IS NULL"));
        assert!(ELIGIBLE_GAMES_SQL.contains("LIMIT $2"));
        assert!(!ELIGIBLE_GAMES_SQL.contains("FOR UPDATE"));
    }

    #[test]
    fn retractable_and_population_sources_are_explicitly_final_only() {
        assert_eq!(
            final_only_source_reason(SOURCE_CONTAINER_ACCESS),
            Some("retractable container-access aggregate")
        );
        assert_eq!(
            final_only_source_reason(SOURCE_ROSTER),
            Some("non-monotonic roster population")
        );
        for incremental in [
            SOURCE_SUBMISSION,
            SOURCE_IDENTITY,
            SOURCE_VPN_DNS,
            SOURCE_VPN_PEER,
            SOURCE_VPN_FLAG,
            SOURCE_SUSPICION_EVENT,
            SOURCE_CHEAT_INFO,
        ] {
            assert_eq!(final_only_source_reason(incremental), None);
        }
    }

    #[test]
    fn final_only_sources_do_not_enter_a_delta_table_scan() {
        let source = include_str!("reconciliation.rs");
        let early_return = source
            .find("if final_only_source_reason(source.source_kind).is_some()")
            .unwrap();
        let table_dispatch = source
            .find("let table_filter = match source.source_kind")
            .unwrap();
        assert!(early_return < table_dispatch);
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn postgres_claim_uses_commit_versions_and_late_dirtiness_survives() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("anticheat_reconcile_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "SuspicionReconciliationState" (
              game_id INTEGER PRIMARY KEY, evidence_closed_at_utc TIMESTAMPTZ,
              sealed_at_utc TIMESTAMPTZ
            );
            CREATE TABLE "Games" (
              id INTEGER PRIMARY KEY, start_time_utc TIMESTAMPTZ NOT NULL,
              end_time_utc TIMESTAMPTZ NOT NULL,
              deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TABLE "AntiCheatReconciliationQueue" (
              game_id INTEGER PRIMARY KEY, desired_generation BIGINT NOT NULL,
              applied_generation BIGINT NOT NULL, final_requested_at_utc TIMESTAMPTZ,
              final_applied_at_utc TIMESTAMPTZ, available_at_utc TIMESTAMPTZ NOT NULL,
              lease_token UUID, lease_expires_at_utc TIMESTAMPTZ, attempts INTEGER NOT NULL,
              last_started_at_utc TIMESTAMPTZ, updated_at_utc TIMESTAMPTZ NOT NULL
            );
            CREATE TABLE "AntiCheatReconciliationSources" (
              game_id INTEGER NOT NULL, source_kind SMALLINT NOT NULL,
              applied_version BIGINT NOT NULL, dirty_version BIGINT NOT NULL,
              PRIMARY KEY (game_id, source_kind)
            );
            CREATE TABLE "SuspicionEvaluationOutbox" (
              id BIGINT PRIMARY KEY, game_id INTEGER NOT NULL,
              completed_at_utc TIMESTAMPTZ, observed_at_utc TIMESTAMPTZ NOT NULL,
              reconciliation_version BIGINT
            );
            INSERT INTO "Games" VALUES
              (1, clock_timestamp() - interval '1 hour',
               clock_timestamp() + interval '1 hour', FALSE);
            INSERT INTO "SuspicionReconciliationState" VALUES (1, NULL, NULL);
            INSERT INTO "AntiCheatReconciliationQueue" VALUES
              (1, 1, 0, NULL, NULL, clock_timestamp(), NULL, NULL, 0, NULL,
               clock_timestamp());
            INSERT INTO "AntiCheatReconciliationSources" VALUES (1, 0, 0, 2);
            INSERT INTO "SuspicionEvaluationOutbox" VALUES
              (200, 1, clock_timestamp(), clock_timestamp(), 1),
              (150, 1, clock_timestamp(), clock_timestamp(), 2);
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let (left, right) = tokio::join!(
            claim_reconciliation(&pool, 1),
            claim_reconciliation(&pool, 1)
        );
        let owners = [left.unwrap(), right.unwrap()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        assert_eq!(owners.len(), 1);
        let claim = &owners[0];
        assert_eq!(
            claim.cursors,
            vec![SourceCursor {
                kind: 0,
                after: 0,
                through: 2
            }]
        );
        sqlx::query(
            r#"UPDATE "AntiCheatReconciliationSources"
                  SET dirty_version = 3 WHERE game_id = 1 AND source_kind = 0"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "SuspicionEvaluationOutbox"
                  VALUES (100, 1, clock_timestamp(), clock_timestamp(), 3)"#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let mut transaction = pool.begin().await.unwrap();
        for cursor in &claim.cursors {
            sqlx::query(
                r#"UPDATE "AntiCheatReconciliationSources"
                      SET applied_version = $3
                    WHERE game_id = $1 AND source_kind = $2"#,
            )
            .bind(claim.game_id)
            .bind(cursor.kind)
            .bind(cursor.through)
            .execute(&mut *transaction)
            .await
            .unwrap();
        }
        transaction.commit().await.unwrap();
        let versions: (i64, i64) = sqlx::query_as(
            r#"SELECT applied_version, dirty_version
                 FROM "AntiCheatReconciliationSources"
                WHERE game_id = 1 AND source_kind = 0"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(versions, (2, 3));

        sqlx::raw_sql(
            r#"UPDATE "Games"
                  SET start_time_utc = clock_timestamp() - interval '2 hours',
                      end_time_utc = clock_timestamp() - interval '1 hour'
                WHERE id = 1;
               UPDATE "SuspicionReconciliationState"
                  SET evidence_closed_at_utc = clock_timestamp() WHERE game_id = 1;
               UPDATE "AntiCheatReconciliationQueue"
                  SET desired_generation = applied_generation,
                      lease_token = NULL, lease_expires_at_utc = NULL
                WHERE game_id = 1;
               INSERT INTO "SuspicionEvaluationOutbox"
                   (id, game_id, completed_at_utc, observed_at_utc, reconciliation_version)
               VALUES (300, 1, NULL, clock_timestamp() - interval '90 minutes', NULL);"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert!(eligible_games(&pool, 0).await.unwrap().is_empty());
        sqlx::query(
            r#"UPDATE "SuspicionEvaluationOutbox"
                  SET completed_at_utc = clock_timestamp(), reconciliation_version = 4
                WHERE id = 300"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(eligible_games(&pool, 0).await.unwrap(), vec![(1, true)]);

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
