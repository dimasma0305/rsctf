use super::*;

/// `GET /api/admin/repobindings` — every configured binding, newest first.
pub async fn list_repo_bindings(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Query(page): Query<crate::utils::shared::PageParams>,
) -> AppResult<ArrayResponse<RepoBindingInfoModel>> {
    let limit = page.count.clamp(1, BINDING_LIST_LIMIT) as i64;
    let skip = i64::try_from(page.skip).unwrap_or(i64::MAX);
    let rows = sqlx::query_as::<_, (Option<Value>, Option<Value>, i64, Option<String>, i64)>(
        r#"WITH counted AS (
               SELECT COUNT(*)::BIGINT AS total FROM "RepoBindings"
           ), bounded AS (
               SELECT * FROM "RepoBindings"
                ORDER BY id DESC LIMIT $1 OFFSET $2
           )
           SELECT CASE WHEN binding.id IS NULL THEN NULL ELSE
                    to_jsonb(binding) || jsonb_build_object(
                      'status', CASE binding.status
                          WHEN 0 THEN 'Active' WHEN 1 THEN 'Paused' END)
                  END,
                  CASE WHEN binding.id IS NULL THEN NULL ELSE COALESCE((
                      SELECT jsonb_agg(jsonb_build_object(
                                 'id', game.id,
                                 'title', game.title,
                                 'eventManifestPath', game.event_manifest_path)
                               ORDER BY game.title, game.id)
                       FROM "Games" game
                       WHERE game.repo_binding_id = binding.id
                  ), '[]'::jsonb) END,
                  (SELECT COUNT(*)::BIGINT FROM "RepoBindingPushJobs" job
                    WHERE job.binding_id = binding.id),
                  (SELECT last_error FROM "RepoBindingPushJobs" job
                    WHERE job.binding_id = binding.id AND last_error IS NOT NULL
                    ORDER BY updated_at_utc DESC, challenge_id DESC LIMIT 1),
                  counted.total
             FROM counted
             LEFT JOIN bounded binding ON TRUE
            ORDER BY binding.id DESC NULLS LAST"#,
    )
    .bind(limit)
    .bind(skip)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let mut out = Vec::with_capacity(rows.len());
    let mut total = 0;
    for (binding, games, pending_pushes, push_error, row_total) in rows {
        total = row_total;
        let (Some(binding), Some(games)) = (binding, games) else {
            continue;
        };
        let binding = serde_json::from_value(binding).map_err(|error| {
            AppError::internal(format!("could not decode repository binding: {error}"))
        })?;
        let games = serde_json::from_value(games).map_err(|error| {
            AppError::internal(format!("could not decode repository games: {error}"))
        })?;
        out.push(to_repo_info_with_games(
            binding,
            games,
            pending_pushes,
            push_error,
        ));
    }
    Ok(ArrayResponse::new(out, total))
}

/// `GET /api/admin/repobindings/{id}/scans` — scan history, newest first.
pub async fn repo_binding_scans(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<i32>,
    Query(page): Query<crate::utils::shared::PageParams>,
) -> AppResult<ArrayResponse<RepoBindingScanHistoryModel>> {
    let total = repo_binding_scan::Entity::find()
        .filter(repo_binding_scan::Column::BindingId.eq(id))
        .count(&st.db)
        .await? as i64;
    let rows = repo_binding_scan::Entity::find()
        .filter(repo_binding_scan::Column::BindingId.eq(id))
        .order_by_desc(repo_binding_scan::Column::Id)
        .offset(page.skip)
        .limit(page.count.clamp(1, SCAN_HISTORY_LIMIT))
        .all(&st.db)
        .await?;
    let data = rows
        .into_iter()
        .map(|s| RepoBindingScanHistoryModel {
            id: s.id,
            ran_at_utc: s.ran_at_utc,
            commit_sha: s.commit_sha,
            games_created: s.games_created,
            games_updated: s.games_updated,
            challenges_imported: s.challenges_imported,
            challenges_updated: s.challenges_updated,
            failures: s.failures,
            messages: s.messages,
        })
        .collect();
    Ok(ArrayResponse::new(data, total))
}
