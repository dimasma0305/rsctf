//! Manifest package helpers: enum/category parsing, local image intent, and
//! bounded ZIP creation for durable import/build source archives.

use std::ffi::OsStr;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use sea_orm::Iterable;

use super::{MAX_REPO_DEPTH, MAX_REPO_FILES, MAX_REPO_FILE_BYTES, MAX_REPO_TOTAL_BYTES};
use crate::utils::enums::ChallengeCategory;
use crate::utils::error::{AppError, AppResult};

type ContextFile = (String, u32, Vec<u8>);

fn canonical_regular_mode(mode: Option<u32>) -> u32 {
    if mode.unwrap_or_default() & 0o111 != 0 {
        0o755
    } else {
        0o644
    }
}

/// Case-insensitively resolve a string to a `sea-orm` DB enum variant, mirroring
/// C# `Enum.TryParse<T>(raw, ignoreCase: true)`.
pub(super) fn parse_enum<T>(raw: &str) -> Option<T>
where
    T: Iterable + std::fmt::Debug,
{
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    T::iter().find(|value| format!("{value:?}").eq_ignore_ascii_case(raw))
}

/// Resolve an explicit category or infer it from up to three enclosing package
/// directories, matching the gzcli/TCP1P convention.
pub(super) fn resolve_category(raw: Option<&str>, package_dir: &Path) -> ChallengeCategory {
    if let Some(category) = raw.and_then(parse_enum::<ChallengeCategory>) {
        return category;
    }
    let mut current = package_dir.parent();
    for _ in 0..3 {
        let Some(dir) = current else { break };
        if let Some(category) = dir.file_name().and_then(OsStr::to_str).and_then(parse_enum) {
            return category;
        }
        current = dir.parent();
    }
    ChallengeCategory::Misc
}

/// Locate a conventional local Docker build context (`src/Dockerfile`, then a
/// package-root `Dockerfile`).
pub(super) fn find_dockerfile_context(dir: &Path) -> Option<PathBuf> {
    let src = dir.join("src");
    if src.join("Dockerfile").is_file() {
        return Some(src);
    }
    dir.join("Dockerfile").is_file().then(|| dir.to_path_buf())
}

pub(super) fn image_tag(game_id: i32, name: &str) -> String {
    format!("rsctf/{game_id}/{}:latest", slugify(name))
}

/// Lowercase a challenge name into a registry/path-safe stable slug.
pub(super) fn slugify(name: &str) -> String {
    let mut output = String::with_capacity(name.len());
    let mut previous_dash = false;
    for character in name.trim().chars() {
        if character.is_ascii_alphanumeric() {
            output.push(character.to_ascii_lowercase());
            previous_dash = false;
        } else if !previous_dash {
            output.push('-');
            previous_dash = true;
        }
    }
    let slug = output.trim_matches('-').to_string();
    if slug.is_empty() {
        "challenge".to_string()
    } else {
        slug
    }
}

/// Package every regular file under `dir` into a bounded ZIP with paths relative
/// to `dir`. Symlinks are skipped so a repository cannot archive host files.
pub(super) async fn zip_context_dir(dir: &Path) -> AppResult<Vec<u8>> {
    let mut files: Vec<ContextFile> = Vec::new();
    let mut total_bytes = 0u64;
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    while let Some((current, depth)) = stack.pop() {
        if depth > MAX_REPO_DEPTH {
            return Err(AppError::bad_request("build context is too deep"));
        }
        let mut entries = tokio::fs::read_dir(&current).await.map_err(|error| {
            AppError::internal(format!("git_sync: read_dir {}: {error}", current.display()))
        })?;
        while let Some(entry) = entries.next_entry().await.map_err(|error| {
            AppError::internal(format!(
                "git_sync: read dir entry in {}: {error}",
                current.display()
            ))
        })? {
            let path = entry.path();
            let file_type = entry.file_type().await.map_err(|error| {
                AppError::internal(format!("git_sync: stat {}: {error}", path.display()))
            })?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                stack.push((path, depth + 1));
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            if files.len() >= MAX_REPO_FILES {
                return Err(AppError::bad_request("build context has too many files"));
            }
            let metadata = entry.metadata().await.map_err(|error| {
                AppError::internal(format!("git_sync: stat {}: {error}", path.display()))
            })?;
            let declared = metadata.len();
            if declared > MAX_REPO_FILE_BYTES
                || total_bytes.saturating_add(declared) > MAX_REPO_TOTAL_BYTES
            {
                return Err(AppError::bad_request(
                    "build context exceeds the size limit",
                ));
            }
            let relative = path
                .strip_prefix(dir)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let data = tokio::fs::read(&path).await.map_err(|error| {
                AppError::internal(format!("git_sync: read {}: {error}", path.display()))
            })?;
            let actual = data.len() as u64;
            if actual > MAX_REPO_FILE_BYTES
                || total_bytes.saturating_add(actual) > MAX_REPO_TOTAL_BYTES
            {
                return Err(AppError::bad_request(
                    "build context exceeds the size limit",
                ));
            }
            total_bytes = total_bytes.saturating_add(actual);
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt;
                canonical_regular_mode(Some(metadata.permissions().mode()))
            };
            #[cfg(not(unix))]
            let mode = canonical_regular_mode(None);
            files.push((relative, mode, data));
        }
    }

    files.sort_by(|left, right| left.0.cmp(&right.0));
    encode_context_files(files)
}

fn encode_context_files(files: Vec<ContextFile>) -> AppResult<Vec<u8>> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::<u8>::new()));
    for (name, mode, data) in files {
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(mode);
        writer
            .start_file(name.clone(), options)
            .map_err(|error| AppError::internal(format!("git_sync: zip write {name}: {error}")))?;
        writer
            .write_all(&data)
            .map_err(|error| AppError::internal(format!("git_sync: zip write {name}: {error}")))?;
    }
    writer
        .finish()
        .map(|cursor| cursor.into_inner())
        .map_err(|error| AppError::internal(format!("git_sync: zip finish: {error}")))
}

/// Stable fingerprint of the exact bytes a local Docker build can consume.
pub(super) async fn context_fingerprint(dir: &Path) -> AppResult<String> {
    zip_context_dir(dir)
        .await
        .map(|bytes| crate::utils::codec::sha256_hex(&bytes))
}

/// Fingerprint one build-context subtree from a previously retained full
/// package archive. Entry names are normalized relative to the selected root,
/// then re-encoded in deterministic order to match [`context_fingerprint`].
pub(super) async fn archived_context_fingerprint(
    archive: Vec<u8>,
    subdir: &str,
) -> AppResult<String> {
    let subdir = subdir.trim_matches('/').to_string();
    tokio::task::spawn_blocking(move || {
        let mut zip = zip::ZipArchive::new(Cursor::new(archive)).map_err(|error| {
            AppError::internal(format!("git_sync: open source archive: {error}"))
        })?;
        let prefix = (subdir != "." && !subdir.is_empty()).then(|| format!("{subdir}/"));
        let mut files = Vec::new();
        let mut total = 0u64;
        for index in 0..zip.len() {
            let mut entry = zip.by_index(index).map_err(|error| {
                AppError::internal(format!("git_sync: read source archive entry: {error}"))
            })?;
            if !entry.is_file() {
                continue;
            }
            let name = match prefix.as_deref() {
                Some(prefix) => match entry.name().strip_prefix(prefix) {
                    Some(name) if !name.is_empty() => name.to_string(),
                    _ => continue,
                },
                None => entry.name().to_string(),
            };
            if files.len() >= MAX_REPO_FILES || entry.size() > MAX_REPO_FILE_BYTES {
                return Err(AppError::bad_request(
                    "source archive build context exceeds limits",
                ));
            }
            total = total.saturating_add(entry.size());
            if total > MAX_REPO_TOTAL_BYTES {
                return Err(AppError::bad_request(
                    "source archive build context exceeds limits",
                ));
            }
            let mut bytes = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut bytes).map_err(|error| {
                AppError::internal(format!("git_sync: extract source archive entry: {error}"))
            })?;
            let mode = canonical_regular_mode(entry.unix_mode());
            files.push((name, mode, bytes));
        }
        files.sort_by(|left, right| left.0.cmp(&right.0));
        encode_context_files(files).map(|bytes| crate::utils::codec::sha256_hex(&bytes))
    })
    .await
    .map_err(|error| AppError::internal(format!("git_sync: source fingerprint task: {error}")))?
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[tokio::test]
    async fn context_fingerprint_includes_the_executable_bit() {
        let root = std::env::temp_dir().join(format!(
            "rsctf-generator-mode-{}",
            uuid::Uuid::new_v4().simple()
        ));
        tokio::fs::create_dir_all(&root).await.unwrap();
        tokio::fs::write(root.join("Dockerfile"), b"FROM scratch\n")
            .await
            .unwrap();
        let executable = root.join("generate");
        tokio::fs::write(&executable, b"#!/bin/sh\n").await.unwrap();
        tokio::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))
            .await
            .unwrap();

        let executable_fingerprint = context_fingerprint(&root).await.unwrap();
        let archive = zip_context_dir(&root).await.unwrap();
        assert_eq!(
            archived_context_fingerprint(archive, ".").await.unwrap(),
            executable_fingerprint
        );

        tokio::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o644))
            .await
            .unwrap();
        let regular_fingerprint = context_fingerprint(&root).await.unwrap();
        assert_ne!(regular_fingerprint, executable_fingerprint);

        tokio::fs::remove_dir_all(&root).await.unwrap();
    }
}
