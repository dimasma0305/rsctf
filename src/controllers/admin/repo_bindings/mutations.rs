use super::*;

async fn remove_repo_checkout(path: &std::path::Path) -> AppResult<()> {
    let metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(AppError::internal(format!(
                "inspect repository checkout {} before removal: {error}",
                path.display()
            )));
        }
    };
    let result = if metadata.file_type().is_dir() {
        tokio::fs::remove_dir_all(path).await
    } else {
        // A checkout path should be a directory. Unlink an unexpected file or
        // symlink itself instead of following it outside the repository root.
        tokio::fs::remove_file(path).await
    };
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AppError::internal(format!(
            "remove repository checkout {}: {error}",
            path.display()
        ))),
    }
}

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
    let checkout = crate::services::git_sync::lock_checkout_distributed(st.pg(), &dest).await?;
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
    } else {
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
    }

    // Keep the checkout fence through filesystem cleanup. Cleanup also runs
    // when the row is already gone so a retry can finish after a prior
    // post-commit filesystem error.
    let cleanup = remove_repo_checkout(&dest).await;
    drop(checkout);
    cleanup?;
    Ok(exists)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn repo_checkout_removal_is_recursive_and_retry_safe() {
        let root = std::env::temp_dir().join(format!(
            "rsctf-repo-binding-delete-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let checkout = root.join("repos").join("42");
        let nested = checkout.join(".git").join("objects");
        tokio::fs::create_dir_all(&nested).await.unwrap();
        tokio::fs::write(nested.join("sentinel"), b"checkout")
            .await
            .unwrap();

        remove_repo_checkout(&checkout).await.unwrap();
        assert!(!tokio::fs::try_exists(&checkout).await.unwrap());
        remove_repo_checkout(&checkout).await.unwrap();

        let _ = tokio::fs::remove_dir_all(root).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn repo_checkout_removal_does_not_follow_symlinks() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "rsctf-repo-binding-symlink-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let checkout_parent = root.join("repos");
        let checkout = checkout_parent.join("42");
        let external = root.join("external");
        tokio::fs::create_dir_all(&checkout_parent).await.unwrap();
        tokio::fs::create_dir_all(&external).await.unwrap();
        tokio::fs::write(external.join("sentinel"), b"keep")
            .await
            .unwrap();
        symlink(&external, &checkout).unwrap();

        remove_repo_checkout(&checkout).await.unwrap();

        assert!(!tokio::fs::try_exists(&checkout).await.unwrap());
        assert!(tokio::fs::try_exists(external.join("sentinel"))
            .await
            .unwrap());
        let _ = tokio::fs::remove_dir_all(root).await;
    }
}
