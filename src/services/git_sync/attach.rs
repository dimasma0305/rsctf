//! git_sync::attachment — package + attach a challenge artifact (`provide:` /
//! `dist/`), split from git_sync/mod.rs to stay under the 1000-line rule.
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use crate::app_state::SharedState;
use crate::utils::enums::FileType;
use crate::utils::error::{AppError, AppResult};

const MAX_ATTACHMENT_FILES: usize = 2_048;
const MAX_ATTACHMENT_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_ATTACHMENT_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ATTACHMENT_DEPTH: usize = 32;

/// Package + attach a challenge's artifact, mirroring RSCTF `SyncAttachmentAsync`
/// (Services/Transfer/ChallengeImportService.cs). The source is the explicit
/// `provide:` path, or — when absent — the TCP1P `dist/` convention (which RSCTF's
/// own code documents). A single file is uploaded as-is; a directory with one
/// file uploads that file; a multi-file directory is zipped. Best-effort: any
/// failure is logged and the challenge simply keeps no attachment (never fails
/// the whole import). Path-escape + symlink guards mirror RSCTF's.
pub(super) async fn sync_attachment(
    st: &SharedState,
    challenge_id: i32,
    package_dir: &Path,
    provide: Option<&str>,
) -> bool {
    let package_dir = package_dir.to_path_buf();
    let provide = provide.map(str::to_owned);
    let packaged =
        tokio::task::spawn_blocking(move || prepare_attachment(&package_dir, provide.as_deref()))
            .await;
    let Some((filename, bytes)) = (match packaged {
        Ok(packaged) => packaged,
        Err(error) => {
            tracing::warn!(%error, "git_sync: attachment packaging task failed");
            return false;
        }
    }) else {
        return false;
    };
    let content_hash = crate::utils::codec::sha256_hex(&bytes);

    // Serialize the physical store, refcount, attachment, and challenge link by
    // content hash. Deletion uses this same transaction-scoped lock, so it
    // cannot remove a just-stored object before this metadata commits.
    let persisted: anyhow::Result<()> = async {
        let mut tx = crate::utils::database::begin_sqlx_transaction(st.pg()).await?;
        let (_, local_file_id) = crate::services::blob_refs::store_and_acquire_in_transaction(
            st.storage.as_ref(),
            &mut tx,
            &filename,
            &bytes,
        )
        .await?;
        let attachment_id = sqlx::query_scalar::<_, i32>(
            r#"INSERT INTO "Attachments" ("Type", remote_url, local_file_id)
               VALUES ($1, NULL, $2)
               RETURNING id"#,
        )
        .bind(FileType::Local as i16)
        .bind(local_file_id)
        .fetch_one(&mut *tx)
        .await?;
        let linked = sqlx::query(
            r#"UPDATE "GameChallenges"
                  SET attachment_id = $2
                WHERE id = $1 AND attachment_id IS NULL"#,
        )
        .bind(challenge_id)
        .bind(attachment_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if linked != 1 {
            return Err(sqlx::Error::RowNotFound.into());
        }
        tx.commit().await?;
        Ok(())
    }
    .await;
    if let Err(error) = persisted {
        tracing::warn!(%error, "git_sync: attachment persistence failed");
        if let Err(cleanup_error) = crate::services::blob_refs::purge_if_unreferenced(
            st.pg(),
            st.storage.as_ref(),
            &content_hash,
        )
        .await
        {
            tracing::warn!(%cleanup_error, %content_hash, "git_sync: unpublished attachment purge failed");
        }
        return false;
    }
    tracing::info!(challenge_id, attachment = %filename, "git_sync: imported attachment");
    true
}

fn prepare_attachment(package_dir: &Path, provide: Option<&str>) -> Option<(String, Vec<u8>)> {
    let rel = match provide.map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => p.replace('\\', "/").trim_start_matches('/').to_string(),
        None => {
            // No explicit provide → fall back to the `dist/` convention if present.
            if package_dir.join("dist").is_dir() {
                "dist".to_string()
            } else {
                return None;
            }
        }
    };
    // Reject traversal / absolute paths (RSCTF `provide` escape guard).
    if rel.contains("..") || Path::new(&rel).is_absolute() {
        tracing::warn!(rel, "git_sync: rejecting unsafe 'provide' path");
        return None;
    }
    let Some(absolute) = resolve_attachment_path(package_dir, &rel) else {
        tracing::warn!(rel, "git_sync: rejecting attachment path outside package");
        return None;
    };
    package_attachment(&absolute)
}

/// Backfill artifacts for challenges imported before attachment packaging was
/// available. Only manifests below the managed repository checkout are read.
pub async fn repair_missing_attachments(st: &SharedState) -> AppResult<u64> {
    let cleaned = reconcile_attachment_references(st).await?;
    if cleaned > 0 {
        tracing::info!(cleaned, "git_sync: removed orphan attachment records");
    }
    let repos_root = PathBuf::from(&st.config.storage_root).join("repos");
    let Ok(repos_root) = tokio::fs::canonicalize(repos_root).await else {
        return Ok(0);
    };
    let mut repaired = 0u64;
    let mut after_id = 0i32;
    loop {
        let challenges = sqlx::query_as::<_, (i32, String)>(
            r#"SELECT id, source_yaml_path
                 FROM "GameChallenges"
                WHERE attachment_id IS NULL
                  AND source_yaml_path IS NOT NULL
                  AND id > $1
                ORDER BY id
                LIMIT 100"#,
        )
        .bind(after_id)
        .fetch_all(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        let Some((last_id, _)) = challenges.last() else {
            break;
        };
        after_id = *last_id;
        for (challenge_id, source) in challenges {
            // Resolve only far enough to identify the managed checkout, then
            // take the same cross-replica lock used by scans/push-back and
            // resolve again under that guard. This prevents startup repair from
            // packaging a tree while another role is replacing its files.
            let Ok(preliminary) = tokio::fs::canonicalize(&source).await else {
                continue;
            };
            let Some(std::path::Component::Normal(repo_dir)) = preliminary
                .strip_prefix(&repos_root)
                .ok()
                .and_then(|relative| relative.components().next())
            else {
                continue;
            };
            let checkout = repos_root.join(repo_dir);
            let _checkout_lock = super::lock_checkout_distributed(st.pg(), &checkout).await?;
            let Ok(locked_checkout) = tokio::fs::canonicalize(&checkout).await else {
                continue;
            };
            if !locked_checkout.starts_with(&repos_root) {
                continue;
            }
            let Ok(manifest) = tokio::fs::canonicalize(&source).await else {
                continue;
            };
            let is_manifest = manifest
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| matches!(name, "challenge.yml" | "challenge.yaml"));
            if !is_manifest || !manifest.starts_with(&locked_checkout) {
                continue;
            }
            let provide = tokio::fs::read_to_string(&manifest)
                .await
                .ok()
                .and_then(|raw| serde_norway::from_str::<super::ChallengeYaml>(&raw).ok())
                .and_then(|model| model.provide);
            let package_dir = manifest.parent().unwrap_or(locked_checkout.as_path());
            if sync_attachment(st, challenge_id, package_dir, provide.as_deref()).await {
                repaired += 1;
            }
        }
    }
    Ok(repaired)
}

/// Repair legacy attachment/refcount drift before creating new links. A Files
/// row with no relational target may still be a deliberate standalone
/// `/api/assets` ownership reference, so reconciliation never guesses that it
/// is safe to remove metadata or physical content.
async fn reconcile_attachment_references(st: &SharedState) -> AppResult<u64> {
    let removed_attachments =
        crate::services::blob_refs::delete_orphan_attachments(st.pg(), st.storage.as_ref()).await?;
    let mut tx = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"UPDATE "Files" file
              SET reference_count = GREATEST(file.reference_count, refs.reference_count)
             FROM (
                   SELECT file_id, SUM(reference_count)::bigint AS reference_count
                     FROM (
                           SELECT local_file_id AS file_id,
                                  COUNT(*)::bigint AS reference_count
                             FROM "Attachments"
                            WHERE local_file_id IS NOT NULL
                            GROUP BY local_file_id
                           UNION ALL
                           SELECT writeup_id AS file_id,
                                  COUNT(*)::bigint AS reference_count
                             FROM "Participations"
                            WHERE writeup_id IS NOT NULL
                            GROUP BY writeup_id
                           UNION ALL
                           SELECT file.id, COUNT(*)::bigint
                             FROM "Files" file
                             JOIN "AspNetUsers" owner ON owner.avatar_hash = file.hash
                            GROUP BY file.id
                           UNION ALL
                           SELECT file.id, COUNT(*)::bigint
                             FROM "Files" file
                             JOIN "Teams" owner ON owner.avatar_hash = file.hash
                            GROUP BY file.id
                           UNION ALL
                           SELECT file.id, COUNT(*)::bigint
                             FROM "Files" file
                             JOIN "Games" owner ON owner.poster_hash = file.hash
                            GROUP BY file.id
                           UNION ALL
                           SELECT file.id, COUNT(*)::bigint
                             FROM "Files" file
                             JOIN "GameChallenges" owner
                               ON owner.original_archive_blob_path = file.hash
                            GROUP BY file.id
                           UNION ALL
                           SELECT file.id, 1::bigint
                             FROM "Files" file
                            WHERE EXISTS (
                                  SELECT 1 FROM "Configs" config
                                   WHERE config.config_key IN (
                                         'GlobalConfig:LogoHash',
                                         'GlobalConfig:FaviconHash'
                                   )
                                     AND config.value = file.hash
                            )
                     ) live_references
                    GROUP BY file_id
             ) refs
            WHERE file.id = refs.file_id"#,
    )
    .execute(&mut *tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    tx.commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(removed_attachments)
}

/// Resolve a repository-authored attachment path after following symlinks, then
/// require the result to remain below the canonical package root. Checking the
/// final path text alone is insufficient because an intermediate component can
/// be a Git symlink such as `root -> /`.
fn resolve_attachment_path(package_dir: &Path, rel: &str) -> Option<PathBuf> {
    let root = std::fs::canonicalize(package_dir).ok()?;
    let candidate = std::fs::canonicalize(package_dir.join(rel)).ok()?;
    candidate.starts_with(&root).then_some(candidate)
}

/// Read/package an attachment source into `(filename, bytes)`: a file → itself; a
/// single-file directory → that file; a multi-file directory → a zip. Symlinks are
/// skipped (never followed out of the package). `None` on any I/O error or empty
/// directory.
fn package_attachment(absolute: &Path) -> Option<(String, Vec<u8>)> {
    let meta = std::fs::symlink_metadata(absolute).ok()?;
    if meta.file_type().is_symlink() {
        return None;
    }
    if meta.is_file() {
        if meta.len() > MAX_ATTACHMENT_FILE_BYTES {
            return None;
        }
        let bytes = std::fs::read(absolute).ok()?;
        if bytes.len() as u64 > MAX_ATTACHMENT_FILE_BYTES {
            return None;
        }
        return Some((absolute.file_name()?.to_str()?.to_string(), bytes));
    }
    if meta.is_dir() {
        let mut files: Vec<PathBuf> = Vec::new();
        let mut total = 0u64;
        collect_attachment_files(absolute, &mut files, &mut total, 0)?;
        files.sort();
        if files.is_empty() {
            return None;
        }
        if files.len() == 1 {
            let bytes = std::fs::read(&files[0]).ok()?;
            if bytes.len() as u64 > MAX_ATTACHMENT_FILE_BYTES {
                return None;
            }
            return Some((files[0].file_name()?.to_str()?.to_string(), bytes));
        }
        let mut zw = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        let mut actual_total = 0u64;
        for f in &files {
            let rel = f.strip_prefix(absolute).ok()?.to_str()?;
            let bytes = std::fs::read(f).ok()?;
            let actual_len = bytes.len() as u64;
            if actual_len > MAX_ATTACHMENT_FILE_BYTES
                || actual_total.saturating_add(actual_len) > MAX_ATTACHMENT_TOTAL_BYTES
            {
                return None;
            }
            actual_total = actual_total.saturating_add(actual_len);
            zw.start_file(rel, opts).ok()?;
            zw.write_all(&bytes).ok()?;
        }
        let cursor = zw.finish().ok()?;
        return Some((
            format!("{}.zip", absolute.file_name()?.to_str()?),
            cursor.into_inner(),
        ));
    }
    None
}

/// Recursively collect regular files under `dir`, skipping symlinks (so a
/// dir-symlink can't tar an arbitrary host tree into a downloadable attachment).
fn collect_attachment_files(
    dir: &Path,
    out: &mut Vec<PathBuf>,
    total: &mut u64,
    depth: usize,
) -> Option<()> {
    if depth > MAX_ATTACHMENT_DEPTH {
        return None;
    }
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries {
        let entry = entry.ok()?;
        let ft = entry.file_type().ok()?;
        if ft.is_symlink() {
            continue;
        }
        let path = entry.path();
        if ft.is_dir() {
            collect_attachment_files(&path, out, total, depth + 1)?;
        } else if ft.is_file() {
            let len = entry.metadata().ok()?.len();
            if len > MAX_ATTACHMENT_FILE_BYTES
                || total.saturating_add(len) > MAX_ATTACHMENT_TOTAL_BYTES
                || out.len() >= MAX_ATTACHMENT_FILES
            {
                return None;
            }
            *total = total.saturating_add(len);
            out.push(path);
        }
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("rsctf-{tag}-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn attachment_resolution_accepts_only_canonical_descendants() {
        let root = temp_dir("attach-root");
        std::fs::create_dir_all(root.join("inside")).unwrap();
        std::fs::write(root.join("inside/file"), b"ok").unwrap();
        assert_eq!(
            resolve_attachment_path(&root, "inside/file"),
            Some(std::fs::canonicalize(root.join("inside/file")).unwrap())
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn attachment_resolution_rejects_intermediate_symlink_escape() {
        use std::os::unix::fs::symlink;

        let root = temp_dir("attach-link-root");
        let outside = temp_dir("attach-link-outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret"), b"secret").unwrap();
        symlink(&outside, root.join("link")).unwrap();

        assert!(resolve_attachment_path(&root, "link/secret").is_none());
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn tcp1p_dist_directory_is_packaged_as_zip() {
        let root = temp_dir("tcp1p-dist");
        let dist = root.join("dist");
        std::fs::create_dir_all(&dist).unwrap();
        std::fs::write(dist.join("Dockerfile"), b"FROM python:3.12").unwrap();
        std::fs::write(dist.join("app.py"), b"print('throne')").unwrap();
        std::fs::write(dist.join("requirements.txt"), b"flask\n").unwrap();

        let (name, bytes) = prepare_attachment(&root, None).expect("implicit dist should package");
        assert_eq!(name, "dist.zip");
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut names: Vec<String> = (0..archive.len())
            .map(|index| archive.by_index(index).unwrap().name().to_string())
            .collect();
        names.sort();
        assert_eq!(names, ["Dockerfile", "app.py", "requirements.txt"]);
        assert!(prepare_attachment(&root, Some("./dist")).is_some());

        let no_dist = temp_dir("tcp1p-no-dist");
        std::fs::create_dir_all(&no_dist).unwrap();
        assert!(prepare_attachment(&no_dist, None).is_none());

        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(no_dist);
    }
}
