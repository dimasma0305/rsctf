//! Bounded challenge ZIP extraction and durable audit-source packaging.

use std::io::{Cursor, Write};

use super::*;

const MAX_ARCHIVE_ENTRIES: usize = 2_048;
const MAX_ARCHIVE_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_ARCHIVE_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ARCHIVE_COMPRESSION_RATIO: u64 = 200;
const MAX_ARCHIVE_PATH_COMPONENTS: usize = 32;

/// Extract every ZIP entry from `bytes` into `dest`. Zip-slip safe: an entry with
/// a noncanonical path or a path that would escape `dest` rejects the archive.
/// A malformed archive is a client error (400).
pub(super) fn extract_zip(bytes: &[u8], dest: &std::path::Path) -> AppResult<()> {
    extract_zip_with_limits(
        bytes,
        dest,
        ArchiveLimits {
            entries: MAX_ARCHIVE_ENTRIES,
            file_bytes: MAX_ARCHIVE_FILE_BYTES,
            total_bytes: MAX_ARCHIVE_TOTAL_BYTES,
            compression_ratio: MAX_ARCHIVE_COMPRESSION_RATIO,
            path_components: MAX_ARCHIVE_PATH_COMPONENTS,
        },
    )
}

#[derive(Clone, Copy)]
pub(super) struct ArchiveLimits {
    pub(super) entries: usize,
    pub(super) file_bytes: u64,
    pub(super) total_bytes: u64,
    pub(super) compression_ratio: u64,
    pub(super) path_components: usize,
}

pub(super) fn extract_zip_with_limits(
    bytes: &[u8],
    dest: &std::path::Path,
    limits: ArchiveLimits,
) -> AppResult<()> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| AppError::bad_request(format!("Invalid or corrupted ZIP file: {e}")))?;
    if archive.len() > limits.entries {
        return Err(AppError::bad_request("ZIP contains too many entries"));
    }

    let mut total_written = 0u64;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| AppError::bad_request(format!("ZIP read error: {e}")))?;
        let rel = crate::utils::archive::canonical_zip_entry_path(&entry)
            .ok_or_else(|| AppError::bad_request("ZIP entry path is not canonical"))?;
        if rel.components().count() > limits.path_components {
            return Err(AppError::bad_request("ZIP entry path is too deep"));
        }
        if !entry.is_dir() {
            if entry.size() > limits.file_bytes {
                return Err(AppError::bad_request("ZIP entry is too large"));
            }
            let compressed = entry.compressed_size().max(1);
            if entry.size() > compressed.saturating_mul(limits.compression_ratio) {
                return Err(AppError::bad_request(
                    "ZIP entry compression ratio is too high",
                ));
            }
            if total_written.saturating_add(entry.size()) > limits.total_bytes {
                return Err(AppError::bad_request("ZIP expands beyond the size limit"));
            }
        }
        let out = dest.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out)
                .map_err(|e| AppError::internal(format!("create dir {}: {e}", out.display())))?;
        } else {
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    AppError::internal(format!("create dir {}: {e}", parent.display()))
                })?;
            }
            let mut file = std::fs::File::create(&out)
                .map_err(|e| AppError::internal(format!("create file {}: {e}", out.display())))?;
            let remaining_total = limits.total_bytes.saturating_sub(total_written);
            let max_write = limits.file_bytes.min(remaining_total);
            let written = std::io::copy(
                &mut std::io::Read::take(&mut entry, max_write + 1),
                &mut file,
            )
            .map_err(|e| AppError::internal(format!("write file {}: {e}", out.display())))?;
            if written > max_write {
                return Err(AppError::bad_request("ZIP expands beyond the size limit"));
            }
            total_written = total_written.saturating_add(written);
        }
    }
    Ok(())
}

fn zip_dir_to_bytes(dir: &std::path::Path) -> AppResult<Vec<u8>> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let mut files_seen = 0usize;
    let mut total_bytes = 0u64;
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    while let Some((current, depth)) = stack.pop() {
        if depth > MAX_ARCHIVE_PATH_COMPONENTS {
            return Err(AppError::bad_request("challenge archive is too deep"));
        }
        let entries = std::fs::read_dir(&current)
            .map_err(|e| AppError::internal(format!("zip read_dir {}: {e}", current.display())))?;
        for entry in entries {
            let entry =
                entry.map_err(|e| AppError::internal(format!("zip read dir entry: {e}")))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|e| AppError::internal(format!("zip stat {}: {e}", path.display())))?;
            if file_type.is_dir() {
                stack.push((path, depth + 1));
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            files_seen += 1;
            if files_seen > MAX_ARCHIVE_ENTRIES {
                return Err(AppError::bad_request(
                    "challenge archive has too many files",
                ));
            }
            let declared = entry
                .metadata()
                .map_err(|e| AppError::internal(format!("zip stat {}: {e}", path.display())))?
                .len();
            if declared > MAX_ARCHIVE_FILE_BYTES
                || total_bytes.saturating_add(declared) > MAX_ARCHIVE_TOTAL_BYTES
            {
                return Err(AppError::bad_request(
                    "challenge archive exceeds the size limit",
                ));
            }
            let Ok(relative) = path.strip_prefix(dir) else {
                continue;
            };
            let name = relative.to_string_lossy().replace('\\', "/");
            let data = std::fs::read(&path)
                .map_err(|e| AppError::internal(format!("zip read {}: {e}", path.display())))?;
            let actual = data.len() as u64;
            if actual > MAX_ARCHIVE_FILE_BYTES
                || total_bytes.saturating_add(actual) > MAX_ARCHIVE_TOTAL_BYTES
            {
                return Err(AppError::bad_request(
                    "challenge archive exceeds the size limit",
                ));
            }
            total_bytes = total_bytes.saturating_add(actual);
            writer
                .start_file(name, options)
                .map_err(|e| AppError::internal(format!("zip start_file: {e}")))?;
            writer
                .write_all(&data)
                .map_err(|e| AppError::internal(format!("zip write: {e}")))?;
        }
    }
    let cursor = writer
        .finish()
        .map_err(|e| AppError::internal(format!("zip finish: {e}")))?;
    Ok(cursor.into_inner())
}

/// Keep a best-effort audit archive unless an authoritative source archive exists.
pub(super) async fn persist_challenge_archive(
    st: &SharedState,
    challenge_id: i32,
    manifest: &std::path::Path,
) {
    let Some(dir) = manifest.parent() else {
        return;
    };
    let dir_name = dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("challenge");
    let already_has_archive = match sqlx::query_scalar::<_, bool>(
        r#"SELECT original_archive_blob_path IS NOT NULL
             FROM "GameChallenges"
            WHERE id = $1"#,
    )
    .bind(challenge_id)
    .fetch_optional(st.pg())
    .await
    {
        Ok(Some(value)) => value,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(%error, "audit archive: preflight {dir_name} failed");
            return;
        }
    };
    if already_has_archive {
        tracing::debug!(
            challenge_id,
            "audit archive: retained authoritative build/source fingerprint"
        );
        return;
    }
    let bytes = match zip_dir_to_bytes(dir) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!("audit archive: zip {dir_name} failed: {error}");
            return;
        }
    };
    let persisted: AppResult<Option<String>> = async {
        let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        let current_hash = sqlx::query_as::<_, (Option<String>,)>(
            r#"SELECT original_archive_blob_path
                 FROM "GameChallenges"
                WHERE id = $1
                FOR UPDATE"#,
        )
        .bind(challenge_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .ok_or_else(|| AppError::not_found("Challenge not found"))?
        .0;
        if current_hash.is_some() {
            transaction
                .commit()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            return Ok(None);
        }
        let (blob, _) = crate::services::blob_refs::store_and_acquire_in_transaction(
            st.storage.as_ref(),
            &mut transaction,
            &format!("{dir_name}.zip"),
            &bytes,
        )
        .await?;
        sqlx::query(
            r#"UPDATE "GameChallenges"
                  SET original_archive_blob_path = $2
                WHERE id = $1"#,
        )
        .bind(challenge_id)
        .bind(&blob.hash)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        transaction
            .commit()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        Ok(Some(blob.hash))
    }
    .await;
    match persisted {
        Ok(Some(_)) => {}
        Ok(None) => tracing::debug!(
            challenge_id,
            "audit archive: retained authoritative build/source fingerprint"
        ),
        Err(error) => tracing::warn!(%error, "audit archive: persist {dir_name} failed"),
    }
}
