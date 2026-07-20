use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ManagedContainerOwner {
    pub lock_key: String,
    pub shared_challenge_id: Option<i32>,
    pub test_challenge_id: Option<i32>,
    pub game_instance_id: Option<i32>,
    pub exercise_instance_id: Option<i32>,
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
                   SELECT 'shared-container:' || challenge.id::text AS lock_key,
                          challenge.id AS shared_challenge_id,
                          NULL::integer AS test_challenge_id,
                          NULL::integer AS game_instance_id,
                          NULL::integer AS exercise_instance_id,
                          0 AS priority
                     FROM "GameChallenges" challenge
                    WHERE challenge.shared_container_id = $1
                   UNION ALL
                   SELECT 'shared-container:' || target.challenge_id::text,
                          target.challenge_id, NULL::integer, NULL::integer,
                          NULL::integer, 1
                     FROM "KothTargets" target
                    WHERE target.container_id = $2
                   UNION ALL
                   SELECT 'test-containers-game:' || challenge.game_id::text,
                          NULL::integer, challenge.id, NULL::integer,
                          NULL::integer, 2
                     FROM "GameChallenges" challenge
                    WHERE challenge.test_container_id = $1
                   UNION ALL
                   SELECT 'game-container:' || instance.participation_id::text,
                          NULL::integer, NULL::integer, instance.id,
                          NULL::integer,
                          CASE WHEN instance.id = $3 THEN 3 ELSE 4 END
                     FROM "GameInstances" instance
                    WHERE instance.container_id = $1
                   UNION ALL
                   SELECT 'exercise-container:' || instance.user_id::text || ':' ||
                              instance.exercise_id::text,
                          NULL::integer, NULL::integer, NULL::integer,
                          instance.id,
                          CASE WHEN instance.id = $4 THEN 5 ELSE 6 END
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
pub(super) async fn clear_destroyed_managed_container(
    pool: &sqlx::PgPool,
    container_id: uuid::Uuid,
    backend_id: &str,
    game_instance_id: Option<i32>,
    exercise_instance_id: Option<i32>,
    shared_challenge_id: Option<i32>,
    test_challenge_id: Option<i32>,
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
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

/// Revoke and destroy one persisted container. The exact owner lock prevents a
/// replacement from publishing between network revocation and runtime destroy.
/// No owner identity is released until the backend confirms destruction.
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
    let distributed = if let Some(key) = flight_key {
        Some(crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(st.pg(), key).await?)
    } else {
        None
    };

    let result = async {
        let Some(current) = container::Entity::find_by_id(candidate.id)
            .one(&st.db)
            .await?
        else {
            return Ok(false);
        };
        if honor_refresh && current.expect_stop_at >= Utc::now() {
            return Ok(false);
        }

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
        )
        .await?;
        Ok(true)
    }
    .await;

    let released = if let Some(lock) = distributed {
        lock.release().await.map_err(AppError::from)
    } else {
        Ok(())
    };
    match (result, released) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(reaped), Ok(())) => Ok(reaped),
    }
}
