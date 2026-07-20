use super::*;

/// Serialize binding configuration changes with scans and push-back. The row is
/// loaded only after the checkout fence, so a waiter cannot reapply stale state.
pub(crate) async fn update_repo_binding_record(
    st: &SharedState,
    id: i32,
    m: RepoBindingUpdateModel,
) -> AppResult<repo_binding::Model> {
    let dest = std::path::PathBuf::from(&st.config.storage_root)
        .join("repos")
        .join(id.to_string());
    let _checkout = crate::services::git_sync::lock_checkout_distributed(st.pg(), &dest).await?;
    let existing = repo_binding::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Repo binding not found"))?;
    let mut model: repo_binding::ActiveModel = existing.into();
    if let Some(value) = m.r#ref {
        model.git_ref = Set(crate::services::git_sync::validate_git_ref(Some(&value))?);
    }
    if let Some(value) = m.interval_seconds {
        model.interval_seconds = Set(value.max(0));
    }
    if let Some(value) = m.status {
        model.status = Set(if value == "Active" {
            RepoWatchStatus::Active
        } else {
            RepoWatchStatus::Paused
        });
    }
    if let Some(value) = m.github_token {
        model.github_token = Set(Some(value).filter(|token| !token.trim().is_empty()));
    }
    if let Some(value) = m.push_on_edit {
        model.push_on_edit = Set(value);
    }
    Ok(model.update(&st.db).await?)
}

/// Stop any active scan, then atomically detach retained games and remove the
/// binding/history. Challenge source identities remain available for audit.
pub(crate) async fn delete_repo_binding_record(st: &SharedState, id: i32) -> AppResult<bool> {
    let dest = std::path::PathBuf::from(&st.config.storage_root)
        .join("repos")
        .join(id.to_string());
    let _checkout = crate::services::git_sync::lock_checkout_distributed(st.pg(), &dest).await?;
    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let exists =
        sqlx::query_scalar::<_, i32>(r#"SELECT id FROM "RepoBindings" WHERE id = $1 FOR UPDATE"#)
            .bind(id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?
            .is_some();
    if !exists {
        transaction
            .rollback()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(false);
    }
    sqlx::query(r#"UPDATE "Games" SET repo_binding_id = NULL WHERE repo_binding_id = $1"#)
        .bind(id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(r#"DELETE FROM "RepoBindingScans" WHERE binding_id = $1"#)
        .bind(id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(r#"DELETE FROM "RepoBindings" WHERE id = $1"#)
        .bind(id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(true)
}

/// Persist only scan-owned columns; operator-owned binding configuration is
/// never written from the model snapshot captured at scan start.
pub(crate) async fn record_scan_completion(
    st: &SharedState,
    id: i32,
    ran_at: DateTime<Utc>,
    commit_sha: Option<String>,
    message: String,
    next_scan: DateTime<Utc>,
) -> AppResult<()> {
    let updated = sqlx::query(
        r#"UPDATE "RepoBindings"
              SET last_scan_utc = $2,
                  last_commit_sha = COALESCE($3, last_commit_sha),
                  last_scan_message = $4,
                  next_scan_utc = $5
            WHERE id = $1"#,
    )
    .bind(id)
    .bind(ran_at)
    .bind(commit_sha)
    .bind(message)
    .bind(next_scan)
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if updated.rows_affected() == 0 {
        return Err(AppError::not_found("Repo binding not found"));
    }
    Ok(())
}
