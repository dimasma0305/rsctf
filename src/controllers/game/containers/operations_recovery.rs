use super::*;

const RECOVERY_ATTEMPT_DEADLINE: Duration = Duration::from_secs(120);
const MAX_RECOVERIES_PER_PASS: usize = 4;

#[derive(sqlx::FromRow)]
struct ReconciliationClaim {
    operation_id: Uuid,
    publication_id: Uuid,
    game_id: i32,
    participation_id: Option<i32>,
    challenge_id: i32,
    definition_fence: Option<String>,
    backend_id: Option<String>,
}

#[derive(sqlx::FromRow)]
struct RecoveryContainer {
    backend_id: String,
    status: i16,
    started_at: DateTime<Utc>,
    expect_stop_at: DateTime<Utc>,
    is_proxy: bool,
    ip: String,
    port: i32,
    public_ip: Option<String>,
    public_port: Option<i32>,
    owned: bool,
    game_instance_id: Option<i32>,
}

impl RecoveryContainer {
    fn response(&self, publication_id: Uuid) -> AppResult<ContainerInfoModel> {
        let status = match self.status {
            value if value == ContainerStatus::Pending as i16 => ContainerStatus::Pending,
            value if value == ContainerStatus::Running as i16 => ContainerStatus::Running,
            value if value == ContainerStatus::Destroyed as i16 => ContainerStatus::Destroyed,
            value => {
                return Err(AppError::internal(format!(
                    "invalid container status {value}"
                )))
            }
        };
        let entry = if self.is_proxy {
            publication_id.to_string()
        } else {
            format!(
                "{}:{}",
                self.public_ip.as_deref().unwrap_or(&self.ip),
                self.public_port.unwrap_or(self.port)
            )
        };
        Ok(ContainerInfoModel {
            id: publication_id.to_string(),
            status,
            started_at: self.started_at,
            expect_stop_at: self.expect_stop_at,
            entry,
        })
    }
}

async fn claim_stale_reconciliation(pool: &sqlx::PgPool) -> AppResult<Option<ReconciliationClaim>> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    let admitted: bool =
        sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(CLAIM_LOCK)
            .fetch_one(&mut *transaction)
            .await
            .map_err(database_error)?;
    if !admitted {
        transaction.rollback().await.map_err(database_error)?;
        return Ok(None);
    }
    let claimed = sqlx::query_as::<_, ReconciliationClaim>(
        r#"WITH candidate AS (
               SELECT operation_id
                 FROM "PlayerContainerOperations"
                WHERE state = 'Running' AND runtime_started = TRUE
                  AND lease_expires_at_utc
                      < clock_timestamp() - interval '30 seconds'
                ORDER BY lease_expires_at_utc, operation_id
                LIMIT 1 FOR UPDATE SKIP LOCKED
           )
           UPDATE "PlayerContainerOperations" operation
              SET lease_expires_at_utc = clock_timestamp() + interval '3 minutes',
                  updated_at_utc = clock_timestamp()
             FROM candidate
            WHERE operation.operation_id = candidate.operation_id
        RETURNING operation.operation_id, operation.publication_id,
                  operation.game_id, operation.participation_id,
                  operation.challenge_id, operation.definition_fence,
                  operation.backend_id"#,
    )
    .fetch_optional(&mut *transaction)
    .await
    .map_err(database_error)?;
    let Some(claimed) = claimed else {
        transaction.commit().await.map_err(database_error)?;
        return Ok(None);
    };
    let runtime_lock_key = match claimed.participation_id {
        Some(participation_id) => format!("game-container:{participation_id}"),
        None => format!("shared-container:{}", claimed.challenge_id),
    };
    let runtime_available = crate::utils::single_flight::try_acquire_transaction_advisory_lock(
        &mut transaction,
        &runtime_lock_key,
    )
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !runtime_available {
        transaction.rollback().await.map_err(database_error)?;
        return Ok(None);
    }
    let teardown_pending: bool = sqlx::query_scalar(MANAGED_REAP_PENDING_SQL)
        .bind(&runtime_lock_key)
        .bind(claimed.publication_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
    if teardown_pending {
        transaction.rollback().await.map_err(database_error)?;
        return Ok(None);
    }
    transaction.commit().await.map_err(database_error)?;
    Ok(Some(claimed))
}

async fn force_reconciliation_failed(
    pool: &sqlx::PgPool,
    operation: &ClaimedOperation,
) -> AppResult<()> {
    sqlx::query(
        r#"UPDATE "PlayerContainerOperations"
              SET state = 'Failed', result = NULL,
                  lease_expires_at_utc = clock_timestamp(),
                  updated_at_utc = clock_timestamp()
            WHERE operation_id = $1 AND publication_id = $2 AND state = 'Running'"#,
    )
    .bind(operation.operation_id)
    .bind(operation.publication_id)
    .execute(pool)
    .await
    .map(|_| ())
    .map_err(database_error)
}

async fn recover_one_stale_launch(st: &SharedState) -> AppResult<bool> {
    let Some(claim) = claim_stale_reconciliation(st.pg()).await? else {
        return Ok(false);
    };
    let operation = ClaimedOperation {
        operation_id: claim.operation_id,
        publication_id: claim.publication_id,
    };
    let published = sqlx::query_as::<_, RecoveryContainer>(
        r#"SELECT container.container_id AS backend_id,
                  container.status, container.started_at, container.expect_stop_at,
                  container.is_proxy, container.ip, container.port,
                  container.public_ip, container.public_port,
                  CASE WHEN $2::integer IS NULL THEN EXISTS(
                           SELECT 1 FROM "GameChallenges" challenge
                            WHERE challenge.id = $3
                              AND challenge.shared_container_id = container.id
                       ) ELSE EXISTS(
                           SELECT 1 FROM "GameInstances" instance
                            WHERE instance.participation_id = $2
                              AND instance.challenge_id = $3
                              AND instance.container_id = container.id
                       ) END AS owned,
                  (SELECT instance.id FROM "GameInstances" instance
                    WHERE instance.participation_id = $2
                      AND instance.challenge_id = $3
                      AND instance.container_id = container.id
                    LIMIT 1) AS game_instance_id
             FROM "Containers" container WHERE container.id = $1"#,
    )
    .bind(claim.publication_id)
    .bind(claim.participation_id)
    .bind(claim.challenge_id)
    .fetch_optional(st.pg())
    .await
    .map_err(database_error)?;

    if let Some(container) = published {
        let definition_current = if container.owned {
            match game_challenge::Entity::find_by_id(claim.challenge_id)
                .one(&st.db)
                .await?
            {
                Some(challenge) if challenge.game_id == claim.game_id => {
                    match crate::services::challenge_workloads::resolve_runtime(st, &challenge) {
                        Ok(runtime) => {
                            claim.definition_fence.as_deref()
                                == Some(runtime.publication_fence.as_str())
                        }
                        Err(error) => {
                            settle_failed_work(st.pg(), &operation).await;
                            return Err(error);
                        }
                    }
                }
                _ => false,
            }
        } else {
            false
        };
        if container.owned
            && definition_current
            && container.status == ContainerStatus::Running as i16
        {
            match st.containers.inspect_liveness(&container.backend_id).await {
                Ok(crate::services::container::ContainerLiveness::Running) => {
                    return complete(
                        st.pg(),
                        &operation,
                        &container.response(claim.publication_id)?,
                    )
                    .await
                    .map(|_| true);
                }
                Ok(crate::services::container::ContainerLiveness::Stopped) => {}
                Ok(crate::services::container::ContainerLiveness::Unknown) => {
                    settle_failed_work(st.pg(), &operation).await;
                    return Ok(true);
                }
                Err(error) => {
                    settle_failed_work(st.pg(), &operation).await;
                    return Err(error);
                }
            }
        }
        if container.owned {
            match (claim.participation_id, container.game_instance_id) {
                (Some(_), Some(instance_id)) => {
                    revoke_published_team_container(
                        st,
                        &container.backend_id,
                        claim.publication_id,
                        instance_id,
                        None,
                        None,
                    )
                    .await?;
                }
                (None, _) => {
                    revoke_published_shared_container(
                        st,
                        claim.challenge_id,
                        claim.publication_id,
                        &container.backend_id,
                    )
                    .await?;
                }
                _ => {
                    settle_failed_work(st.pg(), &operation).await;
                    return Ok(true);
                }
            }
        } else {
            crate::services::traffic::destroy_container_after_capture_fence(
                st,
                &container.backend_id,
            )
            .await?;
            sqlx::query(r#"DELETE FROM "Containers" WHERE id = $1 AND container_id = $2"#)
                .bind(claim.publication_id)
                .bind(&container.backend_id)
                .execute(st.pg())
                .await
                .map_err(database_error)?;
        }
        force_reconciliation_failed(st.pg(), &operation).await?;
        return Ok(true);
    }

    let backend_id = match claim.backend_id {
        Some(backend_id) => Some(backend_id),
        None => {
            st.containers
                .find_operation_runtime(&format!("player-container:{}", claim.operation_id))
                .await?
        }
    };
    if let Some(backend_id) = backend_id {
        crate::services::traffic::destroy_container_after_capture_fence(st, &backend_id).await?;
    }
    force_reconciliation_failed(st.pg(), &operation).await?;
    Ok(true)
}

pub(crate) async fn sweep(st: &SharedState, limit: i64) -> AppResult<u64> {
    for _ in 0..MAX_RECOVERIES_PER_PASS {
        match tokio::time::timeout(RECOVERY_ATTEMPT_DEADLINE, recover_one_stale_launch(st)).await {
            Ok(Ok(true)) => {}
            Ok(Ok(false)) => break,
            Ok(Err(error)) => return Err(error),
            Err(_) => {
                tracing::warn!("player container operation recovery exceeded its deadline");
                break;
            }
        }
    }
    purge_terminal(st.pg(), limit).await
}

async fn purge_terminal(pool: &sqlx::PgPool, limit: i64) -> AppResult<u64> {
    sqlx::query(
        r#"WITH victims AS (
               SELECT operation_id FROM "PlayerContainerOperations"
                WHERE state <> 'Running'
                  AND updated_at_utc < clock_timestamp() - interval '24 hours'
                ORDER BY updated_at_utc, operation_id
                LIMIT $1
                FOR UPDATE SKIP LOCKED
           )
           DELETE FROM "PlayerContainerOperations" operation
            USING victims WHERE operation.operation_id = victims.operation_id"#,
    )
    .bind(limit.clamp(1, 256))
    .execute(pool)
    .await
    .map(|result| result.rows_affected())
    .map_err(database_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_work_is_bounded_and_uses_the_shared_reaper_fence() {
        assert!(MAX_RECOVERIES_PER_PASS <= 4);
        assert!(RECOVERY_ATTEMPT_DEADLINE <= Duration::from_secs(2 * 60));
        assert!(MANAGED_REAP_PENDING_SQL.contains("reap.scope_key = $1"));
    }
}
