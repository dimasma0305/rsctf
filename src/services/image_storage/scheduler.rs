//! Durable cross-replica cadence for the independently supervised cleanup pass.

use super::*;

const CLEANUP_INTERVAL_SECONDS: i64 = 15 * 60;
const CLEANUP_LEASE_SECONDS: i64 = 5 * 60;
const CLEANUP_RETRY_SECONDS: i64 = 60;
const CLEANUP_PASS_BUDGET: Duration = Duration::from_secs(120);

struct ScheduledCleanupClaim {
    owner: uuid::Uuid,
    candidate_cursor: Option<String>,
}

async fn claim_scheduled_cleanup(
    st: &SharedState,
    scope: &str,
    owner: uuid::Uuid,
) -> AppResult<Option<ScheduledCleanupClaim>> {
    let inserted = sqlx::query(
        r#"INSERT INTO "ImageCleanupLeases" (
               installation_scope, next_run_at_utc, updated_at_utc
           ) VALUES (
               $1, clock_timestamp() + make_interval(secs => $2), clock_timestamp()
           ) ON CONFLICT (installation_scope) DO NOTHING"#,
    )
    .bind(scope)
    .bind(CLEANUP_INTERVAL_SECONDS as f64)
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    if inserted == 1 {
        return Ok(None);
    }
    let candidate_cursor = sqlx::query_scalar::<_, Option<String>>(
        r#"WITH claimed AS (
               UPDATE "ImageCleanupLeases"
                  SET lease_owner = $2,
                      lease_until_utc = clock_timestamp() + make_interval(secs => $3),
                      updated_at_utc = clock_timestamp()
                WHERE installation_scope = $1
                  AND next_run_at_utc <= clock_timestamp()
                  AND (lease_until_utc IS NULL OR lease_until_utc < clock_timestamp())
            RETURNING candidate_cursor_ref
           ) SELECT candidate_cursor_ref FROM claimed"#,
    )
    .bind(scope)
    .bind(owner)
    .bind(CLEANUP_LEASE_SECONDS as f64)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(
        candidate_cursor.map(|candidate_cursor| ScheduledCleanupClaim {
            owner,
            candidate_cursor,
        }),
    )
}

async fn finish_scheduled_cleanup(
    st: &SharedState,
    scope: &str,
    owner: uuid::Uuid,
    succeeded: bool,
    backlog: bool,
    candidate_cursor: Option<&str>,
) -> AppResult<()> {
    let delay = if succeeded && !backlog {
        CLEANUP_INTERVAL_SECONDS
    } else {
        CLEANUP_RETRY_SECONDS
    };
    sqlx::query(
        r#"UPDATE "ImageCleanupLeases"
              SET next_run_at_utc = clock_timestamp() + make_interval(secs => $3),
                  lease_owner = NULL,
                  lease_until_utc = NULL,
                  candidate_cursor_ref = CASE WHEN $4
                    THEN $5 ELSE candidate_cursor_ref END,
                  updated_at_utc = clock_timestamp()
            WHERE installation_scope = $1 AND lease_owner = $2"#,
    )
    .bind(scope)
    .bind(owner)
    .bind(delay as f64)
    .bind(succeeded)
    .bind(candidate_cursor)
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

pub async fn scheduled_cleanup(st: &SharedState) -> AppResult<Option<ImageCleanupReport>> {
    let policy = ContainerPolicy::load(st.pg()).await?;
    if !policy.image_cleanup_enabled
        || st.containers.backend_kind() != crate::services::container::ContainerBackendKind::Docker
    {
        return Ok(None);
    }
    let scope = crate::services::container::docker_installation_scope();
    let owner = uuid::Uuid::new_v4();
    let Some(claim) = claim_scheduled_cleanup(st, &scope, owner).await? else {
        return Ok(None);
    };
    let result = tokio::time::timeout(
        CLEANUP_PASS_BUDGET,
        cleanup_from_cursor(st, &policy, claim.candidate_cursor.as_deref()),
    )
    .await;
    let succeeded = matches!(result, Ok(Ok(_)));
    let next_cursor = result
        .as_ref()
        .ok()
        .and_then(|result| result.as_ref().ok())
        .and_then(|report| report.next_candidate_cursor.as_deref());
    let backlog = result
        .as_ref()
        .ok()
        .and_then(|result| result.as_ref().ok())
        .is_some_and(|report| report.candidate_backlog > 0);
    finish_scheduled_cleanup(st, &scope, claim.owner, succeeded, backlog, next_cursor).await?;
    match result {
        Ok(result) => result.map(Some),
        Err(_) => Err(AppError::unavailable("Docker image cleanup pass timed out")),
    }
}
