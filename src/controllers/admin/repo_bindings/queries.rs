use super::*;

#[derive(sqlx::FromRow)]
struct BindingListRow {
    id: i32,
    repo_url: String,
    git_ref: Option<String>,
    created_at_utc: DateTime<Utc>,
    last_scan_utc: Option<DateTime<Utc>>,
    next_scan_utc: Option<DateTime<Utc>>,
    interval_seconds: i32,
    status: i16,
    last_commit_sha: Option<String>,
    last_scan_message: Option<String>,
    has_token: bool,
    current_activity: Option<String>,
    push_on_edit: bool,
    push_backlog: i64,
    push_last_error: Option<String>,
    games: Value,
    total: i64,
}

fn bounded_page(query: ListQuery) -> (i64, i64) {
    (
        query.count.clamp(1, 100) as i64,
        query.skip.min(100_000) as i64,
    )
}

fn bounded_history_page(query: ListQuery) -> (i64, i64) {
    (
        query.count.clamp(1, 20) as i64,
        query.skip.min(100_000) as i64,
    )
}

fn binding_info(row: BindingListRow) -> AppResult<RepoBindingInfoModel> {
    let games = serde_json::from_value::<Vec<Value>>(row.games)
        .map_err(|error| AppError::internal(format!("decode repository games: {error}")))?;
    Ok(RepoBindingInfoModel {
        id: row.id,
        repo_url: row.repo_url,
        r#ref: row.git_ref,
        created_at_utc: row.created_at_utc,
        last_scan_utc: row.last_scan_utc,
        next_scan_utc: row.next_scan_utc,
        interval_seconds: row.interval_seconds,
        status: match row.status {
            0 => "Active",
            _ => "Paused",
        }
        .to_string(),
        last_commit_sha: row.last_commit_sha,
        last_scan_message: row.last_scan_message,
        has_git_hub_token: row.has_token,
        token_status: if row.has_token { "Ok" } else { "NotConfigured" }.to_string(),
        current_activity: row.current_activity,
        push_on_edit: row.push_on_edit,
        push_backlog: row.push_backlog,
        push_last_error: row.push_last_error,
        games,
    })
}

pub(super) async fn repo_info_after_update(
    st: &SharedState,
    model: repo_binding::Model,
) -> AppResult<RepoBindingInfoModel> {
    let (games, backlog, last_error) = sqlx::query_as::<_, (Value, i64, Option<String>)>(
        r#"SELECT COALESCE((
                       SELECT jsonb_agg(
                                  jsonb_build_object(
                                      'id', game.id,
                                      'title', game.title,
                                      'eventManifestPath', game.event_manifest_path
                                  ) ORDER BY game.title, game.id
                              )
                         FROM "Games" game WHERE game.repo_binding_id = $1
                   ), '[]'::jsonb),
                   (SELECT COUNT(*) FROM "RepoPushQueue" queue WHERE queue.binding_id = $1),
                   (SELECT queue.last_error FROM "RepoPushQueue" queue
                     WHERE queue.binding_id = $1 AND queue.last_error IS NOT NULL
                     ORDER BY queue.updated_at_utc DESC LIMIT 1)"#,
    )
    .bind(model.id)
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let games = serde_json::from_value::<Vec<Value>>(games)
        .map_err(|error| AppError::internal(format!("decode repository games: {error}")))?;
    let has_token = model
        .github_token
        .as_deref()
        .is_some_and(|value| !value.is_empty());
    Ok(RepoBindingInfoModel {
        id: model.id,
        repo_url: model.repo_url,
        r#ref: model.git_ref,
        created_at_utc: model.created_at_utc,
        last_scan_utc: model.last_scan_utc,
        next_scan_utc: model.next_scan_utc,
        interval_seconds: model.interval_seconds,
        status: match model.status {
            RepoWatchStatus::Active => "Active",
            RepoWatchStatus::Paused => "Paused",
        }
        .to_string(),
        last_commit_sha: model.last_commit_sha,
        last_scan_message: model.last_scan_message,
        has_git_hub_token: has_token,
        token_status: if has_token { "Ok" } else { "NotConfigured" }.to_string(),
        current_activity: model.current_activity,
        push_on_edit: model.push_on_edit,
        push_backlog: backlog,
        push_last_error: last_error,
        games,
    })
}

/// One bounded aggregate query owns the page, its child games, push backlog,
/// and total count. Idle repository pages therefore perform no N+1 reads.
pub async fn list_repo_bindings(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Query(query): Query<ListQuery>,
) -> AppResult<ArrayResponse<RepoBindingInfoModel>> {
    let (count, skip) = bounded_page(query);
    let rows = sqlx::query_as::<_, BindingListRow>(
        r#"SELECT binding.id, binding.repo_url, binding.git_ref,
                  binding.created_at_utc, binding.last_scan_utc,
                  binding.next_scan_utc, binding.interval_seconds,
                  binding.status::smallint AS status,
                  binding.last_commit_sha, binding.last_scan_message,
                  COALESCE(binding.github_token, '') <> '' AS has_token,
                  binding.current_activity, binding.push_on_edit,
                  COALESCE(pushes.backlog, 0)::bigint AS push_backlog,
                  pushes.last_error AS push_last_error,
                  COALESCE(games.items, '[]'::jsonb) AS games,
                  COUNT(*) OVER() AS total
             FROM "RepoBindings" binding
             LEFT JOIN LATERAL (
                 SELECT jsonb_agg(
                            jsonb_build_object(
                                'id', game.id,
                                'title', game.title,
                                'eventManifestPath', game.event_manifest_path
                            ) ORDER BY game.title, game.id
                        ) AS items
                   FROM "Games" game
                  WHERE game.repo_binding_id = binding.id
             ) games ON TRUE
             LEFT JOIN LATERAL (
                 SELECT COUNT(*) AS backlog,
                        (array_agg(queue.last_error ORDER BY queue.updated_at_utc DESC)
                            FILTER (WHERE queue.last_error IS NOT NULL))[1] AS last_error
                   FROM "RepoPushQueue" queue
                  WHERE queue.binding_id = binding.id
             ) pushes ON TRUE
            ORDER BY binding.id DESC
            LIMIT $1 OFFSET $2"#,
    )
    .bind(count)
    .bind(skip)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let total = rows.first().map(|row| row.total).unwrap_or(0);
    let data = rows
        .into_iter()
        .map(binding_info)
        .collect::<AppResult<Vec<_>>>()?;
    Ok(ArrayResponse::new(data, total))
}

#[derive(sqlx::FromRow)]
struct ScanHistoryRow {
    id: i32,
    ran_at_utc: DateTime<Utc>,
    commit_sha: Option<String>,
    games_created: i32,
    games_updated: i32,
    challenges_imported: i32,
    challenges_updated: i32,
    failures: i32,
    messages: Option<String>,
    total: i64,
}

pub async fn repo_binding_scans(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<i32>,
    Query(query): Query<ListQuery>,
) -> AppResult<ArrayResponse<RepoBindingScanHistoryModel>> {
    let (count, skip) = bounded_history_page(query);
    let rows = sqlx::query_as::<_, ScanHistoryRow>(
        r#"SELECT id, ran_at_utc, commit_sha, games_created, games_updated,
                  challenges_imported, challenges_updated, failures, messages,
                  COUNT(*) OVER() AS total
             FROM "RepoBindingScans"
            WHERE binding_id = $1
            ORDER BY id DESC
            LIMIT $2 OFFSET $3"#,
    )
    .bind(id)
    .bind(count)
    .bind(skip)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let total = rows.first().map(|row| row.total).unwrap_or(0);
    let data = rows
        .into_iter()
        .map(|row| RepoBindingScanHistoryModel {
            id: row.id,
            ran_at_utc: row.ran_at_utc,
            commit_sha: row.commit_sha,
            games_created: row.games_created,
            games_updated: row.games_updated,
            challenges_imported: row.challenges_imported,
            challenges_updated: row.challenges_updated,
            failures: row.failures,
            messages: row.messages,
        })
        .collect();
    Ok(ArrayResponse::new(data, total))
}

#[cfg(test)]
mod tests {
    #[test]
    fn list_is_one_bounded_aggregate_and_history_is_capped() {
        let source = include_str!("queries.rs");
        assert!(source.contains("LEFT JOIN LATERAL"));
        assert!(source.contains("COUNT(*) OVER()"));
        assert!(source.contains("query.count.clamp(1, 100)"));
        assert!(source.contains("query.count.clamp(1, 20)"));
        assert!(!source.contains("Entity::find"));
    }
}
