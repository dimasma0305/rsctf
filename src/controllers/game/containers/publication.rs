use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};

pub(super) struct TeamPublication<'a> {
    pub container_id: uuid::Uuid,
    pub backend_id: &'a str,
    pub image: &'a str,
    pub is_proxy: bool,
    pub ip: &'a str,
    pub port: i32,
    pub participation_id: i32,
    pub challenge_id: i32,
    pub existing_instance_id: Option<i32>,
    pub dynamic_flag: Option<&'a str>,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub expect_stop_at: chrono::DateTime<chrono::Utc>,
}

/// Publish the runtime owner, optional dynamic flag, and instance link as one
/// conditional transaction on the caller's advisory-lock connection.
pub(super) async fn publish_team_container_locked(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    publication: TeamPublication<'_>,
) -> AppResult<crate::models::data::container::Model> {
    let flag_id = match publication.dynamic_flag {
        Some(flag) => Some(
            sqlx::query_scalar::<_, i32>(
                r#"INSERT INTO "FlagContexts"
                       (flag, is_occupied, attachment_id, challenge_id, exercise_id)
                   VALUES ($1, TRUE, NULL, $2, NULL)
                RETURNING id"#,
            )
            .bind(flag)
            .bind(publication.challenge_id)
            .fetch_one(&mut **transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?,
        ),
        None => None,
    };
    let instance_id = match publication.existing_instance_id {
        Some(instance_id) => {
            let owned: bool = sqlx::query_scalar(
                r#"SELECT EXISTS(
                       SELECT 1 FROM "GameInstances"
                        WHERE id = $1 AND participation_id = $2
                          AND challenge_id = $3 AND container_id IS NULL
                   )"#,
            )
            .bind(instance_id)
            .bind(publication.participation_id)
            .bind(publication.challenge_id)
            .fetch_one(&mut **transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
            if !owned {
                return Err(AppError::conflict(
                    "The challenge instance changed while the container was starting",
                ));
            }
            instance_id
        }
        None => sqlx::query_scalar::<_, i32>(
            r#"INSERT INTO "GameInstances"
                       (challenge_id, participation_id, is_loaded,
                        last_container_operation, flag_id, container_id)
                   VALUES ($1, $2, TRUE, $3, $4, NULL)
                RETURNING id"#,
        )
        .bind(publication.challenge_id)
        .bind(publication.participation_id)
        .bind(publication.started_at)
        .bind(flag_id)
        .fetch_one(&mut **transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?,
    };
    sqlx::query(
        r#"INSERT INTO "Containers"
               (id, image, container_id, status, started_at, expect_stop_at,
                is_proxy, ip, port, public_ip, public_port, game_instance_id,
                exercise_instance_id, ad_team_service_id)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9,
                   NULL, NULL, $10, NULL, NULL)"#,
    )
    .bind(publication.container_id)
    .bind(publication.image)
    .bind(publication.backend_id)
    .bind(crate::utils::enums::ContainerStatus::Running as i16)
    .bind(publication.started_at)
    .bind(publication.expect_stop_at)
    .bind(publication.is_proxy)
    .bind(publication.ip)
    .bind(publication.port)
    .bind(instance_id)
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let linked = sqlx::query(
        r#"UPDATE "GameInstances"
              SET container_id = $2, flag_id = $3, is_loaded = TRUE,
                  last_container_operation = $4
            WHERE id = $1 AND container_id IS NULL"#,
    )
    .bind(instance_id)
    .bind(publication.container_id)
    .bind(flag_id)
    .bind(publication.started_at)
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if linked.rows_affected() != 1 {
        return Err(AppError::conflict(
            "The challenge instance changed while the container was starting",
        ));
    }
    Ok(crate::models::data::container::Model {
        id: publication.container_id,
        image: publication.image.to_owned(),
        container_id: publication.backend_id.to_owned(),
        status: crate::utils::enums::ContainerStatus::Running,
        started_at: publication.started_at,
        expect_stop_at: publication.expect_stop_at,
        is_proxy: publication.is_proxy,
        ip: publication.ip.to_owned(),
        port: publication.port,
        public_ip: None,
        public_port: None,
        game_instance_id: Some(instance_id),
        exercise_instance_id: None,
        ad_team_service_id: None,
    })
}

/// Refresh a live KotH target's managed-container lease while its caller holds
/// the challenge's shared-container lock. A missing bookkeeping row returns
/// `false` so provisioning can revoke the stale endpoint and recreate it.
pub(crate) async fn refresh_shared_container_lease_locked(
    st: &SharedState,
    backend_id: &str,
) -> AppResult<bool> {
    let policy = crate::services::container_policy::ContainerPolicy::load(st.pg()).await?;
    let stop_at =
        chrono::Utc::now() + chrono::Duration::minutes(i64::from(policy.default_lifetime));
    let result = sqlx::query(
        r#"UPDATE "Containers" SET expect_stop_at = $2
            WHERE container_id = $1"#,
    )
    .bind(backend_id)
    .bind(stop_at)
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(result.rows_affected() != 0)
}

pub(super) async fn revoke_published_team_container(
    st: &SharedState,
    backend_id: &str,
    container_id: uuid::Uuid,
    instance_id: i32,
    created_instance_id: Option<i32>,
    created_flag_id: Option<i32>,
) -> AppResult<()> {
    // The Containers row is the retry identity. Never erase it unless the
    // capture owner has fenced the old route and the backend confirms destroy.
    crate::services::traffic::destroy_container_after_capture_fence(st, backend_id).await?;

    clear_destroyed_team_container(
        st,
        backend_id,
        container_id,
        Some(instance_id),
        created_instance_id,
        created_flag_id,
        false,
    )
    .await
}

/// Roll back a publication that may have failed between its non-transactional
/// writes. Backend failure returns before any owner row is removed; successful
/// destroy also permits removing a request-created instance that never linked.
pub(super) async fn revoke_failed_team_container_publication(
    st: &SharedState,
    backend_id: &str,
    container_id: uuid::Uuid,
    instance_id: Option<i32>,
    created_instance_id: Option<i32>,
    created_flag_id: Option<i32>,
) -> AppResult<()> {
    crate::services::traffic::destroy_container_after_capture_fence(st, backend_id).await?;

    clear_destroyed_team_container(
        st,
        backend_id,
        container_id,
        instance_id,
        created_instance_id,
        created_flag_id,
        true,
    )
    .await
}

async fn clear_destroyed_team_container(
    st: &SharedState,
    backend_id: &str,
    container_id: uuid::Uuid,
    instance_id: Option<i32>,
    created_instance_id: Option<i32>,
    created_flag_id: Option<i32>,
    allow_unlinked_created_instance: bool,
) -> AppResult<()> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    match instance_id {
        Some(instance_id) if created_instance_id == Some(instance_id) => {
            sqlx::query(
                r#"DELETE FROM "GameInstances"
                    WHERE id = $1
                      AND (
                          container_id = $2
                          OR ($3 AND container_id IS NULL)
                      )"#,
            )
            .bind(instance_id)
            .bind(container_id)
            .bind(allow_unlinked_created_instance)
            .execute(&mut *transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        }
        Some(instance_id) => {
            sqlx::query(
                r#"UPDATE "GameInstances"
                      SET container_id = NULL,
                          is_loaded = FALSE,
                          flag_id = CASE WHEN flag_id = $3 THEN NULL ELSE flag_id END,
                          last_container_operation = CURRENT_TIMESTAMP
                    WHERE id = $1 AND container_id = $2"#,
            )
            .bind(instance_id)
            .bind(container_id)
            .bind(created_flag_id)
            .execute(&mut *transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        }
        None => {}
    }
    // Every ownership mutation is compare-and-swap. A concurrent replacement
    // therefore remains untouched; zero affected rows is a valid stale cleanup.
    sqlx::query(r#"DELETE FROM "Containers" WHERE id = $1 AND container_id = $2"#)
        .bind(container_id)
        .bind(backend_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if let Some(flag_id) = created_flag_id {
        sqlx::query(
            r#"DELETE FROM "FlagContexts" flag
                WHERE flag.id = $1
                  AND NOT EXISTS (
                      SELECT 1 FROM "GameInstances" instance WHERE instance.flag_id = flag.id
                  )"#,
        )
        .bind(flag_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

pub(super) async fn revoke_published_shared_container(
    st: &SharedState,
    challenge_id: i32,
    container_id: uuid::Uuid,
    backend_id: &str,
) -> AppResult<()> {
    // Persist the restrictive endpoint first while retaining both the KothTarget
    // and Containers identities. Cache eviction precedes the kernel policy fence
    // so no stale hill address can outlive a successful backend destroy.
    let game_ids = crate::services::ad_vpn::stage_backend_endpoint_deactivation_retaining_identity(
        &st.db, backend_id,
    )
    .await?;
    for game_id in game_ids {
        crate::controllers::game::ad::invalidate_live_hill_snapshot(st, game_id).await;
    }
    crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await?;
    crate::services::traffic::destroy_container_after_capture_fence(st, backend_id).await?;

    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"UPDATE "KothTargets"
              SET container_id = NULL
            WHERE container_id = $1
              AND NULLIF(BTRIM(host), '') IS NULL AND port = 0"#,
    )
    .bind(backend_id)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"UPDATE "GameChallenges"
              SET shared_container_id = NULL
            WHERE id = $1 AND shared_container_id = $2"#,
    )
    .bind(challenge_id)
    .bind(container_id)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    // The challenge may already point at a replacement. The CAS above leaves it
    // intact while this exact stale Containers row is independently reclaimed.
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
