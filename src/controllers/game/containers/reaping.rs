use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ManagedContainerOwner {
    pub lock_key: String,
    pub shared_challenge_id: Option<i32>,
    pub test_challenge_id: Option<i32>,
    pub game_instance_id: Option<i32>,
    pub exercise_instance_id: Option<i32>,
}

const REAP_LEASE: std::time::Duration = std::time::Duration::from_secs(300);
const REAP_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(10);
const REAP_EXTERNAL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);

#[derive(Clone, Copy)]
pub(super) struct ReapClaim {
    pub(super) lease_owner: uuid::Uuid,
}

pub(super) async fn claim_reap_on(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    candidate: &container::Model,
    scope_key: &str,
) -> AppResult<Option<ReapClaim>> {
    let mut existing = sqlx::query_as::<_, (String, uuid::Uuid, uuid::Uuid, bool)>(
        r#"SELECT backend_id, container_id, lease_owner,
                  lease_expires_at_utc > clock_timestamp() AS lease_active
             FROM "ManagedContainerReapOperations"
            WHERE backend_id = $1 OR container_id = $2
            ORDER BY backend_id
            FOR UPDATE"#,
    )
    .bind(&candidate.container_id)
    .bind(candidate.id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if existing.len() > 1 {
        return Err(AppError::conflict(
            "Container reaper identity conflicts with multiple runtimes",
        ));
    }
    let lease_owner = uuid::Uuid::new_v4();
    if let Some((backend_id, container_id, _prior_owner, lease_active)) = existing.pop() {
        if backend_id != candidate.container_id || container_id != candidate.id {
            return Err(AppError::conflict(
                "Container reaper identity conflicts with another runtime",
            ));
        }
        if lease_active {
            return Ok(None);
        }
        sqlx::query(
            r#"UPDATE "ManagedContainerReapOperations"
                  SET lease_owner = $3, scope_key = $5,
                      lease_expires_at_utc = clock_timestamp() + make_interval(secs => $4),
                      updated_at_utc = clock_timestamp(), last_error = NULL
                WHERE backend_id = $1 AND container_id = $2"#,
        )
        .bind(&candidate.container_id)
        .bind(candidate.id)
        .bind(lease_owner)
        .bind(REAP_LEASE.as_secs() as i32)
        .bind(scope_key)
        .execute(&mut **transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    } else {
        sqlx::query(
            r#"INSERT INTO "ManagedContainerReapOperations"
                   (backend_id, container_id, scope_key, lease_owner, lease_expires_at_utc)
               VALUES ($1, $2, $3, $4,
                       clock_timestamp() + make_interval(secs => $5))"#,
        )
        .bind(&candidate.container_id)
        .bind(candidate.id)
        .bind(scope_key)
        .bind(lease_owner)
        .bind(REAP_LEASE.as_secs() as i32)
        .execute(&mut **transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    Ok(Some(ReapClaim { lease_owner }))
}

async fn abandon_reap_claim(
    pool: &sqlx::PgPool,
    candidate: &container::Model,
    claim: ReapClaim,
) -> AppResult<()> {
    sqlx::query(
        r#"DELETE FROM "ManagedContainerReapOperations"
            WHERE backend_id = $1 AND container_id = $2 AND lease_owner = $3"#,
    )
    .bind(&candidate.container_id)
    .bind(candidate.id)
    .bind(claim.lease_owner)
    .execute(pool)
    .await
    .map(|_| ())
    .map_err(|error| AppError::internal(error.to_string()))
}

async fn retain_failed_reap_claim(
    pool: &sqlx::PgPool,
    candidate: &container::Model,
    claim: ReapClaim,
    error: &AppError,
) {
    let message = error.to_string().chars().take(512).collect::<String>();
    if let Err(update_error) = sqlx::query(
        r#"UPDATE "ManagedContainerReapOperations"
              SET lease_expires_at_utc = clock_timestamp() + make_interval(secs => $4),
                  updated_at_utc = clock_timestamp(), last_error = $5
            WHERE backend_id = $1 AND container_id = $2 AND lease_owner = $3"#,
    )
    .bind(&candidate.container_id)
    .bind(candidate.id)
    .bind(claim.lease_owner)
    .bind(REAP_RETRY_DELAY.as_secs() as i32)
    .bind(message)
    .execute(pool)
    .await
    {
        tracing::warn!(%update_error, backend_id = %candidate.container_id, "failed to retain durable reaper retry claim");
    }
}

pub(super) async fn managed_reap_active(
    pool: &sqlx::PgPool,
    container_id: uuid::Uuid,
    backend_id: &str,
) -> AppResult<bool> {
    sqlx::query_scalar(
        r#"SELECT EXISTS(
               SELECT 1 FROM "ManagedContainerReapOperations"
                WHERE container_id = $1 AND backend_id = $2
           )"#,
    )
    .bind(container_id)
    .bind(backend_id)
    .fetch_one(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

/// Resolve only an owner whose reverse pointer still names this exact container.
/// The forward ids on a stale Containers row are hints used to prioritize a
/// match; they never authorize detaching an instance that already points at a
/// replacement.
pub(super) async fn resolve_managed_container_owner(
    pool: &sqlx::PgPool,
    container_id: uuid::Uuid,
    backend_id: &str,
    game_instance_id: Option<i32>,
    exercise_instance_id: Option<i32>,
) -> AppResult<Option<ManagedContainerOwner>> {
    let row = sqlx::query_as::<_, (String, Option<i32>, Option<i32>, Option<i32>, Option<i32>)>(
        r#"SELECT lock_key, shared_challenge_id, test_challenge_id,
                  game_instance_id, exercise_instance_id
             FROM (
                   SELECT 'ad-inspector:' || owned.ad_team_service_id::text AS lock_key,
                          NULL::integer AS shared_challenge_id,
                          NULL::integer AS test_challenge_id,
                          NULL::integer AS game_instance_id,
                          NULL::integer AS exercise_instance_id,
                          0 AS priority
                     FROM "Containers" owned
                    WHERE owned.id = $1
                      AND owned.ad_team_service_id IS NOT NULL
                   UNION ALL
                   SELECT 'shared-container:' || challenge.id::text AS lock_key,
                          challenge.id AS shared_challenge_id,
                          NULL::integer AS test_challenge_id,
                          NULL::integer AS game_instance_id,
                          NULL::integer AS exercise_instance_id,
                          1 AS priority
                     FROM "GameChallenges" challenge
                    WHERE challenge.shared_container_id = $1
                   UNION ALL
                   SELECT 'shared-container:' || target.challenge_id::text,
                          target.challenge_id, NULL::integer, NULL::integer,
                          NULL::integer, 2
                     FROM "KothTargets" target
                    WHERE target.container_id = $2
                   UNION ALL
                   SELECT 'test-containers-game:' || challenge.game_id::text,
                          NULL::integer, challenge.id, NULL::integer,
                          NULL::integer, 3
                     FROM "GameChallenges" challenge
                    WHERE challenge.test_container_id = $1
                   UNION ALL
                   SELECT 'game-container:' || instance.participation_id::text,
                          NULL::integer, NULL::integer, instance.id,
                          NULL::integer,
                          CASE WHEN instance.id = $3 THEN 4 ELSE 5 END
                     FROM "GameInstances" instance
                    WHERE instance.container_id = $1
                   UNION ALL
                   SELECT 'exercise-container:' || instance.user_id::text || ':' ||
                              instance.exercise_id::text,
                          NULL::integer, NULL::integer, NULL::integer,
                          instance.id,
                          CASE WHEN instance.id = $4 THEN 6 ELSE 7 END
                     FROM "ExerciseInstances" instance
                    WHERE instance.container_id = $1
             ) owner
            ORDER BY priority
            LIMIT 1"#,
    )
    .bind(container_id)
    .bind(backend_id)
    .bind(game_instance_id)
    .bind(exercise_instance_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    Ok(row.map(
        |(
            lock_key,
            shared_challenge_id,
            test_challenge_id,
            game_instance_id,
            exercise_instance_id,
        )| ManagedContainerOwner {
            lock_key,
            shared_challenge_id,
            test_challenge_id,
            game_instance_id,
            exercise_instance_id,
        },
    ))
}

/// Clear every exact reverse owner and the exact Containers identity in one
/// transaction. Zero-row CAS updates are valid: they mean a replacement won and
/// must remain attached.
#[allow(clippy::too_many_arguments)]
pub(super) async fn clear_destroyed_managed_container(
    pool: &sqlx::PgPool,
    container_id: uuid::Uuid,
    backend_id: &str,
    game_instance_id: Option<i32>,
    exercise_instance_id: Option<i32>,
    shared_challenge_id: Option<i32>,
    test_challenge_id: Option<i32>,
    reap_claim: Option<ReapClaim>,
) -> AppResult<()> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if let Some(instance_id) = game_instance_id {
        sqlx::query(
            r#"UPDATE "GameInstances"
                  SET container_id = NULL, is_loaded = FALSE,
                      last_container_operation = CURRENT_TIMESTAMP
                WHERE id = $1 AND container_id = $2"#,
        )
        .bind(instance_id)
        .bind(container_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    if let Some(instance_id) = exercise_instance_id {
        sqlx::query(
            r#"UPDATE "ExerciseInstances"
                  SET container_id = NULL, is_loaded = FALSE,
                      last_container_operation = CURRENT_TIMESTAMP
                WHERE id = $1 AND container_id = $2"#,
        )
        .bind(instance_id)
        .bind(container_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    if let Some(challenge_id) = shared_challenge_id {
        sqlx::query(
            r#"UPDATE "GameChallenges" SET shared_container_id = NULL
                WHERE id = $1 AND shared_container_id = $2"#,
        )
        .bind(challenge_id)
        .bind(container_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    if let Some(challenge_id) = test_challenge_id {
        sqlx::query(
            r#"UPDATE "GameChallenges" SET test_container_id = NULL
                WHERE id = $1 AND test_container_id = $2"#,
        )
        .bind(challenge_id)
        .bind(container_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    sqlx::query(
        r#"UPDATE "KothTargets" SET container_id = NULL
            WHERE container_id = $1
              AND NULLIF(BTRIM(host), '') IS NULL AND port = 0"#,
    )
    .bind(backend_id)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(r#"DELETE FROM "Containers" WHERE id = $1 AND container_id = $2"#)
        .bind(container_id)
        .bind(backend_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if let Some(claim) = reap_claim {
        sqlx::query(
            r#"DELETE FROM "ManagedContainerReapOperations"
                WHERE backend_id = $1 AND container_id = $2 AND lease_owner = $3"#,
        )
        .bind(backend_id)
        .bind(container_id)
        .bind(claim.lease_owner)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

/// Revoke and destroy one persisted container. The exact owner lock publishes
/// a durable teardown claim, then releases its pooled connection before any
/// network/capture/blob work. Player lifecycle claims observe that marker and
/// fail fast until teardown finishes or maintenance reconciles it.
pub(crate) async fn destroy_managed_container_row(
    st: &SharedState,
    candidate: &container::Model,
    honor_refresh: bool,
) -> AppResult<bool> {
    let owner = resolve_managed_container_owner(
        st.pg(),
        candidate.id,
        &candidate.container_id,
        candidate.game_instance_id,
        candidate.exercise_instance_id,
    )
    .await?;
    let flight_key = owner.as_ref().map(|owner| owner.lock_key.as_str());
    let _flight = if let Some(key) = flight_key {
        Some(crate::utils::single_flight::coalesce(key).await)
    } else {
        None
    };
    let mut distributed = if let Some(key) = flight_key {
        Some(crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(st.pg(), key).await?)
    } else {
        None
    };

    let reap_claim = if let (Some(owner), Some(lock)) = (owner.as_ref(), distributed.as_mut()) {
        let player_operation_active = if let Some(instance_id) = owner.game_instance_id {
            sqlx::query_scalar::<_, bool>(
                r#"SELECT EXISTS(
                       SELECT 1 FROM "PlayerContainerOperations" operation
                       JOIN "GameInstances" instance
                         ON instance.participation_id = operation.participation_id
                      WHERE instance.id = $1 AND operation.state = 'Running'
                        AND operation.lease_expires_at_utc > clock_timestamp()
                   )"#,
            )
            .bind(instance_id)
            .fetch_one(&mut **lock.transaction_mut())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?
        } else if let Some(challenge_id) = owner.shared_challenge_id {
            sqlx::query_scalar::<_, bool>(
                r#"SELECT EXISTS(
                       SELECT 1 FROM "PlayerContainerOperations"
                        WHERE scope_key = $1 AND state = 'Running'
                          AND lease_expires_at_utc > clock_timestamp()
                   )"#,
            )
            .bind(format!("shared-challenge:{challenge_id}"))
            .fetch_one(&mut **lock.transaction_mut())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?
        } else {
            false
        };
        let exercise_operation_active = if let Some(instance_id) = owner.exercise_instance_id {
            sqlx::query_scalar::<_, bool>(
                r#"SELECT EXISTS(
                       SELECT 1 FROM "ExerciseContainerOperations" operation
                       JOIN "ExerciseInstances" instance
                         ON instance.user_id = operation.user_id
                        AND instance.exercise_id = operation.exercise_id
                      WHERE instance.id = $1 AND operation.state = 'Running'
                        AND operation.lease_expires_at_utc > clock_timestamp()
                   )"#,
            )
            .bind(instance_id)
            .fetch_one(&mut **lock.transaction_mut())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?
        } else {
            false
        };
        if player_operation_active || exercise_operation_active {
            let lock = distributed.take().expect("checked lifecycle lock exists");
            lock.release().await.map_err(AppError::from)?;
            return Ok(false);
        }
        let claim = claim_reap_on(lock.transaction_mut(), candidate, &owner.lock_key).await?;
        let lock = distributed.take().expect("checked lifecycle lock exists");
        lock.release().await.map_err(AppError::from)?;
        let Some(claim) = claim else {
            return Ok(false);
        };
        Some(claim)
    } else {
        None
    };

    let current = container::Entity::find_by_id(candidate.id)
        .one(&st.db)
        .await?
        .filter(|current| current.container_id == candidate.container_id);
    let Some(current) = current else {
        if let Some(claim) = reap_claim {
            abandon_reap_claim(st.pg(), candidate, claim).await?;
        }
        return Ok(false);
    };
    if honor_refresh && current.expect_stop_at >= Utc::now() {
        if let Some(claim) = reap_claim {
            abandon_reap_claim(st.pg(), candidate, claim).await?;
        }
        return Ok(false);
    }

    let result = tokio::time::timeout(REAP_EXTERNAL_DEADLINE, async {
        // Stage the restrictive endpoint while retaining Koth/A&D identities.
        // Cache eviction precedes the kernel fence; destroy failure leaves the
        // Containers row and inactive endpoint available for an exact retry.
        let game_ids =
            crate::services::ad_vpn::stage_backend_endpoint_deactivation_retaining_identity(
                &st.db,
                &current.container_id,
            )
            .await?;
        for game_id in game_ids {
            crate::controllers::game::ad::invalidate_live_hill_snapshot(st, game_id).await;
        }
        crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await?;
        crate::services::traffic::destroy_container_after_capture_fence(st, &current.container_id)
            .await?;

        let game_instance_id = owner
            .as_ref()
            .and_then(|owner| owner.game_instance_id)
            .or(current.game_instance_id);
        let exercise_instance_id = owner
            .as_ref()
            .and_then(|owner| owner.exercise_instance_id)
            .or(current.exercise_instance_id);
        clear_destroyed_managed_container(
            st.pg(),
            current.id,
            &current.container_id,
            game_instance_id,
            exercise_instance_id,
            owner.as_ref().and_then(|owner| owner.shared_challenge_id),
            owner.as_ref().and_then(|owner| owner.test_challenge_id),
            reap_claim,
        )
        .await?;
        Ok(true)
    })
    .await
    .unwrap_or_else(|_| Err(AppError::overloaded("Container teardown timed out", 10)));
    if let Err(error) = &result {
        if let Some(claim) = reap_claim {
            retain_failed_reap_claim(st.pg(), candidate, claim, error).await;
        }
    }
    result
}
