use super::*;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum DeleteContainerOutcome {
    AlreadyAbsent,
    Destroyed { audit_id: String },
}

/// Destroy only the runtime still owned by the refreshed player snapshot.
///
/// The caller must hold `game-container:{participation_id}`. Creation, deletion,
/// and reaping use that same PostgreSQL advisory identity, so the row read and
/// immutable-ID comparison happen after every earlier replacement has committed.
pub(super) async fn delete_expected_team_container_locked(
    st: &SharedState,
    participation_id: i32,
    challenge_id: i32,
    expected_container_id: Uuid,
) -> AppResult<DeleteContainerOutcome> {
    let Some(instance) = game_instance::Entity::find()
        .filter(game_instance::Column::ParticipationId.eq(participation_id))
        .filter(game_instance::Column::ChallengeId.eq(challenge_id))
        .one(&st.db)
        .await?
    else {
        return Ok(DeleteContainerOutcome::AlreadyAbsent);
    };
    let Some(current_container_id) = instance.container_id else {
        return Ok(DeleteContainerOutcome::AlreadyAbsent);
    };
    if current_container_id != expected_container_id {
        return Err(AppError::conflict(
            "The challenge instance changed; refresh and retry.",
        ));
    }

    // Apply the frequency gate only after the identity precondition. A stale
    // caller must receive a conflict without learning or operating on B's lease.
    if let Some(error) = container_op_too_frequent(&instance) {
        return Err(error);
    }
    let container = container::Entity::find_by_id(current_container_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| {
            AppError::conflict("container bookkeeping is missing; retry after reconciliation")
        })?;
    let audit_id = format!(
        "<{}> {}",
        &container.id.simple().to_string()[..12],
        container.container_id
    );
    revoke_published_team_container(
        st,
        &container.container_id,
        container.id,
        instance.id,
        None,
        None,
    )
    .await?;
    Ok(DeleteContainerOutcome::Destroyed { audit_id })
}
