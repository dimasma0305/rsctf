//! Manifest package helpers: enum/category parsing, local image intent, and
//! bounded ZIP creation for durable import/build source archives.

use std::ffi::OsStr;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use sea_orm::Iterable;

use super::{MAX_REPO_DEPTH, MAX_REPO_FILES, MAX_REPO_FILE_BYTES, MAX_REPO_TOTAL_BYTES};
use crate::utils::enums::ChallengeCategory;
use crate::utils::error::{AppError, AppResult};

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
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
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
            let declared = entry
                .metadata()
                .await
                .map_err(|error| {
                    AppError::internal(format!("git_sync: stat {}: {error}", path.display()))
                })?
                .len();
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
            files.push((relative, data));
        }
    }

    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::<u8>::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, data) in files {
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
