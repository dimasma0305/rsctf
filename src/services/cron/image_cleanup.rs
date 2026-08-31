//! Independent, deployment-wide Docker image-cleanup supervisor.

use std::time::Duration;

use uuid::Uuid;

use crate::app_state::SharedState;
use crate::services::container::ContainerBackendKind;
use crate::services::image_storage::ImageCleanupPass;

const POLL_INTERVAL: Duration = Duration::from_secs(30);
const RUN_INTERVAL_SECONDS: i32 = 15 * 60;
const PASS_BUDGET: Duration = Duration::from_secs(90);
const LEASE_SECONDS: i32 = 2 * 60;
const FINISH_TIMEOUT: Duration = Duration::from_secs(5);

const CLAIM_SCHEDULE_SQL: &str = r#"UPDATE "ImageCleanupSchedules"
   SET lease_token = $2,
       lease_until = clock_timestamp() + make_interval(secs => $3),
       next_run_at_utc = clock_timestamp() + make_interval(secs => $4),
       last_started_at_utc = clock_timestamp(),
       updated_at_utc = clock_timestamp()
 WHERE installation_scope = $1
   AND next_run_at_utc <= clock_timestamp()
   AND (lease_until IS NULL OR lease_until <= clock_timestamp())
RETURNING lease_token"#;

async fn try_claim_schedule(pool: &sqlx::PgPool, scope: &str) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO "ImageCleanupSchedules" (installation_scope)
           VALUES ($1) ON CONFLICT (installation_scope) DO NOTHING"#,
    )
    .bind(scope)
    .execute(pool)
    .await?;
    let token = Uuid::new_v4();
    sqlx::query_scalar::<_, Uuid>(CLAIM_SCHEDULE_SQL)
        .bind(scope)
        .bind(token)
        .bind(LEASE_SECONDS)
        .bind(RUN_INTERVAL_SECONDS)
        .fetch_optional(pool)
        .await
}

fn bounded_error(error: &str) -> String {
    error.chars().take(1024).collect()
}

async fn finish_schedule(
    pool: &sqlx::PgPool,
    scope: &str,
    token: Uuid,
    pass: Option<&ImageCleanupPass>,
    error: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let (scanned, claimed, removed, backlog, duration_millis) = pass
        .map(|pass| {
            (
                pass.scanned,
                pass.claimed,
                i64::from(pass.report.images_removed),
                pass.backlog,
                pass.duration_millis,
            )
        })
        .unwrap_or_default();
    let result = sqlx::query(
        r#"UPDATE "ImageCleanupSchedules"
              SET lease_token = NULL,
                  lease_until = NULL,
                  last_finished_at_utc = clock_timestamp(),
                  last_scanned = $3,
                  last_claimed = $4,
                  last_removed = $5,
                  last_backlog = $6,
                  last_duration_ms = $7,
                  last_error = $8,
                  updated_at_utc = clock_timestamp()
            WHERE installation_scope = $1 AND lease_token = $2"#,
    )
    .bind(scope)
    .bind(token)
    .bind(scanned.max(0))
    .bind(claimed.max(0))
    .bind(removed.max(0))
    .bind(backlog.max(0))
    .bind(duration_millis.max(0))
    .bind(error.map(bounded_error))
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

async fn release_after(
    state: &SharedState,
    scope: &str,
    token: Uuid,
    pass: Option<&ImageCleanupPass>,
    error: Option<&str>,
) {
    match tokio::time::timeout(
        FINISH_TIMEOUT,
        finish_schedule(state.pg(), scope, token, pass, error),
    )
    .await
    {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => tracing::warn!(
            %token,
            "cron: Docker cleanup lease changed before completion was recorded"
        ),
        Ok(Err(finish_error)) => tracing::warn!(
            %finish_error,
            "cron: Docker cleanup completion could not be recorded"
        ),
        Err(_) => tracing::warn!("cron: Docker cleanup completion recording timed out"),
    }
}

pub(super) async fn supervise(
    state: SharedState,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let mut ticker = tokio::time::interval(POLL_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    tracing::info!(
        poll_seconds = POLL_INTERVAL.as_secs(),
        budget_seconds = PASS_BUDGET.as_secs(),
        "cron: Docker image-cleanup supervisor started"
    );

    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
                continue;
            }
            _ = ticker.tick() => {}
        }
        if state.containers.backend_kind() != ContainerBackendKind::Docker {
            continue;
        }
        let policy =
            match crate::services::container_policy::ContainerPolicy::load(state.pg()).await {
                Ok(policy) if policy.image_cleanup_enabled => policy,
                Ok(_) => continue,
                Err(error) => {
                    tracing::warn!(%error, "cron: Docker cleanup policy read failed");
                    continue;
                }
            };
        let scope = crate::services::container::docker_installation_scope();
        let token = match try_claim_schedule(state.pg(), &scope).await {
            Ok(Some(token)) => token,
            Ok(None) => continue,
            Err(error) => {
                tracing::warn!(%error, "cron: Docker cleanup cadence claim failed");
                continue;
            }
        };
        let deadline = tokio::time::Instant::now() + PASS_BUDGET;
        let cleanup =
            crate::services::image_storage::cleanup_with_deadline(&state, &policy, deadline);
        tokio::pin!(cleanup);
        let result = tokio::select! {
            changed = shutdown.changed() => {
                let reason = if changed.is_err() || *shutdown.borrow() {
                    "cancelled by shutdown"
                } else {
                    "cancelled by supervisor signal"
                };
                release_after(&state, &scope, token, None, Some(reason)).await;
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
                continue;
            }
            result = tokio::time::timeout_at(deadline, &mut cleanup) => result,
        };
        match result {
            Ok(Ok(pass)) => {
                tracing::info!(
                    scanned = pass.scanned,
                    claimed = pass.claimed,
                    destroyed = pass.report.images_removed,
                    backlog = pass.backlog,
                    duration_ms = pass.duration_millis,
                    deadline_expired = pass.deadline_expired,
                    image_bytes = pass.report.image_bytes_evicted,
                    cache_bytes = pass.report.cache_bytes_reclaimed,
                    dangling_bytes = pass.report.dangling_bytes_reclaimed,
                    pressure = pass.report.pressure_mode,
                    "cron: completed bounded Docker storage cleanup"
                );
                for message in &pass.report.messages {
                    tracing::warn!(%message, "cron: Docker storage cleanup note");
                }
                release_after(&state, &scope, token, Some(&pass), None).await;
            }
            Ok(Err(error)) => {
                let message = error.to_string();
                tracing::warn!(%error, "cron: Docker storage cleanup failed");
                release_after(&state, &scope, token, None, Some(&message)).await;
            }
            Err(_) => {
                let message = format!(
                    "cleanup exceeded its absolute {}-second budget",
                    PASS_BUDGET.as_secs()
                );
                tracing::warn!(%message, "cron: Docker storage cleanup cancelled");
                release_after(&state, &scope, token, None, Some(&message)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durable_claim_advances_cadence_and_excludes_a_live_lease() {
        assert!(CLAIM_SCHEDULE_SQL.contains("next_run_at_utc <= clock_timestamp()"));
        assert!(CLAIM_SCHEDULE_SQL.contains("lease_until IS NULL OR lease_until <="));
        assert!(CLAIM_SCHEDULE_SQL.contains("next_run_at_utc = clock_timestamp()"));
        assert!(LEASE_SECONDS as u64 > PASS_BUDGET.as_secs());
    }

    #[test]
    fn stored_errors_are_utf8_safe_and_bounded() {
        let error = "é".repeat(2_000);
        let bounded = bounded_error(&error);
        assert_eq!(bounded.chars().count(), 1024);
        assert!(bounded.is_char_boundary(bounded.len()));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn restart_and_failover_share_one_durable_cadence() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_with(crate::migrations::test_pg_connect_options(&database_url))
            .await
            .unwrap();
        let schema = format!("cleanup_cadence_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(4)
            .connect_with(
                crate::migrations::test_pg_connect_options(&database_url)
                    .options([("search_path", schema.as_str())]),
            )
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"CREATE TABLE "ImageCleanupSchedules" (
                 installation_scope TEXT PRIMARY KEY,
                 next_run_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                 lease_token UUID, lease_until TIMESTAMPTZ,
                 last_started_at_utc TIMESTAMPTZ, last_finished_at_utc TIMESTAMPTZ,
                 last_scanned BIGINT NOT NULL DEFAULT 0,
                 last_claimed BIGINT NOT NULL DEFAULT 0,
                 last_removed BIGINT NOT NULL DEFAULT 0,
                 last_backlog BIGINT NOT NULL DEFAULT 0,
                 last_duration_ms BIGINT NOT NULL DEFAULT 0,
                 last_error VARCHAR(1024),
                 updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
               )"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let scope = "0123456789abcdef0123456789abcdef";
        let first = try_claim_schedule(&pool, scope).await.unwrap().unwrap();
        assert!(try_claim_schedule(&pool, scope).await.unwrap().is_none());
        assert!(finish_schedule(&pool, scope, first, None, None)
            .await
            .unwrap());
        assert!(try_claim_schedule(&pool, scope).await.unwrap().is_none());

        sqlx::query(
            r#"UPDATE "ImageCleanupSchedules"
                  SET next_run_at_utc = clock_timestamp() - interval '1 second'"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert!(try_claim_schedule(&pool, scope).await.unwrap().is_some());

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
