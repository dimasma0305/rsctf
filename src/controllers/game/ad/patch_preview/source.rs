use super::*;
use crate::services::ad::patches::checkout::{BlobRow, Checkout};
use axum::extract::Query;
use std::fs::File;

const BATCH_BYTES: usize = 2 * 1024 * 1024;

#[derive(serde::Deserialize)]
pub(crate) struct Version {
    commit: String,
}

pub async fn download(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game, challenge, revision)): Path<(i32, i32, i32)>,
    Query(version): Query<Version>,
) -> AppResult<Response> {
    let who = caller(&st, &user, game).await?;
    let (mut roster, service, starter) = lock_source(st.pg(), who, challenge).await?;
    let starter = starter.ok_or_else(missing)?;
    // Bound manifest work before reading it. The larger export allocation is
    // weighted below using metadata only, before fetching any source payload.
    let small = crate::utils::upload::reserve_buffered(16 * 1024 * 1024)?;
    let (tree_id, record) = sqlx::query_as::<_,(String,String)>(r#"SELECT t.tree_id,t.manifest
        FROM "AdPatchRevisions" r JOIN "AdPatchSourceTrees" t USING (service_id,tree_id)
        WHERE r.service_id=$1 AND r.revision=$2 AND r.commit_id=$3 AND octet_length(t.manifest)<=4194304"#)
        .bind(service).bind(revision).bind(version.commit).fetch_optional(&mut **roster.transaction_mut()).await
        .map_err(|e| AppError::internal(e.to_string()))?.ok_or_else(|| AppError::not_found("Private revision not found"))?;
    roster.release().await?;
    let (manifest, ids, small) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let manifest = PatchManifest::from_record(&record)?;
        let ids = manifest.blob_ids();
        Ok((manifest, ids, small))
    })
    .await
    .map_err(|e| AppError::internal(e.to_string()))??;
    let sizes = sqlx::query_as::<_, (String, i32)>(
        r#"SELECT oid,octet_length(payload) FROM "AdPatchSourceBlobs"
        WHERE service_id=$1 AND oid=ANY($2) ORDER BY oid"#,
    )
    .bind(service)
    .bind(ids)
    .fetch_all(st.pg())
    .await
    .map_err(|e| AppError::internal(e.to_string()))?;
    let largest = sizes.iter().map(|(_, n)| *n as usize).max().unwrap_or(0);
    let bytes = 16 * 1024 * 1024 + 2 * largest.max(BATCH_BYTES).max(starter.archive_bytes as usize);
    let permit = match st
        .bulk_export_admission
        .try_acquire(Arc::clone(&st.cache), bytes)
        .await
    {
        Ok(permit) => Arc::new(permit),
        Err(_) => return Ok(bulk_export::overload_response()),
    };
    drop(small);
    let original = load(st.pg(), challenge, &starter).await?;
    let original_id = starter.tree_id.clone();
    let work = Arc::clone(&permit);
    let (mut checkout, groups) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let _work = work;
        let private_sizes = sizes.iter().cloned().collect();
        let checkout =
            Checkout::start(manifest, &tree_id, &original, &original_id, &private_sizes)?;
        let groups = blob_batches(&checkout, sizes);
        Ok((checkout, groups))
    })
    .await
    .map_err(|e| AppError::internal(e.to_string()))??;
    for ids in groups {
        let rows = sqlx::query_as::<_, BlobRow>(
            r#"SELECT oid,deflated,raw_size,payload FROM "AdPatchSourceBlobs"
            WHERE service_id=$1 AND oid=ANY($2)"#,
        )
        .bind(service)
        .bind(ids)
        .fetch_all(st.pg())
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
        let work = Arc::clone(&permit);
        checkout = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            let _work = work;
            checkout.append_blobs(rows)?;
            Ok(checkout)
        })
        .await
        .map_err(|e| AppError::internal(e.to_string()))??;
    }
    let work = Arc::clone(&permit);
    let file: File = tokio::task::spawn_blocking(move || {
        let _work = work;
        checkout.finish()
    })
    .await
    .map_err(|e| AppError::internal(e.to_string()))??;
    let (roster, current, current_starter) = lock_source(st.pg(), who, challenge).await?;
    if current != service || current_starter.as_ref() != Some(&starter) {
        return Err(AppError::conflict(
            "The hosted service changed. Reopen the challenge.",
        ));
    }
    roster.release().await?;
    let stream = tokio_util::io::ReaderStream::new(tokio::fs::File::from_std(file));
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_owned()),
            (header::CACHE_CONTROL, "private, no-store".to_owned()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"patch-revision-{revision}.zip\""),
            ),
        ],
        bulk_export::permitted_stream_body(stream, permit),
    )
        .into_response())
}

// One large file or up to 2 MiB of small files per query. Database connections
// are released between reads and while reconstructing or transmitting the ZIP.
fn blob_batches(checkout: &Checkout, sizes: Vec<(String, i32)>) -> Vec<Vec<String>> {
    let mut groups = Vec::new();
    let mut group = Vec::new();
    let mut bytes = 0;
    for (id, size) in sizes {
        if !checkout.needs(&id) {
            continue;
        }
        if !group.is_empty() && bytes + size as usize > BATCH_BYTES {
            groups.push(std::mem::take(&mut group));
            bytes = 0;
        }
        bytes += size as usize;
        group.push(id);
    }
    if !group.is_empty() {
        groups.push(group);
    }
    groups
}
