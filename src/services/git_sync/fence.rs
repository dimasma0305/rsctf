//! Two-phase database fence for repository imports.
//!
//! Imports first take a short game/definition snapshot, release every domain
//! lock while packaging and staging content, then reacquire the canonical
//! game -> definition order and prove the exact rows are unchanged before
//! publishing metadata.

use std::path::Path;

use super::{
    durable_repo_manifest_path, game, game_challenge,
    repository::{legacy_manifest_lookup_parameters, REPOSITORY_MANIFEST_LOOKUP_SQL},
    ImportPolicy,
};
use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};

#[derive(Clone)]
pub(super) struct ImportSnapshot {
    pub(super) game: game::Model,
    pub(super) existing: Option<game_challenge::Model>,
    pub(super) source_yaml_path: Option<String>,
    game_revision: String,
    challenge_revision: Option<String>,
}

pub(super) struct ImportMutationFence {
    game: crate::services::ad_engine::GameControlLock,
}

impl ImportMutationFence {
    /// Reserve the database-generated ID while the game fence is held, then
    /// acquire its definition fence before the row can become visible. Sequence
    /// gaps on a failed import are harmless; a post-commit lock would leave a
    /// real mutation window for definition-only writers.
    pub(super) async fn reserve_created_challenge(&mut self, game_id: i32) -> AppResult<i32> {
        let challenge_id = sqlx::query_scalar::<_, i64>(
            r#"SELECT nextval(pg_get_serial_sequence('"GameChallenges"', 'id'))"#,
        )
        .fetch_one(&mut **self.game.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        let challenge_id = i32::try_from(challenge_id)
            .map_err(|_| AppError::internal("challenge identity exceeded integer range"))?;
        try_definition(self.game.transaction_mut(), game_id, challenge_id).await?;
        Ok(challenge_id)
    }

    pub(super) async fn release(self) -> AppResult<()> {
        self.game
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))
    }
}

async fn challenge_revision(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<(String, bool)> {
    sqlx::query_as::<_, (String, bool)>(
        r#"SELECT xmin::text, deletion_pending
              FROM "GameChallenges"
             WHERE id = $1 AND game_id = $2"#,
    )
    .bind(challenge_id)
    .bind(game_id)
    .fetch_optional(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Challenge not found"))
}

async fn game_revision(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
) -> AppResult<(String, bool)> {
    sqlx::query_as::<_, (String, bool)>(
        r#"SELECT xmin::text, deletion_pending FROM "Games" WHERE id = $1"#,
    )
    .bind(game_id)
    .fetch_optional(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found(format!("game {game_id} not found")))
}

async fn ensure_epoch_is_open(connection: &mut sqlx::PgConnection, game_id: i32) -> AppResult<()> {
    if crate::controllers::edit::ad_epoch_scoring_started_locked(connection, game_id).await? {
        return Err(AppError::conflict(
            "Challenge import is locked after A&D/KotH epoch scoring has started.",
        ));
    }
    Ok(())
}

async fn load_game_locked(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
) -> AppResult<game::Model> {
    let value = sqlx::query_scalar::<_, serde_json::Value>(
        r#"SELECT to_jsonb(game) FROM "Games" game WHERE id = $1"#,
    )
    .bind(game_id)
    .fetch_optional(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found(format!("game {game_id} not found")))?;
    serde_json::from_value(value)
        .map_err(|error| AppError::internal(format!("could not decode game row: {error}")))
}

async fn resolve_existing_locked(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    game: &game::Model,
    source_yaml_path: Option<&str>,
    import_source_identity: Option<&str>,
) -> AppResult<Option<game_challenge::Model>> {
    let ids = match import_source_identity {
        Some(source_identity) => sqlx::query_scalar::<_, i32>(
            r#"SELECT id FROM "GameChallenges"
                    WHERE game_id = $1 AND import_source_identity = $2
                    LIMIT 2"#,
        )
        .bind(game_id)
        .bind(source_identity)
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?,
        None => {
            let Some(source_yaml_path) = source_yaml_path else {
                return Ok(None);
            };
            let (legacy_relative, legacy_suffix_pattern) =
                legacy_manifest_lookup_parameters(game.repo_binding_id, source_yaml_path);
            sqlx::query_scalar::<_, i32>(REPOSITORY_MANIFEST_LOOKUP_SQL)
                .bind(game_id)
                .bind(source_yaml_path)
                .bind(legacy_relative)
                .bind(legacy_suffix_pattern)
                .fetch_all(&mut *connection)
                .await
                .map_err(|error| AppError::internal(error.to_string()))?
        }
    };
    match ids.as_slice() {
        [] => Ok(None),
        [challenge_id] => {
            crate::controllers::edit::load_challenge_locked(connection, game_id, *challenge_id)
                .await
                .map(Some)
        }
        _ => Err(AppError::bad_request(
            "repository manifest is already linked to multiple challenges",
        )),
    }
}

async fn try_definition(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<()> {
    let key = crate::services::challenge_workloads::definition_lock_key(game_id, challenge_id);
    let acquired =
        crate::utils::single_flight::try_acquire_transaction_advisory_lock(transaction, &key)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    if !acquired {
        return Err(AppError::conflict(
            "challenge definition is being updated; retry the repository scan",
        ));
    }
    Ok(())
}

/// Capture the exact game and challenge rows under the canonical mutation
/// fences. No filesystem, object-store, container, or checker work occurs while
/// these locks are retained.
pub(super) async fn snapshot_import(
    st: &SharedState,
    game_id: i32,
    manifest: &Path,
    import_source_identity: Option<&str>,
    source_yaml_path_override: Option<&str>,
    policy: ImportPolicy,
) -> AppResult<ImportSnapshot> {
    // Canonicalizing the repository path may hit a slow filesystem. Use only a
    // binding hint here, then prove it still matches the locked game row.
    let binding_hint = sqlx::query_scalar::<_, Option<i32>>(
        r#"SELECT repo_binding_id FROM "Games" WHERE id = $1"#,
    )
    .bind(game_id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found(format!("game {game_id} not found")))?;
    if import_source_identity.is_some() && source_yaml_path_override.is_some() {
        return Err(AppError::internal(
            "repository and import-job identities are mutually exclusive",
        ));
    }
    if let Some(source_path) = source_yaml_path_override {
        let expected_prefix = binding_hint.map(|binding_id| format!("binding/{binding_id}/"));
        if expected_prefix
            .as_deref()
            .is_none_or(|prefix| !source_path.starts_with(prefix))
        {
            return Err(AppError::internal(
                "repository snapshot identity does not match the game's binding",
            ));
        }
    }
    let source_yaml_path = source_yaml_path_override.map(str::to_owned).or_else(|| {
        if import_source_identity.is_none() {
            durable_repo_manifest_path(&st.config.storage_root, binding_hint, manifest)
        } else {
            None
        }
    });
    let mut game_lock = crate::services::ad_engine::acquire_ad_game_lock(&st.db, game_id).await?;
    ensure_epoch_is_open(game_lock.transaction_mut(), game_id).await?;
    let (game_revision, game_deletion_pending) =
        game_revision(game_lock.transaction_mut(), game_id).await?;
    if game_deletion_pending {
        return Err(AppError::conflict("Game is being deleted"));
    }
    let game = load_game_locked(game_lock.transaction_mut(), game_id).await?;
    if game.repo_binding_id != binding_hint {
        return Err(AppError::conflict(
            "repository binding changed while the import snapshot was acquired; retry",
        ));
    }
    let initial_existing = resolve_existing_locked(
        game_lock.transaction_mut(),
        game_id,
        &game,
        source_yaml_path.as_deref(),
        import_source_identity,
    )
    .await?;
    if let Some(challenge) = initial_existing.as_ref() {
        try_definition(game_lock.transaction_mut(), game_id, challenge.id).await?;
    }
    // A definition-only writer can commit between identity discovery and the
    // definition fence. Resolve the model again under that fence so the model
    // and xmin always describe the same committed row.
    let existing = resolve_existing_locked(
        game_lock.transaction_mut(),
        game_id,
        &game,
        source_yaml_path.as_deref(),
        import_source_identity,
    )
    .await?;
    if existing.as_ref().map(|challenge| challenge.id)
        != initial_existing.as_ref().map(|challenge| challenge.id)
    {
        return Err(AppError::conflict(
            "repository challenge identity changed while its definition fence was acquired; retry",
        ));
    }
    let challenge_revision = match existing.as_ref() {
        Some(challenge) => {
            let (revision, deletion_pending) =
                challenge_revision(game_lock.transaction_mut(), game_id, challenge.id).await?;
            if deletion_pending {
                return Err(AppError::conflict("Challenge is being deleted"));
            }
            Some(revision)
        }
        None => None,
    };
    // Reject known-full pending queues before packaging/staging large content;
    // the same predicate is repeated authoritatively after reacquisition.
    ensure_pending_capacity(
        game_lock.transaction_mut(),
        game_id,
        policy,
        existing.is_none(),
    )
    .await?;
    game_lock
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(ImportSnapshot {
        game,
        existing,
        source_yaml_path,
        game_revision,
        challenge_revision,
    })
}

async fn ensure_pending_capacity(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    policy: ImportPolicy,
    is_new: bool,
) -> AppResult<()> {
    let (Some(submitted_by_user_id), true) = (policy.submitted_by_user_id(), is_new) else {
        return Ok(());
    };
    let pending = sqlx::query_scalar::<_, i64>(
        r#"SELECT COUNT(*)
              FROM "GameChallenges"
             WHERE game_id = $1
               AND submitted_by_user_id = $2
               AND review_status = $3"#,
    )
    .bind(game_id)
    .bind(submitted_by_user_id)
    .bind(crate::utils::enums::ChallengeReviewStatus::Pending as i16)
    .fetch_one(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if pending >= super::policy::MAX_PENDING_CHALLENGES_PER_USER_GAME {
        return Err(AppError::bad_request(format!(
            "At most {} pending challenges may be submitted per user and game.",
            super::policy::MAX_PENDING_CHALLENGES_PER_USER_GAME
        )));
    }
    Ok(())
}

/// Reacquire the canonical game -> definition fences and prove the exact rows
/// captured by [`snapshot_import`] remain current. The returned guard may be
/// retained only for short database publication work.
pub(super) async fn reacquire_import(
    st: &SharedState,
    game_id: i32,
    snapshot: &ImportSnapshot,
    import_source_identity: Option<&str>,
    policy: ImportPolicy,
) -> AppResult<ImportMutationFence> {
    let mut game_lock = crate::services::ad_engine::acquire_ad_game_lock(&st.db, game_id).await?;
    ensure_epoch_is_open(game_lock.transaction_mut(), game_id).await?;
    let (current_game_revision, deletion_pending) =
        game_revision(game_lock.transaction_mut(), game_id).await?;
    if deletion_pending {
        return Err(AppError::conflict("Game is being deleted"));
    }
    if current_game_revision != snapshot.game_revision {
        return Err(AppError::conflict(
            "game settings changed while repository content was staged; retry",
        ));
    }
    let current_game = load_game_locked(game_lock.transaction_mut(), game_id).await?;
    let current = resolve_existing_locked(
        game_lock.transaction_mut(),
        game_id,
        &current_game,
        snapshot.source_yaml_path.as_deref(),
        import_source_identity,
    )
    .await?;
    let expected_id = snapshot.existing.as_ref().map(|challenge| challenge.id);
    if current.as_ref().map(|challenge| challenge.id) != expected_id {
        return Err(AppError::conflict(
            "repository challenge identity changed while content was staged; retry",
        ));
    }
    if let Some(challenge_id) = expected_id {
        try_definition(game_lock.transaction_mut(), game_id, challenge_id).await?;
        let fenced_current = resolve_existing_locked(
            game_lock.transaction_mut(),
            game_id,
            &current_game,
            snapshot.source_yaml_path.as_deref(),
            import_source_identity,
        )
        .await?;
        if fenced_current.as_ref().map(|challenge| challenge.id) != expected_id {
            return Err(AppError::conflict(
                "repository challenge identity changed while its definition fence was acquired; retry",
            ));
        }
    }
    ensure_pending_capacity(
        game_lock.transaction_mut(),
        game_id,
        policy,
        expected_id.is_none(),
    )
    .await?;
    if let Some(challenge_id) = expected_id {
        let (current_revision, challenge_deletion_pending) =
            challenge_revision(game_lock.transaction_mut(), game_id, challenge_id).await?;
        if challenge_deletion_pending {
            return Err(AppError::conflict("Challenge is being deleted"));
        }
        if Some(current_revision) != snapshot.challenge_revision {
            return Err(AppError::conflict(
                "challenge changed while repository content was staged; retry",
            ));
        }
    }
    Ok(ImportMutationFence { game: game_lock })
}

#[cfg(test)]
mod tests {
    use super::super::repository::REPOSITORY_MANIFEST_LOOKUP_SQL;

    #[test]
    fn revision_queries_use_database_owned_mvcc_fences() {
        let source = include_str!("fence.rs");
        assert!(source.matches("xmin::text").count() >= 2);
        assert!(source.contains("game settings changed while repository content was staged"));
        assert!(source.contains("challenge changed while repository content was staged"));
        assert!(source.contains("try_definition(game_lock.transaction_mut()"));
        assert!(!source.contains("try_acquire_definition_lock("));
    }

    #[test]
    fn repository_identity_queries_bound_ambiguity_inside_postgres() {
        assert!(REPOSITORY_MANIFEST_LOOKUP_SQL.contains("LIMIT 2"));
        assert!(REPOSITORY_MANIFEST_LOOKUP_SQL.contains("reverse(replace(source_yaml_path"));
        assert!(REPOSITORY_MANIFEST_LOOKUP_SQL.contains("LIKE $4 ESCAPE '!'"));
        let source = include_str!("fence.rs");
        assert!(!source.contains("repository_manifest_identity_matches"));
        assert!(!source.contains("SELECT id, source_yaml_path"));
    }
}
