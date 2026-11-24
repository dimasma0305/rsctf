//! Private, durable source history. Saving a revision never changes a running service.
use super::*;
use crate::services::ad::patches::revisions::{commit_id, SourceTree};
use crate::utils::single_flight::try_acquire_transaction_advisory_lock;
use axum::extract::Query;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use sqlx::{PgConnection, Postgres, QueryBuilder};

#[derive(Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
struct Revision {
    revision: i32,
    commit_id: String,
    parent_id: String,
    tree_id: String,
    #[serde(with = "crate::utils::datetime::millis")]
    created_at: DateTime<Utc>,
}

#[derive(Deserialize)]
pub(crate) struct Page {
    before: Option<i32>,
}

pub(super) fn db_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

pub async fn list(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game, challenge)): Path<(i32, i32)>,
    Query(page): Query<Page>,
) -> AppResult<Response> {
    let who = caller(&st, &user, game).await?;
    let (mut roster, service, starter) = lock_source(st.pg(), who, challenge).await?;
    starter.ok_or_else(missing)?;
    let rows = sqlx::query_as::<_, Revision>(r#"SELECT revision, commit_id, parent_id, tree_id, created_at
        FROM "AdPatchRevisions" WHERE service_id=$1 AND revision < $2 ORDER BY revision DESC LIMIT 50"#)
        .bind(service).bind(page.before.unwrap_or(i32::MAX))
        .fetch_all(&mut **roster.transaction_mut()).await.map_err(db_error)?;
    roster.release().await?;
    Ok(([(header::CACHE_CONTROL, "private, no-store")], Json(rows)).into_response())
}

pub async fn save(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game, challenge)): Path<(i32, i32)>,
    request: Request,
) -> AppResult<Response> {
    let who = caller(&st, &user, game).await?;
    let (roster, initial_service, starter) = lock_source(st.pg(), who, challenge).await?;
    let starter = starter.ok_or_else(missing)?;
    roster.release().await?;
    let expected_service = request
        .headers()
        .get("x-rsctf-expected-service")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i32>().ok())
        .ok_or_else(|| AppError::bad_request("Send the hosted team service ID"))?;
    if expected_service != initial_service {
        return Err(AppError::conflict(
            "The hosted service changed. Reopen the challenge.",
        ));
    }
    let expected = request
        .headers()
        .get("x-rsctf-expected-revision")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i32>().ok())
        .filter(|r| (0..=4096).contains(r))
        .ok_or_else(|| {
            AppError::bad_request("Send the last seen revision number, or 0 for the first revision")
        })?;
    let (candidate, permit) = read_patch_zip(request, starter.archive_bytes as usize).await?;
    let (original, sources) = sqlx::query_as::<_, (Vec<u8>,sqlx::types::Json<Vec<crate::services::ad::patches::ssh_sources::SshSource>>)>(
        r#"SELECT archive,ssh_sources FROM "AdPatchStarters" WHERE challenge_id=$1 AND tree_id=$2 AND archive_bytes=$3"#,
    ).bind(challenge).bind(&starter.tree_id).bind(starter.archive_bytes)
        .fetch_optional(st.pg()).await.map_err(db_error)?.ok_or_else(missing)?;
    let (tree, _permit) = tokio::task::spawn_blocking(move || {
        SourceTree::prepare(&original, candidate, &sources).map(|tree| (tree, permit))
    })
    .await
    .map_err(|e| AppError::internal(e.to_string()))?
    .map_err(|e| AppError::bad_request(e.to_string()))?;
    let (mut roster, service, current_starter) = lock_source(st.pg(), who, challenge).await?;
    if service != initial_service || current_starter.as_ref() != Some(&starter) {
        return Err(AppError::conflict(
            "Service or starter changed. Reopen the challenge.",
        ));
    }
    let tx = roster.transaction_mut();
    if !try_acquire_transaction_advisory_lock(tx, &format!("ad-patch-history:{service}")).await? {
        return Err(AppError::overloaded(
            "Another teammate is saving a revision. Retry shortly.",
            1,
        ));
    }
    let revision = save_on(tx, who, service, &starter.tree_id, expected, tree).await?;
    roster.release().await?;
    Ok((
        [(header::CACHE_CONTROL, "private, no-store")],
        Json(revision),
    )
        .into_response())
}

pub async fn changes(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game, challenge, revision)): Path<(i32, i32, i32)>,
) -> AppResult<Response> {
    let who = caller(&st, &user, game).await?;
    let (mut roster, service, starter) = lock_source(st.pg(), who, challenge).await?;
    let starter = starter.ok_or_else(missing)?;
    let original_bytes = if revision == 1 {
        starter.archive_bytes as usize
    } else {
        0
    };
    let permit = crate::utils::upload::reserve_buffered(16 * 1024 * 1024 + 3 * original_bytes)?;
    let (current, previous) = sqlx::query_as::<_, (String, Option<String>)>(
        r#"SELECT t.manifest, pt.manifest FROM "AdPatchRevisions" r
        JOIN "AdPatchSourceTrees" t USING (service_id,tree_id)
        LEFT JOIN "AdPatchRevisions" p ON p.service_id=r.service_id AND p.revision=r.revision-1
        LEFT JOIN "AdPatchSourceTrees" pt ON pt.service_id=p.service_id AND pt.tree_id=p.tree_id
        WHERE r.service_id=$1 AND r.revision=$2"#,
    )
    .bind(service)
    .bind(revision)
    .fetch_optional(&mut **roster.transaction_mut())
    .await
    .map_err(db_error)?
    .ok_or_else(|| AppError::not_found("Private revision not found"))?;
    roster.release().await?;
    let original = if revision == 1 {
        Some(load(st.pg(), challenge, &starter).await?)
    } else {
        None
    };
    let changes = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let _permit = permit;
        let baseline = match (previous, original) {
            (Some(prev), None) => PatchManifest::from_record(&prev)?,
            (None, Some(zip)) => PatchManifest::from_zip(&zip).map_err(anyhow::Error::msg)?,
            _ => anyhow::bail!("missing private revision parent"),
        };
        Ok(PatchManifest::from_record(&current)?.changes_from(&baseline))
    })
    .await
    .map_err(|e| AppError::internal(e.to_string()))??;
    let (roster, current_service, current_starter) = lock_source(st.pg(), who, challenge).await?;
    if service != current_service || Some(&starter) != current_starter.as_ref() {
        return Err(AppError::conflict(
            "Service or starter changed. Reopen the challenge.",
        ));
    }
    roster.release().await?;
    Ok((
        [(header::CACHE_CONTROL, "private, no-store")],
        Json(changes),
    )
        .into_response())
}

async fn save_on(
    db: &mut PgConnection,
    who: LiveParticipationIdentity<'_>,
    service: i32,
    starter_tree: &str,
    expected: i32,
    tree: SourceTree,
) -> AppResult<Revision> {
    // A retry with the same parent and content returns the original receipt,
    // even if a second teammate has since appended another revision.
    let existing = sqlx::query_as::<_, Revision>(
        r#"SELECT revision, commit_id, parent_id, tree_id, created_at
        FROM "AdPatchRevisions" WHERE service_id=$1 AND revision=$2"#,
    )
    .bind(service)
    .bind(expected + 1)
    .fetch_optional(&mut *db)
    .await
    .map_err(db_error)?;
    if let Some(existing) = existing {
        if existing.tree_id == tree.oid {
            return Ok(existing);
        }
        return Err(AppError::conflict(
            "A teammate saved another revision. Refresh the history.",
        ));
    }
    let head = sqlx::query_as::<_, Revision>(
        r#"SELECT revision, commit_id, parent_id, tree_id, created_at
        FROM "AdPatchRevisions" WHERE service_id=$1 ORDER BY revision DESC LIMIT 1"#,
    )
    .bind(service)
    .fetch_optional(&mut *db)
    .await
    .map_err(db_error)?;
    if head.as_ref().map_or(0, |r| r.revision) != expected {
        return Err(AppError::conflict("Revision changed. Refresh the history."));
    }
    if let Some(ref head) = head {
        if head.tree_id == tree.oid {
            return Err(AppError::conflict(
                "These files already match the latest revision.",
            ));
        }
    } else if tree.oid == starter_tree {
        return Err(AppError::conflict("These files already match the starter."));
    }
    if expected >= 4096 {
        return Err(AppError::conflict(
            "The private source history contains 4096 revisions.",
        ));
    }
    let parent_id = head
        .map(|r| r.commit_id)
        .unwrap_or_else(|| commit_id(starter_tree, None, None, 0));
    store_tree(db, service, &tree).await?;
    let now = Utc::now();
    let commit = commit_id(
        &tree.oid,
        Some(&parent_id),
        Some(who.user_id),
        now.timestamp(),
    );
    let saved = sqlx::query_as::<_, Revision>(r#"INSERT INTO "AdPatchRevisions" (service_id,revision,tree_id,parent_id,commit_id,actor_id,created_at)
        SELECT $1,$2,$3,$4,$5,$6,$7 FROM "Games" WHERE id=$8 AND start_time_utc <= clock_timestamp() AND end_time_utc > clock_timestamp()
        RETURNING revision,commit_id,parent_id,tree_id,created_at"#)
        .bind(service).bind(expected + 1).bind(tree.oid).bind(parent_id).bind(commit).bind(who.user_id).bind(now).bind(who.game_id)
        .fetch_optional(&mut *db).await.map_err(db_error)?.ok_or_else(|| AppError::conflict("The game has ended."))?;
    if !tree.shared_changes.is_empty() {
        sqlx::query(
            r#"INSERT INTO "AdSharedPatchChanges"(service_id,revision,changes)
             SELECT $1,$2,$3 FROM "AdPatchActivityPolicies" WHERE game_id=$4 AND enabled"#,
        )
        .bind(service)
        .bind(saved.revision)
        .bind(sqlx::types::Json(tree.shared_changes))
        .bind(who.game_id)
        .execute(db)
        .await
        .map_err(db_error)?;
    }
    Ok(saved)
}

async fn store_tree(db: &mut PgConnection, service: i32, tree: &SourceTree) -> AppResult<()> {
    let new_tree = sqlx::query(
        r#"INSERT INTO "AdPatchSourceTrees" (service_id,tree_id,manifest)
        VALUES ($1,$2,$3) ON CONFLICT (service_id,tree_id) DO NOTHING"#,
    )
    .bind(service)
    .bind(&tree.oid)
    .bind(&tree.manifest)
    .execute(&mut *db)
    .await
    .map_err(db_error)?
    .rows_affected()
        > 0;
    if !new_tree {
        return Ok(());
    }
    let ids: Vec<_> = tree.blobs.iter().map(|blob| &blob.oid[..]).collect();
    let saved: std::collections::BTreeMap<String,i32> = sqlx::query_as(
        r#"SELECT oid,octet_length(payload) FROM "AdPatchSourceBlobs" WHERE service_id=$1 AND oid=ANY($2)"#,
    )
    .bind(service)
    .bind(&ids)
    .fetch_all(&mut *db)
    .await
    .map_err(db_error)?
    .into_iter()
    .collect();
    let missing = tree.storage_plan(&saved, crate::utils::upload::SOURCE_ARCHIVE_BLOB_BYTES);
    // Charge only new bytes. The fixed allowance covers the keys and row
    // metadata of tiny files. This counter rolls back with the objects.
    let charge = tree.manifest.len() as i64
        + 192
        + missing
            .iter()
            .map(|blob| {
                blob.data.len() as i64 + saved.get(&blob.oid).map_or(192, |old| -i64::from(*old))
            })
            .sum::<i64>();
    sqlx::query(r#"INSERT INTO "AdPatchSourceUsage" (service_id,used_bytes) VALUES ($1,0) ON CONFLICT (service_id) DO NOTHING"#)
        .bind(service).execute(&mut *db).await.map_err(db_error)?;
    let admitted = sqlx::query(
        r#"UPDATE "AdPatchSourceUsage" SET used_bytes=used_bytes+$2 WHERE service_id=$1 AND used_bytes+$2<=536870912"#,
    )
    .bind(service)
    .bind(charge)
    .execute(&mut *db)
    .await
    .map_err(db_error)?;
    if admitted.rows_affected() == 0 {
        return Err(AppError::conflict(
            "This service has reached its 512 MiB private source history budget.",
        ));
    }
    for chunk in missing.chunks(100) {
        let mut query = QueryBuilder::<Postgres>::new(
            r#"INSERT INTO "AdPatchSourceBlobs" (service_id, oid, deflated, raw_size, payload) "#,
        );
        query
            .push_values(chunk, |mut row, blob| {
                row.push_bind(service)
                    .push_bind(&blob.oid)
                    .push_bind(blob.deflated)
                    .push_bind(blob.raw_size)
                    .push_bind(&blob.data[..]);
            })
            .push(" ON CONFLICT (service_id,oid) DO UPDATE SET deflated=EXCLUDED.deflated, payload=EXCLUDED.payload");
        query.build().execute(&mut *db).await.map_err(db_error)?;
    }
    Ok(())
}
