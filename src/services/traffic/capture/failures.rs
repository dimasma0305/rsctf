//! Durable handling for capture threads that fail after publication.
//!
//! Capture ownership is process-local, but endpoint reachability is durable.
//! The singleton owner therefore records an unexpected exit and deactivates
//! only the exact endpoint it observed in one PostgreSQL transaction. Network
//! policy reconciliation is deliberately a separate, retryable acknowledgement
//! so a failed firewall apply cannot hold up unrelated capture stop requests.

use std::time::Duration;

use sqlx::{Acquire, PgConnection};

use crate::app_state::SharedState;
use crate::utils::enums::AdCheckStatus;

use super::CaptureFailure;

const FAILURE_RETENTION_DAYS: i32 = 30;
const MAX_ERROR_CHARS: usize = 2_000;

fn bounded_error(error: &str) -> String {
    error.chars().take(MAX_ERROR_CHARS).collect()
}

/// Persist the incident and revoke the exact database endpoint atomically.
/// The identity predicates are the generation fence: a later replacement that
/// reused a service row or backend id is never deactivated by a stale thread.
pub(super) async fn persist_and_deactivate(
    connection: &mut PgConnection,
    failures: &[CaptureFailure],
) -> Result<(), sqlx::Error> {
    for failure in failures {
        let mut transaction = connection.begin().await?;
        let spec = &failure.spec;
        let game_id = sqlx::query_scalar::<_, i32>(
            r#"SELECT game_id
                 FROM "AdTeamServices"
                WHERE id = $1
                  AND challenge_id = $2
                  AND participation_id = $3"#,
        )
        .bind(spec.service_id)
        .bind(spec.challenge_id)
        .bind(spec.participation_id)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(game_id) = game_id else {
            transaction.rollback().await?;
            continue;
        };
        if !crate::services::participation_evidence::lock_audit_insert_scope_sqlx(
            &mut transaction,
            game_id,
            Some(spec.challenge_id),
            &[spec.participation_id],
        )
        .await?
        {
            transaction.rollback().await?;
            continue;
        }
        // The canonical scope check above owns every eligibility lock. Re-read
        // the narrower service identity afterward so teardown cannot remove the
        // observed owner between authorization and incident persistence.
        let service_still_exists = sqlx::query_scalar::<_, bool>(
            r#"SELECT EXISTS (
                 SELECT 1
                   FROM "AdTeamServices" service
                  WHERE service.id = $1
                    AND service.challenge_id = $2
                    AND service.participation_id = $3
               )"#,
        )
        .bind(spec.service_id)
        .bind(spec.challenge_id)
        .bind(spec.participation_id)
        .fetch_one(&mut *transaction)
        .await?;
        if !service_still_exists {
            transaction.rollback().await?;
            continue;
        }
        let endpoint_was_current = sqlx::query(
            r#"UPDATE "AdTeamServices"
                  SET host = '', port = 0, status = $5
                WHERE id = $1
                  AND container_id = $2
                  AND BTRIM(host) = $3
                  AND port = $4"#,
        )
        .bind(spec.service_id)
        .bind(&spec.container_id)
        .bind(&spec.host_text)
        .bind(i32::from(spec.port))
        .bind(AdCheckStatus::Offline as i16)
        .execute(&mut *transaction)
        .await?
        .rows_affected()
            == 1;

        sqlx::query(
            r#"INSERT INTO "TrafficCaptureFailures"
                   (service_id, container_id, host, port, challenge_id,
                    participation_id, detected_at, error,
                    endpoint_was_current, endpoint_deactivated_at,
                    network_revoked_at, last_reconcile_error)
               VALUES ($1, $2, $3, $4, $5, $6, clock_timestamp(), $7,
                       $8, clock_timestamp(), NULL, NULL)
               ON CONFLICT (service_id, container_id)
                   WHERE network_revoked_at IS NULL
               DO UPDATE SET
                   host = EXCLUDED.host,
                   port = EXCLUDED.port,
                   challenge_id = EXCLUDED.challenge_id,
                   participation_id = EXCLUDED.participation_id,
                   detected_at = EXCLUDED.detected_at,
                   error = EXCLUDED.error,
                   endpoint_was_current =
                       "TrafficCaptureFailures".endpoint_was_current
                       OR EXCLUDED.endpoint_was_current,
                   endpoint_deactivated_at = EXCLUDED.endpoint_deactivated_at,
                   last_reconcile_error = NULL"#,
        )
        .bind(spec.service_id)
        .bind(&spec.container_id)
        .bind(&spec.host_text)
        .bind(i32::from(spec.port))
        .bind(spec.challenge_id)
        .bind(spec.participation_id)
        .bind(bounded_error(&failure.error))
        .bind(endpoint_was_current)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;

        tracing::error!(
            service = spec.service_id,
            container = %spec.container_id,
            endpoint_was_current,
            error = %failure.error,
            "traffic capture failed; endpoint deactivated pending network-policy acknowledgement"
        );
    }
    Ok(())
}

async fn pending_failure_ids(connection: &mut PgConnection) -> Result<Vec<i64>, sqlx::Error> {
    sqlx::query_scalar(
        r#"SELECT id
             FROM "TrafficCaptureFailures"
            WHERE network_revoked_at IS NULL
            ORDER BY id"#,
    )
    .fetch_all(connection)
    .await
}

async fn record_reconcile_error(
    connection: &mut PgConnection,
    ids: &[i64],
    error: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"UPDATE "TrafficCaptureFailures"
              SET last_reconcile_error = $2
            WHERE id = ANY($1)
              AND network_revoked_at IS NULL"#,
    )
    .bind(ids)
    .bind(bounded_error(error))
    .execute(connection)
    .await?;
    Ok(())
}

async fn mark_network_revoked(
    connection: &mut PgConnection,
    ids: &[i64],
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"UPDATE "TrafficCaptureFailures"
              SET network_revoked_at = clock_timestamp(),
                  last_reconcile_error = NULL
            WHERE id = ANY($1)
              AND network_revoked_at IS NULL"#,
    )
    .bind(ids)
    .execute(connection)
    .await?;
    Ok(())
}

async fn prune_resolved(connection: &mut PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"DELETE FROM "TrafficCaptureFailures"
            WHERE network_revoked_at IS NOT NULL
              AND network_revoked_at
                    < clock_timestamp() - ($1 * interval '1 day')"#,
    )
    .bind(FAILURE_RETENTION_DAYS)
    .execute(connection)
    .await?;
    Ok(())
}

/// Retry the durable endpoint-to-kernel revocation without failing the owner
/// pass. The capture generation has already been acknowledged at this point,
/// so a firewall fault cannot strand an unrelated service's teardown request.
pub(super) async fn reconcile_pending(
    state: &SharedState,
    connection: &mut PgConnection,
    timeout: Duration,
) -> Result<bool, sqlx::Error> {
    let ids = pending_failure_ids(connection).await?;
    if ids.is_empty() {
        prune_resolved(connection).await?;
        return Ok(true);
    }

    let result = tokio::time::timeout(
        timeout,
        crate::services::ad_vpn::ensure_hub_and_sync(&state.db),
    )
    .await;
    match result {
        Ok(Ok(())) => {
            mark_network_revoked(connection, &ids).await?;
            tracing::info!(
                failures = ids.len(),
                "traffic capture failure endpoint revocation acknowledged"
            );
        }
        Ok(Err(error)) => {
            let error = error.to_string();
            record_reconcile_error(connection, &ids, &error).await?;
            tracing::warn!(
                failures = ids.len(),
                %error,
                "traffic capture failure endpoint revocation will retry"
            );
            return Ok(false);
        }
        Err(_) => {
            let error = format!(
                "network-policy acknowledgement timed out after {} seconds",
                timeout.as_secs()
            );
            record_reconcile_error(connection, &ids, &error).await?;
            tracing::warn!(
                failures = ids.len(),
                %error,
                "traffic capture failure endpoint revocation will retry"
            );
            return Ok(false);
        }
    }
    prune_resolved(connection).await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::bounded_error;

    #[test]
    fn persisted_errors_are_bounded_on_character_boundaries() {
        let input = "é".repeat(2_100);
        let bounded = bounded_error(&input);
        assert_eq!(bounded.chars().count(), 2_000);
        assert!(input.starts_with(&bounded));
    }
}
