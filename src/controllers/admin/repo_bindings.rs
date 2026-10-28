//! HTTP admission for repository binding operations.
use super::*;
mod queries;
pub(crate) use crate::services::repository_bindings::{
    delete_repo_binding_record, update_repo_binding_record,
};
pub use crate::services::repository_bindings::{
    RepoBindingCreateModel, RepoBindingInfoModel, RepoBindingScanHistoryModel,
    RepoBindingScanResultModel, RepoBindingUpdateModel,
};
pub use queries::{list_repo_bindings, repo_binding_scans};

/// `POST /api/admin/repobindings` — register a repo, optionally scanning at once.
pub async fn create_repo_binding(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Json(m): Json<RepoBindingCreateModel>,
) -> AppResult<RequestResponse<RepoBindingScanResultModel>> {
    let repo_url = crate::services::git_sync::validate_binding_repo_url(&m.repo_url)?;
    let git_ref = crate::services::git_sync::validate_git_ref(m.r#ref.as_deref())?;
    let now = sqlx::query_scalar::<_, DateTime<Utc>>("SELECT clock_timestamp()")
        .fetch_one(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let interval = crate::services::repo_binding_scheduler::validate_interval(
        m.interval_seconds.unwrap_or(3600),
    )?;
    let am = repo_binding::ActiveModel {
        repo_url: Set(repo_url),
        git_ref: Set(git_ref),
        github_token: Set(m.github_token.filter(|s| !s.trim().is_empty())),
        interval_seconds: Set(interval),
        status: Set(RepoWatchStatus::Active),
        last_commit_sha: Set(None),
        last_scan_message: Set(None),
        last_scan_utc: Set(None),
        next_scan_utc: Set(Some(now + Duration::seconds(interval as i64))),
        created_at_utc: Set(now),
        push_on_edit: Set(false),
        ..Default::default()
    };
    let model = am.insert(&st.db).await?;
    let id = model.id;
    // RSCTF's create returns the scan result, not the binding: when scanning at
    // once, hand back the real counts; otherwise a zeroed result the client's
    // success toast can read (gamesCreated/... rather than NaN).
    let result = if m.run_immediately.unwrap_or(false) {
        // Best-effort: the binding exists whether or not the first scan succeeds.
        crate::services::repo_binding_scheduler::run_manual(st.clone(), id)
            .await
            .unwrap_or_else(|e| RepoBindingScanResultModel {
                games_created: 0,
                games_updated: 0,
                challenges_imported: 0,
                challenges_updated: 0,
                failures: 1,
                messages: vec![format!("scan failed: {e}")],
            })
    } else {
        RepoBindingScanResultModel {
            games_created: 0,
            games_updated: 0,
            challenges_imported: 0,
            challenges_updated: 0,
            failures: 0,
            messages: vec!["Repo binding created; scan not run.".to_string()],
        }
    };
    Ok(RequestResponse::ok(result))
}

/// `PUT /api/admin/repobindings/{id}` — patch only the provided fields.
pub async fn update_repo_binding(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<i32>,
    Json(m): Json<RepoBindingUpdateModel>,
) -> AppResult<RequestResponse<RepoBindingInfoModel>> {
    let model = update_repo_binding_record(&st, id, m).await?;
    Ok(RequestResponse::ok(
        queries::repo_info_after_update(&st, model).await?,
    ))
}

/// `DELETE /api/admin/repobindings/{id}` — detach retained games, then drop the
/// binding and its scan history.
pub async fn delete_repo_binding(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<i32>,
) -> AppResult<MessageResponse> {
    delete_repo_binding_record(&st, id).await?;
    Ok(MessageResponse::ok(""))
}

/// `POST /api/admin/repobindings/{id}/scan` — clone + import now.
pub async fn scan_repo_binding(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<RepoBindingScanResultModel>> {
    Ok(RequestResponse::ok(
        crate::services::repo_binding_scheduler::run_manual(st, id).await?,
    ))
}
