use super::*;

/// Never release a backend identity until the singleton capture owner has
/// acknowledged that its old IP/port filter is gone.
pub(super) async fn destroy_after_capture_fence(
    st: &SharedState,
    backend_id: &str,
) -> AppResult<()> {
    if let Err(error) = crate::services::traffic::stop_container_capture(st, backend_id).await {
        tracing::warn!(%backend_id, %error, "capture fence failed; retaining challenge container");
        return Err(error);
    }
    st.containers.destroy(backend_id).await.map_err(|error| {
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
    crate::services::ad_vpn::deactivate_backend_endpoint(&st.db, &container.container_id).await?;
    destroy_after_capture_fence(st, &container.container_id).await?;
    container::Entity::delete_by_id(container_id)
        .exec(&st.db)
        .await?;
    Ok(())
}

pub(super) async fn mark_challenge_deleting(st: &SharedState, challenge_id: i32) -> AppResult<()> {
    game_challenge::ActiveModel {
        id: Set(challenge_id),
        is_enabled: Set(false),
        ..Default::default()
    }
    .update(&st.db)
    .await?;
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
            destroy_after_capture_fence(st, &container.container_id).await?;
            container::Entity::delete_by_id(container_id)
                .exec(&st.db)
                .await?;
        }
    }
    Ok(())
}
