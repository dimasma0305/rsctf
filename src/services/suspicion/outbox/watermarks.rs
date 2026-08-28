//! Durable per-source cursors and the short game reconciliation lease.

use uuid::Uuid;

use super::{AppError, AppResult, DIRTY_ALL, DIRTY_CORRELATION, DIRTY_EVENT_SECURITY};

const GAME_RECONCILE_LEASE_SECONDS: i64 = 45;
pub(super) const RECONCILE_SOURCE_BATCH: i64 = 512;

#[derive(Debug)]
pub(super) struct GameReconciliationClaim {
    pub(super) token: Uuid,
    pub(super) generation: i64,
    pub(super) dirty_mask: i64,
    pub(super) sources: ReconciliationSourceWindow,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, sqlx::FromRow)]
pub(super) struct ReconciliationSourceWatermarks {
    pub(super) identity_observation_id: i64,
    pub(super) dns_revision: i64,
    pub(super) network_revision: i64,
    pub(super) flag_transport_id: i64,
    pub(super) cheat_info_id: i64,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ReconciliationSourceWindow {
    pub(super) after: ReconciliationSourceWatermarks,
    pub(super) through: ReconciliationSourceWatermarks,
    pub(super) backlog_mask: i64,
}

pub(super) async fn capture_source_window(
    pool: &sqlx::PgPool,
    game_id: i32,
    final_snapshot: bool,
) -> AppResult<ReconciliationSourceWindow> {
    sqlx::query(
        r#"INSERT INTO "SuspicionReconciliationWatermarks" (game_id)
           VALUES ($1) ON CONFLICT (game_id) DO NOTHING"#,
    )
    .bind(game_id)
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let after = sqlx::query_as::<_, ReconciliationSourceWatermarks>(
        r#"SELECT identity_observation_id, dns_revision, network_revision,
                  flag_transport_id, cheat_info_id
             FROM "SuspicionReconciliationWatermarks"
            WHERE game_id = $1"#,
    )
    .bind(game_id)
    .fetch_one(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let limit = if final_snapshot {
        i64::MAX
    } else {
        RECONCILE_SOURCE_BATCH
    };
    let through = sqlx::query_as::<_, ReconciliationSourceWatermarks>(
        r#"SELECT
             COALESCE((SELECT MAX(id) FROM (
               SELECT id FROM "IdentityObservations"
                WHERE game_id = $1 AND id > $2 ORDER BY id LIMIT $7
             ) source), $2) AS identity_observation_id,
             COALESCE((SELECT MAX(reconcile_revision) FROM (
               SELECT reconcile_revision FROM "VpnDnsProviderBuckets"
                WHERE game_id = $1 AND reconcile_revision > $3
                ORDER BY reconcile_revision LIMIT $7
             ) source), $3) AS dns_revision,
             COALESCE((SELECT MAX(reconcile_revision) FROM (
               SELECT reconcile_revision FROM "VpnPeerNetworkObservations"
                WHERE game_id = $1 AND reconcile_revision > $4
                ORDER BY reconcile_revision LIMIT $7
             ) source), $4) AS network_revision,
             COALESCE((SELECT MAX(id) FROM (
               SELECT id FROM "VpnFlagTransportEvents"
                WHERE game_id = $1 AND id > $5 ORDER BY id LIMIT $7
             ) source), $5) AS flag_transport_id,
             COALESCE((SELECT MAX(id) FROM (
               SELECT id FROM "CheatInfo"
                WHERE game_id = $1 AND id > $6 ORDER BY id LIMIT $7
             ) source), $6) AS cheat_info_id"#,
    )
    .bind(game_id)
    .bind(after.identity_observation_id)
    .bind(after.dns_revision)
    .bind(after.network_revision)
    .bind(after.flag_transport_id)
    .bind(after.cheat_info_id)
    .bind(limit)
    .fetch_one(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let remaining: (bool, bool) = sqlx::query_as(
        r#"SELECT
             EXISTS(SELECT 1 FROM "IdentityObservations"
                     WHERE game_id = $1 AND id > $2),
             EXISTS(SELECT 1 FROM "VpnDnsProviderBuckets"
                     WHERE game_id = $1 AND reconcile_revision > $3)
             OR EXISTS(SELECT 1 FROM "VpnPeerNetworkObservations"
                       WHERE game_id = $1 AND reconcile_revision > $4)
             OR EXISTS(SELECT 1 FROM "VpnFlagTransportEvents"
                       WHERE game_id = $1 AND id > $5)
             OR EXISTS(SELECT 1 FROM "CheatInfo"
                       WHERE game_id = $1 AND id > $6)"#,
    )
    .bind(game_id)
    .bind(through.identity_observation_id)
    .bind(through.dns_revision)
    .bind(through.network_revision)
    .bind(through.flag_transport_id)
    .bind(through.cheat_info_id)
    .fetch_one(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(ReconciliationSourceWindow {
        after,
        through,
        backlog_mask: if remaining.0 { DIRTY_CORRELATION } else { 0 }
            | if remaining.1 { DIRTY_EVENT_SECURITY } else { 0 },
    })
}

pub(super) async fn claim_game_reconciliation(
    pool: &sqlx::PgPool,
    game_id: i32,
    force: bool,
) -> AppResult<Option<GameReconciliationClaim>> {
    let token = Uuid::new_v4();
    let claimed = sqlx::query_as::<_, (i64, i64)>(
        r#"UPDATE "SuspicionReconciliationState"
              SET lease_token = $2,
                  lease_expires_at_utc = clock_timestamp()
                    + ($3::bigint * INTERVAL '1 second')
            WHERE game_id = $1
              AND ($4 OR dirty_generation > completed_generation)
              AND (lease_expires_at_utc IS NULL
                   OR lease_expires_at_utc <= clock_timestamp())
        RETURNING dirty_generation, dirty_mask"#,
    )
    .bind(game_id)
    .bind(token)
    .bind(GAME_RECONCILE_LEASE_SECONDS)
    .bind(force)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((generation, dirty_mask)) = claimed else {
        return Ok(None);
    };
    let sources = match capture_source_window(pool, game_id, force).await {
        Ok(sources) => sources,
        Err(error) => {
            sqlx::query(
                r#"UPDATE "SuspicionReconciliationState"
                      SET lease_token = NULL, lease_expires_at_utc = NULL
                    WHERE game_id = $1 AND lease_token = $2"#,
            )
            .bind(game_id)
            .bind(token)
            .execute(pool)
            .await
            .map_err(|release_error| AppError::internal(release_error.to_string()))?;
            return Err(error);
        }
    };
    Ok(Some(GameReconciliationClaim {
        token,
        generation,
        dirty_mask: if force { DIRTY_ALL } else { dirty_mask },
        sources,
    }))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn record_game_reconciliation(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    game_id: i32,
    claim: &GameReconciliationClaim,
    seal: bool,
    inserted: usize,
    errors: &[String],
    source_success_mask: i64,
) -> AppResult<()> {
    let last_error = (!errors.is_empty()).then(|| errors.join("; "));
    let pending_mask = claim.sources.backlog_mask & source_success_mask;
    sqlx::query(
        r#"UPDATE "SuspicionReconciliationWatermarks"
              SET identity_observation_id = CASE WHEN ($2 & $7) <> 0
                    THEN GREATEST(identity_observation_id, $3)
                    ELSE identity_observation_id END,
                  dns_revision = CASE WHEN ($2 & $8) <> 0
                    THEN GREATEST(dns_revision, $4) ELSE dns_revision END,
                  network_revision = CASE WHEN ($2 & $8) <> 0
                    THEN GREATEST(network_revision, $5) ELSE network_revision END,
                  flag_transport_id = CASE WHEN ($2 & $8) <> 0
                    THEN GREATEST(flag_transport_id, $6) ELSE flag_transport_id END,
                  cheat_info_id = CASE WHEN ($2 & $8) <> 0
                    THEN GREATEST(cheat_info_id, $9) ELSE cheat_info_id END,
                  updated_at_utc = clock_timestamp()
            WHERE game_id = $1"#,
    )
    .bind(game_id)
    .bind(source_success_mask)
    .bind(claim.sources.through.identity_observation_id)
    .bind(claim.sources.through.dns_revision)
    .bind(claim.sources.through.network_revision)
    .bind(claim.sources.through.flag_transport_id)
    .bind(DIRTY_CORRELATION)
    .bind(DIRTY_EVENT_SECURITY)
    .bind(claim.sources.through.cheat_info_id)
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let updated = sqlx::query(
        r#"UPDATE "SuspicionReconciliationState"
              SET evidence_closed_at_utc = CASE
                    WHEN $5 THEN COALESCE(evidence_closed_at_utc, clock_timestamp())
                    ELSE evidence_closed_at_utc END,
                  last_reconciled_at_utc = CASE WHEN $6::text IS NULL
                    THEN clock_timestamp() ELSE last_reconciled_at_utc END,
                  sealed_at_utc = CASE WHEN $5 AND $6::text IS NULL
                    THEN COALESCE(sealed_at_utc, clock_timestamp())
                    ELSE sealed_at_utc END,
                  completed_generation = CASE WHEN $6::text IS NULL
                    THEN GREATEST(completed_generation, $3)
                    ELSE completed_generation END,
                  dirty_generation = CASE
                    WHEN $6::text IS NULL AND $7 <> 0
                         AND dirty_generation <= $3
                    THEN $3 + 1 ELSE dirty_generation END,
                  dirty_mask = CASE
                    WHEN $6::text IS NULL AND dirty_generation <= $3
                    THEN (dirty_mask & ~$4) | $7
                    ELSE dirty_mask END,
                  attempts = attempts + 1,
                  last_error = $6,
                  lease_token = NULL,
                  lease_expires_at_utc = NULL
            WHERE game_id = $1 AND lease_token = $2"#,
    )
    .bind(game_id)
    .bind(claim.token)
    .bind(claim.generation)
    .bind(claim.dirty_mask)
    .bind(seal)
    .bind(last_error.as_deref())
    .bind(pending_mask)
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    if updated != 1 {
        return Err(AppError::conflict(
            "Anti-cheat reconciliation lease expired",
        ));
    }
    sqlx::query(
        r#"UPDATE "SuspicionReconciliationOperations"
              SET status = CASE WHEN $5 = 0 THEN 1 ELSE status END,
                  inserted_count = COALESCE(inserted_count, 0) + $4,
                  completed_at_utc = CASE WHEN $5 = 0
                    THEN clock_timestamp() ELSE completed_at_utc END
            WHERE game_id = $1 AND generation <= $2 AND status = 0
              AND $3::text IS NULL"#,
    )
    .bind(game_id)
    .bind(claim.generation)
    .bind(last_error.as_deref())
    .bind(i32::try_from(inserted).unwrap_or(i32::MAX))
    .bind(pending_mask)
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}
