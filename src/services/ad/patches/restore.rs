//! Restore an immutable tree for an authorized HTTP export or an isolated patch worker.
use super::checkout::{BlobRow, Checkout};
use super::PatchManifest;
use sqlx::PgPool;
use std::{fs::File, sync::Arc};

pub(crate) const BATCH_BYTES: usize = 2 * 1024 * 1024;

pub(crate) async fn blob_sizes(
    pool: &PgPool,
    service: i32,
    ids: Vec<String>,
) -> anyhow::Result<Vec<(String, i32)>> {
    let sizes = sqlx::query_as::<_, (String, i32)>(
        r#"SELECT oid,octet_length(payload) FROM "AdPatchSourceBlobs"
        WHERE service_id=$1 AND oid=ANY($2) ORDER BY oid"#,
    )
    .bind(service)
    .bind(ids)
    .fetch_all(pool)
    .await?;
    Ok(sizes)
}

pub(crate) struct Source {
    pub manifest: PatchManifest,
    pub tree_id: String,
    pub original: Vec<u8>,
    pub original_id: String,
}

pub(crate) async fn restore(
    pool: &PgPool,
    service: i32,
    source: Source,
    sizes: Vec<(String, i32)>,
    guard: Arc<dyn Send + Sync>,
) -> anyhow::Result<File> {
    let work = Arc::clone(&guard);
    let (mut checkout, groups) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let _work = work;
        let private_sizes = sizes.iter().cloned().collect();
        let checkout = Checkout::start(
            source.manifest,
            &source.tree_id,
            &source.original,
            &source.original_id,
            &private_sizes,
        )?;
        let groups = blob_batches(&checkout, sizes);
        Ok((checkout, groups))
    })
    .await??;
    for ids in groups {
        let rows = sqlx::query_as::<_, BlobRow>(
            r#"SELECT oid,deflated,raw_size,payload FROM "AdPatchSourceBlobs"
            WHERE service_id=$1 AND oid=ANY($2)"#,
        )
        .bind(service)
        .bind(ids)
        .fetch_all(pool)
        .await?;
        let work = Arc::clone(&guard);
        checkout = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            let _work = work;
            checkout.append_blobs(rows)?;
            Ok(checkout)
        })
        .await??;
    }
    let work = Arc::clone(&guard);
    let file: File = tokio::task::spawn_blocking(move || {
        let _work = work;
        checkout.finish()
    })
    .await??;
    Ok(file)
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
