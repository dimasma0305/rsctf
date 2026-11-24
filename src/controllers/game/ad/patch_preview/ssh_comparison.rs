use super::*;
use axum::extract::Query;
use chrono::{DateTime, Utc};

#[derive(serde::Deserialize)]
pub(crate) struct Selection {
    service: i32,
    commit: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Comparison {
    revision: i64,
    #[serde(with = "crate::utils::datetime::millis")]
    last_seen: DateTime<Utc>,
    tracked_files: usize,
    changes: Vec<PatchChange>,
}

pub(crate) async fn get(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game, challenge, revision)): Path<(i32, i32, i32)>,
    Query(selection): Query<Selection>,
) -> AppResult<Response> {
    let who = caller(&st, &user, game).await?;
    let (mut roster, service, starter) = lock_source(st.pg(), who, challenge).await?;
    let starter = starter.ok_or_else(missing)?;
    if selection.service != service {
        return Err(AppError::not_found("Private revision not found"));
    }
    let permit = crate::utils::upload::reserve_buffered(16 * 1024 * 1024)?;
    let db = &mut **roster.transaction_mut();
    // Inspect the newest observation, including unavailable samples. An earlier
    // readable report must never stand in for an unreadable last observation.
    let latest = sqlx::query_as::<_, (i64, DateTime<Utc>, Option<String>)>(
        r#"SELECT revision,last_seen,CASE WHEN state='observed' THEN files END
           FROM "AdHostedSshSourceHistory" WHERE service_id=$1 ORDER BY revision DESC LIMIT 1"#,
    )
    .bind(service)
    .fetch_optional(&mut *db)
    .await
    .map_err(revisions::db_error)?;
    let (tree, manifest) = sqlx::query_as::<_, (String, Option<String>)>(
        r#"SELECT t.tree_id,CASE WHEN $4 THEN t.manifest END FROM "AdPatchRevisions" r
           JOIN "AdPatchSourceTrees" t USING(service_id,tree_id)
           WHERE r.service_id=$1 AND r.revision=$2 AND r.commit_id=$3 AND octet_length(t.manifest)<=4194304"#,
    ).bind(service).bind(revision).bind(selection.commit)
        .bind(latest.as_ref().is_some_and(|(_, _, files)| files.is_some()))
        .fetch_optional(db).await.map_err(revisions::db_error)?
        .ok_or_else(|| AppError::not_found("Private revision not found"))?;
    roster.release().await?;
    let report =
        if let (Some((revision, last_seen, Some(files))), Some(record)) = (latest, manifest) {
            let report = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
                let _permit = permit;
                let observed: crate::services::byoc_tunnel::source::Files =
                    serde_json::from_str(&files)?;
                let manifest = PatchManifest::from_record(&record)?;
                anyhow::ensure!(
                    manifest.tree_id() == tree,
                    "Saved patch manifest does not match its Git tree"
                );
                Ok(Comparison {
                    revision,
                    last_seen,
                    tracked_files: observed.len(),
                    changes: manifest.changes_since_ssh(&observed),
                })
            })
            .await
            .map_err(|e| AppError::internal(e.to_string()))??;
            let (roster, current, current_starter) = lock_source(st.pg(), who, challenge).await?;
            if current != service || current_starter.as_ref() != Some(&starter) {
                return Err(AppError::conflict(
                    "Service or starter changed. Reopen the challenge.",
                ));
            }
            roster.release().await?;
            Some(report)
        } else {
            None
        };
    Ok((
        [(header::CACHE_CONTROL, "private, no-store")],
        Json(serde_json::json!({"report": report})),
    )
        .into_response())
}
