//! git_sync/checker.rs — prepare an A&D/KotH functional checker as an isolated
//! Python venv (no Docker). Called from the manifest importer: copies `./checker/`
//! into an immutable `<storage>/checkers/<game>/<slug>/revisions/<uuid>` directory,
//! builds a venv alongside it, optionally installs tightly pinned binary wheels,
//! and the run path
//! (`services::ad_engine::sandbox`) later sandbox-execs `venv/bin/python3 src/run.py`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use crate::utils::error::{AppError, AppResult};

use super::package::slugify;

const REQUIREMENTS_MAX_BYTES: usize = 16 * 1024;
const REQUIREMENTS_MAX_COUNT: usize = 32;
const REQUIREMENT_NAME_MAX_BYTES: usize = 128;
const REQUIREMENT_VERSION_MAX_BYTES: usize = 128;
const VENV_CREATE_TIMEOUT: Duration = Duration::from_secs(30);
const REQUIREMENTS_INSTALL_TIMEOUT: Duration = Duration::from_secs(120);

/// The dir under `./checker/` that holds `run.py` (the checker entrypoint) — the
/// challenge's `checker/` itself, or `checker/src/`. `None` if there's no checker.
pub(super) fn checker_source_dir(checker_dir: &std::path::Path) -> Option<PathBuf> {
    if checker_dir.join("run.py").is_file() {
        Some(checker_dir.to_path_buf())
    } else if checker_dir.join("src").join("run.py").is_file() {
        Some(checker_dir.join("src"))
    } else {
        None
    }
}

/// A unique immutable checker revision below
/// `<storage>/checkers/<game>/<slug>/revisions/`.
pub(super) fn checker_dest_dir(storage_root: &Path, game_id: i32, name: &str) -> String {
    checker_dest_dir_at(storage_root, game_id, name)
        .to_string_lossy()
        .into_owned()
}

pub(super) async fn cleanup_unpublished_checker(
    prepared: bool,
    preparation: Option<&(String, PathBuf)>,
    guard: &mut Option<crate::utils::single_flight::PgAdvisoryLock>,
) {
    if prepared {
        if let Some((dest, _)) = preparation {
            if let Err(error) = tokio::fs::remove_dir_all(dest).await {
                tracing::warn!(%error, path = %dest, "git_sync: unpublished checker cleanup failed");
            }
        }
    }
    if let Some(guard) = guard.take() {
        if let Err(error) = guard.release().await {
            tracing::warn!(%error, "checker publication guard release failed");
        }
    }
}

fn checker_dest_dir_at(root: &Path, game_id: i32, name: &str) -> PathBuf {
    root.join("checkers")
        .join(game_id.to_string())
        .join(slugify(name))
        .join("revisions")
        .join(uuid::Uuid::new_v4().simple().to_string())
}

fn requirement_error(line_number: usize) -> AppError {
    AppError::bad_request(format!(
        "checker requirements.txt line {line_number} must be an exact name==version pin"
    ))
}

fn valid_requirement_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= REQUIREMENT_NAME_MAX_BYTES
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_requirement_version(version: &str) -> bool {
    let bytes = version.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= REQUIREMENT_VERSION_MAX_BYTES
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.iter().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+' | b'!')
        })
}

fn normalized_requirement_name(name: &str) -> String {
    let mut normalized = String::with_capacity(name.len());
    let mut last_was_separator = false;
    for byte in name.bytes() {
        if matches!(byte, b'-' | b'_' | b'.') {
            if !last_was_separator {
                normalized.push('-');
                last_was_separator = true;
            }
        } else {
            normalized.push(char::from(byte.to_ascii_lowercase()));
            last_was_separator = false;
        }
    }
    normalized
}

fn validate_checker_requirements(contents: &[u8]) -> AppResult<Vec<String>> {
    if contents.len() > REQUIREMENTS_MAX_BYTES {
        return Err(AppError::bad_request(format!(
            "checker requirements.txt exceeds the {REQUIREMENTS_MAX_BYTES}-byte limit"
        )));
    }
    let text = std::str::from_utf8(contents)
        .map_err(|_| AppError::bad_request("checker requirements.txt must be valid UTF-8"))?;
    let mut requirements = Vec::new();
    let mut names = std::collections::HashSet::new();

    for (index, raw_line) in text.lines().enumerate() {
        let line_number = index + 1;
        let line = raw_line
            .split_once('#')
            .map_or(raw_line, |(requirement, _)| requirement)
            .trim();
        if line.is_empty() {
            continue;
        }
        if requirements.len() >= REQUIREMENTS_MAX_COUNT {
            return Err(AppError::bad_request(format!(
                "checker requirements.txt contains more than {REQUIREMENTS_MAX_COUNT} packages"
            )));
        }

        let Some((name, version)) = line.split_once("==") else {
            return Err(requirement_error(line_number));
        };
        if version.contains("==")
            || !valid_requirement_name(name)
            || !valid_requirement_version(version)
        {
            return Err(requirement_error(line_number));
        }
        if !names.insert(normalized_requirement_name(name)) {
            return Err(AppError::bad_request(format!(
                "checker requirements.txt line {line_number} repeats package {name}"
            )));
        }
        requirements.push(format!("{name}=={version}"));
    }

    Ok(requirements)
}

async fn read_checker_requirements(src_dir: &Path) -> AppResult<Vec<String>> {
    use tokio::io::AsyncReadExt;

    let path = src_dir.join("requirements.txt");
    let metadata = match tokio::fs::symlink_metadata(&path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(AppError::internal(format!(
                "checker requirements stat: {error}"
            )))
        }
    };
    if !metadata.file_type().is_file() {
        return Err(AppError::bad_request(
            "checker requirements.txt must be a regular file",
        ));
    }
    if metadata.len() > REQUIREMENTS_MAX_BYTES as u64 {
        return Err(AppError::bad_request(format!(
            "checker requirements.txt exceeds the {REQUIREMENTS_MAX_BYTES}-byte limit"
        )));
    }

    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|error| AppError::internal(format!("checker requirements open: {error}")))?;
    let mut contents = Vec::with_capacity(metadata.len() as usize);
    file.take((REQUIREMENTS_MAX_BYTES + 1) as u64)
        .read_to_end(&mut contents)
        .await
        .map_err(|error| AppError::internal(format!("checker requirements read: {error}")))?;
    validate_checker_requirements(&contents)
}

/// Validate the inert checker tree before an enabled challenge is considered
/// runtime-equivalent. Preparation performs the same checks, but live imports
/// deliberately do not prepare a replacement and therefore need this explicit
/// validation path.
pub(super) async fn validate_checker_source(src_dir: &Path) -> AppResult<()> {
    let entrypoint = tokio::fs::symlink_metadata(src_dir.join("run.py"))
        .await
        .map_err(|error| AppError::bad_request(format!("checker/run.py is unreadable: {error}")))?;
    if !entrypoint.file_type().is_file() {
        return Err(AppError::bad_request(
            "checker/run.py must be a regular file",
        ));
    }
    read_checker_requirements(src_dir).await?;
    Ok(())
}

/// Build the checker environment (no Docker) in a private sibling staging
/// directory, make it readable by the sandbox uid, then atomically rename it to
/// `dest`. A published revision is never modified or replaced. Optional
/// dependencies must be exact direct pins; pip may resolve their dependencies,
/// but every installed distribution must be available as a binary wheel. This
/// prevents source-distribution setup.py/PEP 517 build hooks from running before
/// the run-path sandbox applies.
pub(super) async fn prepare_checker_venv(dest: &str, src_dir: &std::path::Path) -> AppResult<()> {
    let requirements = read_checker_requirements(src_dir).await?;
    let dest = std::path::Path::new(dest);
    let parent = dest
        .parent()
        .ok_or_else(|| AppError::internal("checker destination has no parent"))?;
    let revision = dest
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| AppError::internal("checker destination is not valid UTF-8"))?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|e| AppError::internal(format!("checker mkdir: {e}")))?;
    if tokio::fs::try_exists(dest)
        .await
        .map_err(|e| AppError::internal(format!("checker destination stat: {e}")))?
    {
        return Err(AppError::internal(
            "checker revision already exists and is immutable",
        ));
    }

    // The staging directory lives beside the final revision, so rename cannot
    // cross filesystems and publication is atomic to every engine replica.
    let staging = parent.join(format!(
        ".{revision}.staging-{}",
        uuid::Uuid::new_v4().simple()
    ));
    tokio::fs::create_dir(&staging)
        .await
        .map_err(|e| AppError::internal(format!("checker staging mkdir: {e}")))?;

    let result = prepare_staging(&staging, src_dir, &requirements).await;
    if let Err(error) = result {
        remove_staging(&staging).await;
        return Err(error);
    }
    match tokio::fs::try_exists(dest).await {
        Ok(false) => {}
        Ok(true) => {
            remove_staging(&staging).await;
            return Err(AppError::internal(
                "checker revision already exists and is immutable",
            ));
        }
        Err(error) => {
            remove_staging(&staging).await;
            return Err(AppError::internal(format!(
                "checker destination stat: {error}"
            )));
        }
    }
    if let Err(error) = tokio::fs::rename(&staging, dest).await {
        remove_staging(&staging).await;
        return Err(AppError::internal(format!(
            "checker atomic publish: {error}"
        )));
    }
    tracing::info!(dest = %dest.display(), "git_sync: published sandboxed checker revision");
    Ok(())
}

/// Materialize the reviewed `checker/` tree from a durable challenge archive,
/// then publish it through the same immutable venv path as repository imports.
/// `None` is a valid result: an A&D/KotH challenge without a custom checker uses
/// the built-in TCP probe. A malformed or ambiguous checker archive is rejected
/// instead of activating an unreviewed executable.
pub(super) async fn prepare_checker_from_archive(
    storage_root: &Path,
    game_id: i32,
    name: &str,
    archive: Vec<u8>,
) -> AppResult<Option<String>> {
    // Shape this temporary tree like a managed revision hierarchy so the
    // conservative checker GC can reclaim it after the same grace period if a
    // process dies before the normal cleanup below.
    let source_parent = storage_root
        .join("checkers")
        .join(".review-sources")
        .join("work")
        .join("revisions");
    tokio::fs::create_dir_all(&source_parent)
        .await
        .map_err(|error| AppError::internal(format!("checker source mkdir: {error}")))?;
    let source_root = source_parent.join(uuid::Uuid::new_v4().simple().to_string());
    let extract_root = source_root.clone();
    let extracted =
        tokio::task::spawn_blocking(move || extract_checker_from_archive(&archive, &extract_root))
            .await
            .map_err(|error| AppError::internal(format!("checker archive task failed: {error}")))?;

    let source = match extracted {
        Ok(Some(source)) => source,
        Ok(None) => {
            remove_staging(&source_root).await;
            return Ok(None);
        }
        Err(error) => {
            remove_staging(&source_root).await;
            return Err(error);
        }
    };
    let dest = checker_dest_dir(storage_root, game_id, name);
    let result = prepare_checker_venv(&dest, &source).await;
    remove_staging(&source_root).await;
    result.map(|()| Some(dest))
}

/// Validate and reuse a checker revision that a previous approval attempt
/// published while the challenge remained inert (for example, because its image
/// build failed). Only platform-managed immutable revision paths are accepted.
pub(super) async fn validate_prepared_checker_revision(
    storage_root: &Path,
    game_id: i32,
    revision: &str,
) -> AppResult<()> {
    let managed_root =
        tokio::fs::canonicalize(storage_root.join("checkers").join(game_id.to_string()))
            .await
            .map_err(|error| AppError::internal(format!("checker root unavailable: {error}")))?;
    let revision = tokio::fs::canonicalize(revision)
        .await
        .map_err(|error| AppError::bad_request(format!("prepared checker unavailable: {error}")))?;
    let relative = revision.strip_prefix(&managed_root).map_err(|_| {
        AppError::bad_request("prepared checker is outside managed checker storage")
    })?;
    let components: Vec<_> = relative.components().collect();
    let valid_shape = components.len() == 3
        && matches!(components[0], std::path::Component::Normal(_))
        && components[1].as_os_str() == "revisions"
        && components[2].as_os_str().to_str().is_some_and(|value| {
            value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        });
    if !valid_shape
        || !revision.join("src/run.py").is_file()
        || !revision.join("venv/bin/python3").is_file()
    {
        return Err(AppError::bad_request(
            "prepared checker revision is incomplete or invalid",
        ));
    }
    Ok(())
}

const ARCHIVE_MAX_ENTRIES: usize = 2_048;
const ARCHIVE_MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
const ARCHIVE_MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
const ARCHIVE_MAX_COMPRESSION_RATIO: u64 = 200;
const ARCHIVE_MAX_PATH_COMPONENTS: usize = 32;

fn extract_checker_from_archive(archive: &[u8], root: &Path) -> AppResult<Option<PathBuf>> {
    use std::io::Read;

    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive))
        .map_err(|error| AppError::bad_request(format!("invalid checker archive: {error}")))?;
    if zip.len() > ARCHIVE_MAX_ENTRIES {
        return Err(AppError::bad_request(
            "checker archive contains too many entries",
        ));
    }

    let checker_root = root.join("checker");
    let mut files = std::collections::HashSet::new();
    let mut total_bytes = 0u64;
    let mut found_checker = false;
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|error| AppError::bad_request(format!("checker archive read: {error}")))?;
        let Some(path) = crate::utils::archive::canonical_zip_entry_path(&entry) else {
            return Err(AppError::bad_request(
                "checker archive contains an invalid path",
            ));
        };
        if path.components().count() > ARCHIVE_MAX_PATH_COMPONENTS {
            return Err(AppError::bad_request("checker archive path is too deep"));
        }
        let Ok(relative) = path.strip_prefix("checker") else {
            continue;
        };
        if relative.as_os_str().is_empty() || entry.is_dir() {
            continue;
        }
        // Unix-mode symlinks are regular ZIP entries whose payload is the link
        // target. Never materialize them into a reviewed source tree.
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(AppError::bad_request(
                "checker archive must not contain symlinks",
            ));
        }
        if entry.size() > ARCHIVE_MAX_FILE_BYTES {
            return Err(AppError::bad_request("checker archive entry is too large"));
        }
        let compressed = entry.compressed_size().max(1);
        if entry.size() > compressed.saturating_mul(ARCHIVE_MAX_COMPRESSION_RATIO) {
            return Err(AppError::bad_request(
                "checker archive compression ratio is too high",
            ));
        }
        if total_bytes.saturating_add(entry.size()) > ARCHIVE_MAX_TOTAL_BYTES {
            return Err(AppError::bad_request(
                "checker archive expands beyond the size limit",
            ));
        }
        let relative = relative.to_path_buf();
        if !files.insert(relative.clone()) {
            return Err(AppError::bad_request(
                "checker archive contains duplicate paths",
            ));
        }
        let output = checker_root.join(&relative);
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                AppError::internal(format!("checker archive create dir: {error}"))
            })?;
        }
        let remaining = ARCHIVE_MAX_TOTAL_BYTES.saturating_sub(total_bytes);
        let max_write = ARCHIVE_MAX_FILE_BYTES.min(remaining);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output)
            .map_err(|error| AppError::internal(format!("checker archive create: {error}")))?;
        let written = std::io::copy(&mut entry.by_ref().take(max_write + 1), &mut file)
            .map_err(|error| AppError::internal(format!("checker archive write: {error}")))?;
        if written > max_write {
            return Err(AppError::bad_request(
                "checker archive expands beyond the size limit",
            ));
        }
        total_bytes = total_bytes.saturating_add(written);
        found_checker = true;
    }

    if !found_checker {
        return Ok(None);
    }
    checker_source_dir(&checker_root)
        .map(Some)
        .ok_or_else(|| AppError::bad_request("checker archive is missing checker/run.py"))
}

async fn install_checker_requirements(venv: &Path, requirements: &[String]) -> AppResult<()> {
    if requirements.is_empty() {
        return Ok(());
    }

    let mut command = tokio::process::Command::new(venv.join("bin/python3"));
    command
        .arg("-m")
        .arg("pip")
        .arg("--isolated")
        .arg("--disable-pip-version-check")
        .arg("--no-input")
        .arg("--no-cache-dir")
        .arg("--require-virtualenv")
        .arg("install")
        // Direct requirements are exact pins. Dependencies may resolve normally,
        // but the global wheel-only rule prevents arbitrary setup.py/PEP 517
        // build hooks in this privileged preparation process.
        .arg("--only-binary=:all:")
        .arg("--no-compile")
        .arg("--progress-bar=off")
        .arg("--")
        .args(requirements)
        .current_dir(venv)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("PYTHONNOUSERSITE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);

    let status = match tokio::time::timeout(REQUIREMENTS_INSTALL_TIMEOUT, command.status()).await {
        Err(_) => {
            return Err(AppError::bad_request(format!(
                "checker requirements installation timed out after {} seconds",
                REQUIREMENTS_INSTALL_TIMEOUT.as_secs()
            )))
        }
        Ok(Err(error)) => {
            return Err(AppError::internal(format!(
                "checker pip install failed to start: {error}"
            )))
        }
        Ok(Ok(status)) => status,
    };
    if !status.success() {
        return Err(AppError::bad_request(format!(
            "checker requirements installation failed ({status}); ensure every exact pin has a binary wheel compatible with the platform Python"
        )));
    }
    Ok(())
}

async fn prepare_staging(staging: &Path, src_dir: &Path, requirements: &[String]) -> AppResult<()> {
    use tokio::process::Command;

    let dest_src = staging.join("src");
    tokio::fs::create_dir(&dest_src)
        .await
        .map_err(|e| AppError::internal(format!("checker source mkdir: {e}")))?;
    copy_dir_recursive(src_dir, &dest_src).await?;

    // Build the venv.
    let venv = staging.join("venv");
    let mut command = Command::new("python3");
    command
        .arg("-m")
        .arg("venv")
        .arg(&venv)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let status = match tokio::time::timeout(VENV_CREATE_TIMEOUT, command.status()).await {
        Err(_) => {
            return Err(AppError::internal(format!(
                "python3 -m venv timed out after {} seconds",
                VENV_CREATE_TIMEOUT.as_secs()
            )))
        }
        Ok(Err(error)) => {
            return Err(AppError::internal(format!(
                "python3 -m venv failed to start: {error}"
            )))
        }
        Ok(Ok(status)) => status,
    };
    if !status.success() {
        return Err(AppError::internal(format!(
            "python3 -m venv failed ({status})"
        )));
    }
    install_checker_requirements(&venv, requirements).await?;

    // Pinned dependencies are now installed, before the tree becomes immutable.
    // World-traversable/readable so the dropped checker uid can reach it, while
    // stripping group/other writes inherited from a permissive repository mode.
    // Source and venv files stay immutable. The revision root is made
    // application-group writable below so another replica can create GC marker
    // files even on an RWX filesystem with root-squash. The sandbox drops every
    // supplementary group and Landlock grants no writes to this tree.
    let chmod_ok = Command::new("chmod")
        .args(["-R", "u-s,g-s,o-t,go-w,a+rX"])
        .arg(staging)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .status()
        .await
        .is_ok_and(|status| status.success());
    if !chmod_ok {
        return Err(AppError::internal("checker chmod failed"));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        // Helm gives every role fsGroup 10000; Compose roles share root's group.
        // Preserve the creator's group and inherit it for later marker files.
        tokio::fs::set_permissions(staging, std::fs::Permissions::from_mode(0o2775))
            .await
            .map_err(|error| AppError::internal(format!("checker root chmod: {error}")))?;
        let execution_lock = staging.join(super::checker_gc::EXECUTION_LOCK_FILE);
        tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&execution_lock)
            .await
            .map_err(|error| AppError::internal(format!("checker lock create: {error}")))?;
        tokio::fs::set_permissions(&execution_lock, std::fs::Permissions::from_mode(0o660))
            .await
            .map_err(|error| AppError::internal(format!("checker lock chmod: {error}")))?;
    }
    Ok(())
}

async fn remove_staging(staging: &Path) {
    if let Err(error) = tokio::fs::remove_dir_all(staging).await {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(%error, path = %staging.display(), "git_sync: checker staging cleanup failed");
        }
    }
}

/// Recursively copy `src` into `dst` (files + subdirs), skipping symlinks.
async fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> AppResult<()> {
    let mut stack = vec![(src.to_path_buf(), dst.to_path_buf())];
    while let Some((s, d)) = stack.pop() {
        let mut rd = tokio::fs::read_dir(&s)
            .await
            .map_err(|e| AppError::internal(format!("checker read_dir: {e}")))?;
        loop {
            let entry = rd
                .next_entry()
                .await
                .map_err(|e| AppError::internal(format!("checker read_dir entry: {e}")))?;
            let Some(entry) = entry else {
                break;
            };
            let ft = entry
                .file_type()
                .await
                .map_err(|e| AppError::internal(format!("checker source stat: {e}")))?;
            if ft.is_symlink() {
                continue;
            }
            let sp = entry.path();
            let dp = d.join(entry.file_name());
            if ft.is_dir() {
                tokio::fs::create_dir(&dp)
                    .await
                    .map_err(|e| AppError::internal(format!("checker mkdir: {e}")))?;
                stack.push((sp, dp));
            } else if ft.is_file() {
                tokio::fs::copy(&sp, &dp)
                    .await
                    .map_err(|e| AppError::internal(format!("checker copy: {e}")))?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, data) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn requirements_accept_only_exact_pins_and_comments() {
        let requirements = validate_checker_requirements(
            br#"
# Exact direct pins can still resolve their normal wheel dependencies.
requests==2.32.4
zope.interface==7.2  # inline comments are allowed
demo-package==1!2.0rc1.post2+local.1
"#,
        )
        .unwrap();

        assert_eq!(
            requirements,
            [
                "requests==2.32.4",
                "zope.interface==7.2",
                "demo-package==1!2.0rc1.post2+local.1",
            ]
        );
        assert!(validate_checker_requirements(b"\n# comments only\n")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn requirements_reject_options_urls_paths_and_unpinned_specs() {
        let invalid = [
            "--index-url https://packages.example.test/simple",
            "demo @ https://packages.example.test/demo.whl",
            "-e ../demo",
            "../demo",
            "demo",
            "demo>=1.0",
            "demo==1.*",
            "demo[extra]==1.0",
            "demo==1.0; python_version >= '3.11'",
            "demo===1.0",
            "demo==../demo.whl",
            "demo==1.0 --hash=sha256:abc",
        ];

        for requirement in invalid {
            assert!(
                validate_checker_requirements(requirement.as_bytes()).is_err(),
                "accepted invalid requirement: {requirement}"
            );
        }
        assert!(validate_checker_requirements(&[0xff]).is_err());
    }

    #[test]
    fn requirements_enforce_size_count_and_unique_package_caps() {
        assert!(validate_checker_requirements(&vec![b'a'; REQUIREMENTS_MAX_BYTES + 1]).is_err());

        let too_many = (0..=REQUIREMENTS_MAX_COUNT)
            .map(|index| format!("package{index}==1\n"))
            .collect::<String>();
        assert!(validate_checker_requirements(too_many.as_bytes()).is_err());

        assert!(validate_checker_requirements(b"some_package==1\nSome.Package==1\n").is_err());
    }

    #[test]
    fn destination_is_unique_and_nested_below_challenge_slug() {
        let root = Path::new("/tmp/rsctf-checker-test");
        let first = checker_dest_dir_at(root, 42, "Web / Service");
        let second = checker_dest_dir_at(root, 42, "Web / Service");

        assert_ne!(first, second);
        assert_eq!(
            first.parent().and_then(Path::file_name),
            Some(std::ffi::OsStr::new("revisions"))
        );
        assert!(first.starts_with(root.join("checkers").join("42").join("web-service")));
    }

    #[tokio::test]
    async fn publication_is_complete_and_never_replaces_a_revision() {
        let root = std::env::temp_dir().join(format!(
            "rsctf-checker-publication-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let source = root.join("source");
        tokio::fs::create_dir_all(&source).await.unwrap();
        tokio::fs::write(source.join("run.py"), b"print('first')\n")
            .await
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                source.join("run.py"),
                std::fs::Permissions::from_mode(0o777),
            )
            .unwrap();
        }
        let dest = checker_dest_dir_at(&root, 7, "Checker");
        let dest_text = dest.to_string_lossy().into_owned();

        prepare_checker_venv(&dest_text, &source).await.unwrap();
        assert_eq!(
            tokio::fs::read(dest.join("src/run.py")).await.unwrap(),
            b"print('first')\n"
        );
        assert!(dest.join("venv/bin/python3").exists());
        assert!(dest
            .join(super::super::checker_gc::EXECUTION_LOCK_FILE)
            .exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dest.join("src/run.py"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o022,
                0,
                "sandbox uid must not mutate checker source"
            );
            let root_mode = std::fs::metadata(&dest).unwrap().permissions().mode();
            assert_eq!(root_mode & 0o2777, 0o2775);
            let lock_mode =
                std::fs::metadata(dest.join(super::super::checker_gc::EXECUTION_LOCK_FILE))
                    .unwrap()
                    .permissions()
                    .mode();
            assert_eq!(lock_mode & 0o0777, 0o0660);
        }

        tokio::fs::write(source.join("run.py"), b"print('second')\n")
            .await
            .unwrap();
        assert!(prepare_checker_venv(&dest_text, &source).await.is_err());
        assert_eq!(
            tokio::fs::read(dest.join("src/run.py")).await.unwrap(),
            b"print('first')\n"
        );

        let mut entries = tokio::fs::read_dir(dest.parent().unwrap()).await.unwrap();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            assert!(
                !entry.file_name().to_string_lossy().contains(".staging-"),
                "staging directory leaked"
            );
        }
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[test]
    fn reviewed_archive_extracts_only_an_unambiguous_checker_tree() {
        let root = std::env::temp_dir().join(format!(
            "rsctf-checker-archive-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let bytes = archive(&[
            ("challenge.yml", b"name: test\n"),
            ("checker/run.py", b"print('reviewed')\n"),
            ("checker/helper.py", b"VALUE = 1\n"),
        ]);
        let source = extract_checker_from_archive(&bytes, &root)
            .unwrap()
            .expect("custom checker");
        assert_eq!(
            std::fs::read(source.join("run.py")).unwrap(),
            b"print('reviewed')\n"
        );
        assert!(!root.join("challenge.yml").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reviewed_archive_rejects_ambiguous_or_traversing_checker_paths() {
        let root = std::env::temp_dir().join(format!(
            "rsctf-checker-archive-invalid-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let ambiguous = archive(&[("checker\\run.py", b"ambiguous")]);
        assert!(extract_checker_from_archive(&ambiguous, &root).is_err());

        let traversal = archive(&[("checker/../run.py", b"escape")]);
        assert!(extract_checker_from_archive(&traversal, &root).is_err());
        let _ = std::fs::remove_dir_all(root);
    }
}
