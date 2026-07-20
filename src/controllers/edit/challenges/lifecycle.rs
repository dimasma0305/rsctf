use super::*;

#[derive(Debug, PartialEq)]
pub(super) struct RuntimeDefinitionSnapshot {
    row: serde_json::Value,
    static_flags: Vec<String>,
}

/// Capture every persisted field that can affect launch, grading, review, or
/// build readiness while deliberately excluding presentation-only metadata,
/// counters, and the shared pointer that teardown itself clears. The JSONB row
/// projection automatically includes newly-added safety fields, so an older
/// topology transition fails closed when a concurrent writer publishes a
/// definition shape it does not understand.
pub(super) async fn runtime_definition_snapshot(
    pool: &sqlx::PgPool,
    challenge_id: i32,
    challenge_type: ChallengeType,
) -> AppResult<RuntimeDefinitionSnapshot> {
    let row = sqlx::query_scalar::<_, serde_json::Value>(
        r#"SELECT to_jsonb(challenge) - ARRAY[
                    'id', 'game_id', 'title', 'content', 'category', 'hints',
                    'is_enabled', 'deadline_utc', 'accepted_count',
                    'submission_count', 'file_name', 'source_yaml_path',
                    'attachment_id', 'shared_container_id'
                  ]::TEXT[]
             FROM "GameChallenges" challenge
            WHERE challenge.id = $1"#,
    )
    .bind(challenge_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Challenge not found"))?;
    let static_flags = if challenge_type == ChallengeType::StaticContainer {
        sqlx::query_scalar::<_, String>(
            r#"SELECT flag
                 FROM "FlagContexts"
                WHERE challenge_id = $1
                ORDER BY flag"#,
        )
        .bind(challenge_id)
        .fetch_all(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
    } else {
        Vec::new()
    };
    Ok(RuntimeDefinitionSnapshot { row, static_flags })
}

async fn teardown_allowed(pool: &sqlx::PgPool, challenge_id: i32, require_inactive: bool) -> bool {
    if !require_inactive {
        return true;
    }
    match sqlx::query_scalar::<_, bool>(
        r#"SELECT NOT is_enabled OR review_status <> $2
             FROM "GameChallenges" WHERE id = $1"#,
    )
    .bind(challenge_id)
    .bind(ChallengeReviewStatus::Active as i16)
    .fetch_optional(pool)
    .await
    {
        Ok(Some(allowed)) => allowed,
        Ok(None) => false,
        Err(error) => {
            tracing::warn!(challenge = challenge_id, %error, "challenge teardown revalidation failed");
            false
        }
    }
}

/// Release only the exact inactive pointer whose backend was successfully
/// destroyed. A replacement endpoint can never be detached by a stale cleanup.
async fn clear_destroyed_koth_target(
    pool: &sqlx::PgPool,
    target_id: i32,
    backend_id: &str,
) -> AppResult<bool> {
    let changed = sqlx::query(
        r#"UPDATE "KothTargets"
              SET container_id = NULL
            WHERE id = $1 AND container_id = $2
              AND NULLIF(BTRIM(host), '') IS NULL AND port = 0"#,
    )
    .bind(target_id)
    .bind(backend_id)
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    Ok(changed == 1)
}

fn handle_teardown_result(
    result: AppResult<()>,
    strict: bool,
    challenge_id: i32,
    stage: &'static str,
) -> AppResult<()> {
    match result {
        Ok(()) => Ok(()),
        Err(error) if strict => Err(error),
        Err(error) => {
            tracing::warn!(challenge = challenge_id, %error, %stage,
                "challenge teardown failed; retaining durable owner for retry");
            Ok(())
        }
    }
}

async fn koth_target_still_owns_backend(pool: &sqlx::PgPool, target_id: i32) -> AppResult<bool> {
    sqlx::query_scalar(
        r#"SELECT EXISTS (
             SELECT 1 FROM "KothTargets"
              WHERE id = $1 AND container_id IS NOT NULL
           )"#,
    )
    .bind(target_id)
    .fetch_one(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

/// Teardown every runtime owned by one challenge. When
/// `require_inactive` is set, every provisioning section rechecks the durable
/// enabled + review marker after taking its runtime lock. This prevents stale
/// disable/reject cleanup from destroying a runtime after a concurrent
/// re-enable/approval has made the challenge playable again. `strict` is used
/// by physical deletion: any failure aborts before retained identities cascade.
pub(crate) async fn destroy_challenge_containers(
    st: &SharedState,
    challenge: &game_challenge::Model,
    require_inactive: bool,
    strict: bool,
) -> AppResult<()> {
    if !teardown_allowed(st.pg(), challenge.id, true).await {
        if strict {
            return Err(AppError::conflict(
                "Challenge runtime teardown requires a durable inactive marker",
            ));
        }
        tracing::warn!(
            challenge = challenge.id,
            "challenge teardown skipped because the challenge is still eligible"
        );
        return Ok(());
    }
    // Every caller has committed an inactive marker first (live topology flips
    // use the controller's durable two-phase disable -> teardown -> restore
    // sequence). Briefly take the exclusive challenge parent to drain creators
    // with no AdTeamServices row yet, then release it before taking per-pair
    // leaves. Later creators observe the inactive marker and cannot publish.
    let publications_drained =
        match crate::services::ad::service_lifecycle::acquire_challenge_publication_fence(
            st.pg(),
            challenge.id,
        )
        .await
        {
            Ok(fence) => match fence.release().await {
                Ok(()) => true,
                Err(error) if strict => return Err(AppError::internal(error.to_string())),
                Err(error) => {
                    tracing::warn!(challenge = challenge.id, %error,
                    "challenge teardown: publication fence release failed");
                    false
                }
            },
            Err(error) if strict => return Err(error),
            Err(error) => {
                tracing::warn!(challenge = challenge.id, %error,
                "challenge teardown: publication fence failed");
                false
            }
        };
    let services = if publications_drained {
        match ad_team_service::Entity::find()
            .filter(ad_team_service::Column::ChallengeId.eq(challenge.id))
            .all(&st.db)
            .await
        {
            Ok(services) => services,
            Err(error) if strict => return Err(error.into()),
            Err(error) => {
                tracing::warn!(challenge = challenge.id, %error,
                    "challenge teardown: service listing failed");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    for service in services {
        let lock_key = crate::services::ad::service_lifecycle::service_lock_key(
            service.participation_id,
            service.challenge_id,
        );
        let _local = crate::utils::single_flight::coalesce(&lock_key).await;
        let distributed = match crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(
            st.pg(),
            &lock_key,
        )
        .await
        {
            Ok(lock) => lock,
            Err(error) => {
                tracing::warn!(challenge = challenge.id, service = service.id, %error,
                            "challenge teardown: service lock failed");
                if strict {
                    return Err(AppError::internal(error.to_string()));
                }
                continue;
            }
        };
        if !teardown_allowed(st.pg(), challenge.id, require_inactive).await {
            let _ = distributed.release().await;
            return Ok(());
        }
        let teardown: AppResult<()> = async {
            crate::services::ad::service_lifecycle::destroy_persisted_service(st, service.id).await
        }
        .await;
        let released = distributed
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()));
        handle_teardown_result(teardown, strict, challenge.id, "A&D service")?;
        handle_teardown_result(released, strict, challenge.id, "A&D service unlock")?;
    }
    let participation_ids = match sqlx::query_scalar::<_, i32>(
        r#"SELECT id
             FROM "Participations"
            WHERE game_id = $1
            UNION
           SELECT participation_id
             FROM "GameInstances"
            WHERE challenge_id = $2
            ORDER BY 1"#,
    )
    .bind(challenge.game_id)
    .bind(challenge.id)
    .fetch_all(st.pg())
    .await
    {
        Ok(ids) => ids,
        Err(error) if strict => return Err(AppError::internal(error.to_string())),
        Err(error) => {
            tracing::warn!(challenge = challenge.id, %error,
                "challenge teardown: listing participation owners failed");
            Vec::new()
        }
    };
    for participation_id in participation_ids {
        let key = format!("game-container:{participation_id}");
        let _local = crate::utils::single_flight::coalesce(&key).await;
        let distributed = match crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(
            st.pg(),
            &key,
        )
        .await
        {
            Ok(lock) => lock,
            Err(error) => {
                tracing::warn!(challenge = challenge.id, participation = participation_id, %error,
                    "challenge teardown: instance lock failed");
                if strict {
                    return Err(AppError::internal(error.to_string()));
                }
                continue;
            }
        };
        if !teardown_allowed(st.pg(), challenge.id, require_inactive).await {
            let _ = distributed.release().await;
            return Ok(());
        }
        let teardown: AppResult<()> = async {
            let instances = game_instance::Entity::find()
                .filter(game_instance::Column::ParticipationId.eq(participation_id))
                .filter(game_instance::Column::ChallengeId.eq(challenge.id))
                .all(&st.db)
                .await?;
            for inst in instances {
                let Some(cuuid) = inst.container_id else {
                    continue;
                };
                destroy_container_row_after_capture_fence(st, cuuid).await?;
                let mut active: game_instance::ActiveModel = inst.into();
                active.container_id = Set(None);
                active.is_loaded = Set(false);
                active.last_container_operation = Set(Utc::now());
                active.update(&st.db).await?;
            }
            Ok(())
        }
        .await;
        let released = distributed
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()));
        handle_teardown_result(teardown, strict, challenge.id, "team instance")?;
        handle_teardown_result(released, strict, challenge.id, "team instance unlock")?;
    }

    let shared_key = format!("shared-container:{}", challenge.id);
    let _shared_flight = crate::utils::single_flight::coalesce(&shared_key).await;
    let shared_lock = match crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(
        st.pg(),
        &shared_key,
    )
    .await
    {
        Ok(lock) => lock,
        Err(error) => {
            tracing::warn!(challenge = challenge.id, %error, "challenge teardown lock failed");
            if strict {
                return Err(AppError::internal(error.to_string()));
            }
            return Ok(());
        }
    };
    if !teardown_allowed(st.pg(), challenge.id, require_inactive).await {
        let _ = shared_lock.release().await;
        return Ok(());
    }

    let shared_teardown: AppResult<()> = async {
        let targets = koth_target::Entity::find()
            .filter(koth_target::Column::ChallengeId.eq(challenge.id))
            .filter(koth_target::Column::ContainerId.is_not_null())
            .all(&st.db)
            .await?;
        for target in targets {
            let Some(container_id) = target.container_id.clone() else {
                continue;
            };
            let koth_game_ids =
                crate::services::ad_vpn::stage_backend_endpoint_deactivation_retaining_identity(
                    &st.db,
                    &container_id,
                )
                .await?;
            for game_id in koth_game_ids {
                crate::controllers::game::ad::invalidate_live_hill_snapshot(st, game_id).await;
            }
            crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await?;
            destroy_after_capture_fence(st, &container_id).await?;
            if !clear_destroyed_koth_target(st.pg(), target.id, &container_id).await?
                && koth_target_still_owns_backend(st.pg(), target.id).await?
            {
                return Err(AppError::conflict(
                    "KotH target changed during challenge teardown",
                ));
            }
        }

        let current_challenge = game_challenge::Entity::find_by_id(challenge.id)
            .one(&st.db)
            .await?;
        if let Some(current_challenge) = current_challenge {
            if let Some(sid) = current_challenge.shared_container_id {
                destroy_shared_container_after_capture_fence(st, sid).await?;
                clear_destroyed_shared_container(st.pg(), challenge.id, sid).await?;
            }
        }
        Ok(())
    }
    .await;
    let released = shared_lock
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()));
    handle_teardown_result(shared_teardown, strict, challenge.id, "shared/KotH runtime")?;
    handle_teardown_result(released, strict, challenge.id, "shared/KotH unlock")?;
    Ok(())
}

/// Clear only the exact shared runtime that was destroyed. Raw CAS avoids a
/// stale full-model update overwriting a concurrent title/hint edit and leaves
/// any replacement pointer intact.
async fn clear_destroyed_shared_container(
    pool: &sqlx::PgPool,
    challenge_id: i32,
    container_id: uuid::Uuid,
) -> AppResult<bool> {
    let changed = sqlx::query(
        r#"UPDATE "GameChallenges"
              SET shared_container_id = NULL
            WHERE id = $1 AND shared_container_id = $2"#,
    )
    .bind(challenge_id)
    .bind(container_id)
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    Ok(changed == 1)
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod tests;

/// Never release a backend identity until the singleton capture owner has
/// acknowledged that its old IP/port filter is gone.
pub(super) async fn destroy_after_capture_fence(
    st: &SharedState,
    backend_id: &str,
) -> AppResult<()> {
    crate::services::traffic::destroy_container_after_capture_fence(st, backend_id)
        .await
        .map_err(|error| {
        tracing::warn!(%backend_id, %error, "container destroy failed; retaining database pointer");
        error
    })
}

/// Remove one generic container row only after its backend is safely fenced and
/// destroyed. A missing row means the caller's pointer is already stale and may
/// be cleared; every other failure leaves that pointer available for retry.
pub(super) async fn destroy_container_row_after_capture_fence(
    st: &SharedState,
    container_id: uuid::Uuid,
) -> AppResult<()> {
    let Some(container) = container::Entity::find_by_id(container_id)
        .one(&st.db)
        .await?
    else {
        return Ok(());
    };
    destroy_after_capture_fence(st, &container.container_id).await?;
    container::Entity::delete_by_id(container_id)
        .exec(&st.db)
        .await?;
    Ok(())
}

pub(super) async fn destroy_shared_container_after_capture_fence(
    st: &SharedState,
    container_id: uuid::Uuid,
) -> AppResult<()> {
    let Some(container) = container::Entity::find_by_id(container_id)
        .one(&st.db)
        .await?
    else {
        return Ok(());
    };
    let koth_game_ids =
        crate::services::ad_vpn::stage_backend_endpoint_deactivation_retaining_identity(
            &st.db,
            &container.container_id,
        )
        .await?;
    for game_id in koth_game_ids {
        crate::controllers::game::ad::invalidate_live_hill_snapshot(st, game_id).await;
    }
    crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await?;
    destroy_after_capture_fence(st, &container.container_id).await?;
    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"UPDATE "KothTargets"
              SET container_id = NULL
            WHERE container_id = $1
              AND NULLIF(BTRIM(host), '') IS NULL AND port = 0"#,
    )
    .bind(&container.container_id)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(r#"DELETE FROM "Containers" WHERE id = $1 AND container_id = $2"#)
        .bind(container_id)
        .bind(&container.container_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

/// Caller holds `test-containers-game:{game_id}` through the subsequent challenge
/// delete, preventing a new test backend from being published after this re-query.
pub(super) async fn destroy_test_container_locked(
    st: &SharedState,
    challenge_id: i32,
) -> AppResult<()> {
    let current_test_id = game_challenge::Entity::find_by_id(challenge_id)
        .one(&st.db)
        .await?
        .and_then(|current| current.test_container_id);
    if let Some(container_id) = current_test_id {
        if let Some(container) = container::Entity::find_by_id(container_id)
            .one(&st.db)
            .await?
        {
            super::super::helpers::destroy_test_container_with(
                st.pg(),
                challenge_id,
                container_id,
                &container.container_id,
                super::super::helpers::revoke_and_destroy_backend(st, &container.container_id),
            )
            .await?;
        } else {
            sqlx::query(
                r#"UPDATE "GameChallenges" SET test_container_id = NULL
                    WHERE id = $1 AND test_container_id = $2"#,
            )
            .bind(challenge_id)
            .bind(container_id)
            .execute(st.pg())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        }
    }
    Ok(())
}
