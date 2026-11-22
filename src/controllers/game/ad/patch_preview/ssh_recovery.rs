use super::*;
use std::collections::BTreeMap;
use std::io::{Seek, Write};
use uuid::Uuid;

type CapturedFile = (String, Option<String>, bool, Option<Vec<u8>>);

async fn check(
    db: &mut sqlx::PgConnection,
    service: i32,
    revision: i64,
    capture: Uuid,
    starter: &Starter,
) -> AppResult<()> {
    sqlx::query_scalar::<_,i32>(r#"SELECT 1 FROM "AdSshRecovery" WHERE service_id=$1 AND revision=$2 AND id=$3 AND starter_tree=$4"#)
        .bind(service).bind(revision).bind(capture).bind(&starter.tree_id).fetch_optional(db).await
        .map_err(|e| AppError::internal(e.to_string()))?.ok_or_else(||AppError::not_found("SSH revision is no longer available"))?;
    Ok(())
}

pub(crate) async fn download(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game, challenge, revision, capture)): Path<(i32, i32, i64, Uuid)>,
) -> AppResult<Response> {
    let who = caller(&st, &user, game).await?;
    let (mut roster, service, starter) = lock_source(st.pg(), who, challenge).await?;
    let starter = starter.ok_or_else(missing)?;
    check(
        roster.transaction_mut(),
        service,
        revision,
        capture,
        &starter,
    )
    .await?;
    roster.release().await?;
    let bytes = 32 * 1024 * 1024 + 2 * starter.archive_bytes as usize;
    let permit = match st
        .bulk_export_admission
        .try_acquire(Arc::clone(&st.cache), bytes)
        .await
    {
        Ok(permit) => Arc::new(permit),
        Err(_) => return Ok(bulk_export::overload_response()),
    };
    let archive = load(st.pg(), challenge, &starter).await?;
    let rows = sqlx::query_as::<_,CapturedFile>(r#"SELECT f.path,f.oid,f.executable,b.payload FROM "AdSshRecoveryFiles" f
        JOIN "AdSshRecovery" r USING(service_id,revision) LEFT JOIN "AdSshRecoveryBlobs" b USING(service_id,oid)
        WHERE f.service_id=$1 AND f.revision=$2 AND r.id=$3 ORDER BY f.path LIMIT 33"#)
        .bind(service).bind(revision).bind(capture).fetch_all(st.pg()).await.map_err(|e|AppError::internal(e.to_string()))?;
    let tree = starter.tree_id.clone();
    let guard = permit.clone();
    let file = tokio::task::spawn_blocking(move || {
        let _guard = guard;
        export(&archive, &tree, rows)
    })
    .await
    .map_err(|e| AppError::internal(e.to_string()))??;
    let (mut roster, current, current_starter) = lock_source(st.pg(), who, challenge).await?;
    if current != service || current_starter.as_ref() != Some(&starter) {
        return Err(missing());
    }
    check(
        roster.transaction_mut(),
        current,
        revision,
        capture,
        &starter,
    )
    .await?;
    roster.release().await?;
    let stream = tokio_util::io::ReaderStream::new(tokio::fs::File::from_std(file));
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_owned()),
            (header::CACHE_CONTROL, "private, no-store".to_owned()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"ssh-revision-{revision}.zip\""),
            ),
        ],
        bulk_export::permitted_stream_body(stream, permit),
    )
        .into_response())
}

fn export(archive: &[u8], tree: &str, rows: Vec<CapturedFile>) -> AppResult<std::fs::File> {
    let manifest = PatchManifest::from_zip(archive).map_err(AppError::internal)?;
    let registered = manifest.ssh_sources(archive).map_err(AppError::internal)?;
    let mut retained = BTreeMap::new();
    for (name, oid, executable, bytes) in rows {
        if oid
            != bytes
                .as_deref()
                .map(crate::services::ad::patches::ssh_sources::content_id)
        {
            return Err(AppError::internal("Invalid SSH revision content"));
        }
        retained.insert(name, (executable, bytes));
    }
    if manifest.tree_id() != tree
        || retained.is_empty()
        || retained.keys().ne(registered
            .iter()
            .filter(|s| s.retain_content)
            .map(|s| &s.source_path))
    {
        return Err(AppError::conflict(
            "SSH revision does not match the registered starter",
        ));
    }
    let file = crate::services::ad::patches::checkout::private_export_file()
        .map_err(anyhow::Error::from)?;
    let mut writer = zip::ZipWriter::new(file);
    crate::services::challenge_builds::visit_build_files(
        archive,
        None,
        |name, mode, _, original| {
            let replacement = retained.remove(name);
            if replacement.is_some() {
                std::io::copy(original, &mut std::io::sink()).map_err(|e| e.to_string())?;
            }
            let (mode, data) = match replacement {
                Some((_, None)) => return Ok(()),
                Some((executable, Some(bytes))) => {
                    (if executable { 0o755 } else { 0o644 }, Some(bytes))
                }
                None => (mode, None),
            };
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .unix_permissions(mode);
            writer
                .start_file(name, options)
                .map_err(|e| e.to_string())?;
            if let Some(data) = data {
                writer.write_all(&data).map_err(|e| e.to_string())?;
            } else {
                std::io::copy(original, &mut writer).map_err(|e| e.to_string())?;
            }
            Ok(())
        },
    )
    .map_err(AppError::internal)?;
    let mut file = writer
        .finish()
        .map_err(|e| AppError::internal(e.to_string()))?;
    file.rewind().map_err(anyhow::Error::from)?;
    Ok(file)
}
