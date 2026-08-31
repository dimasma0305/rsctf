//! Immutable, bounded snapshots of a synchronized repository checkout.
//!
//! The persistent checkout is mutable and protected by [`CheckoutLockGuard`]. A
//! scan copies its validated working tree while that guard is held, then consumes
//! the guard and reads only this snapshot. The snapshot retains a filesystem
//! lock for the binding, so later scans remain serialized without reserving a
//! PostgreSQL session during blob storage, checker, build, or cleanup work.

use std::fs::{File, OpenOptions};
use std::path::{Component, Path, PathBuf};

use nix::errno::Errno;
use nix::fcntl::{Flock, FlockArg};

use super::{
    repository, CheckoutLockGuard, MAX_REPO_DEPTH, MAX_REPO_ENTRIES, MAX_REPO_FILES,
    MAX_REPO_FILE_BYTES, MAX_REPO_TOTAL_BYTES,
};
use crate::utils::error::{AppError, AppResult};

const SNAPSHOT_DIRECTORY: &str = ".snapshots";
const SCAN_LOCK_DIRECTORY: &str = ".scan-locks";
const MAX_STALE_SNAPSHOTS_PER_BINDING: usize = 128;

/// One immutable repository tree and its cross-process scan lease.
///
/// Dropping the value schedules best-effort cleanup while retaining the lease
/// until removal finishes. Call [`Self::cleanup`] when the caller needs an
/// acknowledged cleanup result.
#[must_use = "retain the snapshot for as long as repository paths are in use"]
pub struct CheckoutSnapshot {
    root: Option<PathBuf>,
    binding_id: i32,
    scan_lock: Option<Flock<File>>,
}

impl CheckoutSnapshot {
    async fn create(checkout: &Path, binding_id: i32) -> AppResult<Self> {
        if binding_id <= 0
            || checkout
                .file_name()
                .and_then(|name| name.to_str())
                .is_none_or(|name| name != binding_id.to_string())
        {
            return Err(AppError::internal(
                "repository checkout does not match its binding id",
            ));
        }
        let checkout = tokio::fs::canonicalize(checkout).await.map_err(|error| {
            AppError::internal(format!(
                "git_sync: canonicalize checkout {}: {error}",
                checkout.display()
            ))
        })?;
        let repos_root = checkout
            .parent()
            .ok_or_else(|| AppError::internal("repository checkout has no parent"))?
            .to_path_buf();
        let lock_root = checked_service_directory(&repos_root, SCAN_LOCK_DIRECTORY).await?;
        let snapshot_parent = checked_service_directory(&repos_root, SNAPSHOT_DIRECTORY)
            .await?
            .join(binding_id.to_string());
        create_checked_directory(&snapshot_parent).await?;

        let lock_path = lock_root.join(format!("{binding_id}.lock"));
        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|error| {
                AppError::internal(format!(
                    "git_sync: open repository scan lock {}: {error}",
                    lock_path.display()
                ))
            })?;
        let scan_lock =
            Flock::lock(lock_file, FlockArg::LockExclusiveNonblock).map_err(|(_, error)| {
                if error == Errno::EAGAIN {
                    AppError::conflict("A repository scan for this binding is already running")
                } else {
                    AppError::internal(format!("git_sync: lock repository scan: {error}"))
                }
            })?;

        // The caller still owns the distributed checkout guard here. Therefore
        // no second snapshot creator for this binding can race the lease setup,
        // and the exclusive filesystem scan lock proves all remaining trees are
        // crash leftovers rather than active scans.
        cleanup_stale_snapshots(&snapshot_parent).await?;
        let root = snapshot_parent.join(uuid::Uuid::new_v4().simple().to_string());
        tokio::fs::create_dir(&root).await.map_err(|error| {
            AppError::internal(format!(
                "git_sync: create checkout snapshot {}: {error}",
                root.display()
            ))
        })?;

        if let Err(error) = copy_checkout_tree(&checkout, &root).await {
            let _ = tokio::fs::remove_dir_all(&root).await;
            return Err(error);
        }
        Ok(Self {
            root: Some(root),
            binding_id,
            scan_lock: Some(scan_lock),
        })
    }

    pub fn path(&self) -> &Path {
        self.root
            .as_deref()
            .expect("checkout snapshot remains live until cleanup")
    }

    pub fn binding_id(&self) -> i32 {
        self.binding_id
    }

    /// Produce the durable identity to persist for one manifest in this
    /// snapshot. The caller passes this explicit value into repository import;
    /// no temporary snapshot path is ever written to the database.
    pub async fn manifest_identity(&self, manifest: &Path) -> AppResult<String> {
        let manifest = tokio::fs::canonicalize(manifest).await.map_err(|error| {
            AppError::internal(format!(
                "git_sync: canonicalize snapshot manifest {}: {error}",
                manifest.display()
            ))
        })?;
        let metadata = tokio::fs::metadata(&manifest).await.map_err(|error| {
            AppError::internal(format!(
                "git_sync: stat snapshot manifest {}: {error}",
                manifest.display()
            ))
        })?;
        let relative = manifest.strip_prefix(self.path()).map_err(|_| {
            AppError::bad_request("repository manifest escaped its immutable snapshot")
        })?;
        if !metadata.is_file()
            || relative.as_os_str().is_empty()
            || relative.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(AppError::bad_request(
                "repository manifest is not a regular snapshot file",
            ));
        }
        Ok(repository::scoped_manifest_identity(
            self.binding_id,
            &relative.to_string_lossy().replace('\\', "/"),
        ))
    }

    pub async fn cleanup(mut self) -> AppResult<()> {
        let root = self
            .root
            .take()
            .expect("checkout snapshot remains live until cleanup");
        let scan_lock = self
            .scan_lock
            .take()
            .expect("checkout snapshot retains its scan lease");
        let result = remove_snapshot(&root).await;
        drop(scan_lock);
        result
    }
}

impl Drop for CheckoutSnapshot {
    fn drop(&mut self) {
        let (Some(root), Some(scan_lock)) = (self.root.take(), self.scan_lock.take()) else {
            return;
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if let Err(error) = remove_snapshot(&root).await {
                    tracing::warn!(%error, path = %root.display(), "git_sync: snapshot cleanup deferred");
                }
                drop(scan_lock);
            });
        } else {
            if let Err(error) = std::fs::remove_dir_all(&root) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(%error, path = %root.display(), "git_sync: snapshot cleanup deferred");
                }
            }
            drop(scan_lock);
        }
    }
}

impl CheckoutLockGuard {
    /// Copy the current working tree, release the local and PostgreSQL checkout
    /// guards, and return a filesystem-leased immutable scan tree.
    pub async fn immutable_snapshot(
        self,
        checkout: &Path,
        binding_id: i32,
    ) -> AppResult<CheckoutSnapshot> {
        let snapshot = CheckoutSnapshot::create(checkout, binding_id).await;
        // Explicitly release the PostgreSQL session before the caller can begin
        // manifest packaging, object-store publication, or image builds.
        drop(self);
        snapshot
    }
}

async fn checked_service_directory(repos_root: &Path, name: &str) -> AppResult<PathBuf> {
    let path = repos_root.join(name);
    create_checked_directory(&path).await?;
    let canonical = tokio::fs::canonicalize(&path).await.map_err(|error| {
        AppError::internal(format!(
            "git_sync: canonicalize service directory {}: {error}",
            path.display()
        ))
    })?;
    if canonical.parent() != Some(repos_root) {
        return Err(AppError::internal(
            "repository snapshot directory escaped the managed repository root",
        ));
    }
    Ok(canonical)
}

async fn create_checked_directory(path: &Path) -> AppResult<()> {
    tokio::fs::create_dir_all(path).await.map_err(|error| {
        AppError::internal(format!(
            "git_sync: create service directory {}: {error}",
            path.display()
        ))
    })?;
    let metadata = tokio::fs::symlink_metadata(path).await.map_err(|error| {
        AppError::internal(format!(
            "git_sync: stat service directory {}: {error}",
            path.display()
        ))
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(AppError::internal(
            "repository snapshot storage must be a real directory",
        ));
    }
    Ok(())
}

async fn cleanup_stale_snapshots(parent: &Path) -> AppResult<()> {
    let mut entries = tokio::fs::read_dir(parent).await.map_err(|error| {
        AppError::internal(format!(
            "git_sync: read snapshot directory {}: {error}",
            parent.display()
        ))
    })?;
    let mut stale = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| AppError::internal(format!("git_sync: read snapshot entry: {error}")))?
    {
        let name = entry.file_name();
        let managed = name.to_str().is_some_and(|name| {
            name.len() == 32 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
        });
        if !managed {
            continue;
        }
        let metadata = entry.file_type().await.map_err(|error| {
            AppError::internal(format!("git_sync: stat snapshot entry: {error}"))
        })?;
        if metadata.is_dir() && !metadata.is_symlink() {
            stale.push(entry.path());
        }
    }
    if stale.len() > MAX_STALE_SNAPSHOTS_PER_BINDING {
        return Err(AppError::overloaded(
            "Repository snapshot cleanup capacity is busy",
            2,
        ));
    }
    stale.sort_unstable();
    for path in stale {
        remove_snapshot(&path).await?;
    }
    Ok(())
}

async fn remove_snapshot(path: &Path) -> AppResult<()> {
    match tokio::fs::remove_dir_all(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AppError::internal(format!(
            "git_sync: remove checkout snapshot {}: {error}",
            path.display()
        ))),
    }
}

#[derive(Default)]
struct CopyBudget {
    entries: usize,
    files: usize,
    bytes: u64,
}

async fn copy_checkout_tree(source: &Path, destination: &Path) -> AppResult<()> {
    let mut budget = CopyBudget::default();
    let mut stack = vec![(source.to_path_buf(), destination.to_path_buf(), 0usize)];
    while let Some((source_dir, destination_dir, depth)) = stack.pop() {
        if depth > MAX_REPO_DEPTH {
            return Err(AppError::bad_request("repository tree is too deep"));
        }
        let mut entries = tokio::fs::read_dir(&source_dir).await.map_err(|error| {
            AppError::internal(format!(
                "git_sync: read snapshot source {}: {error}",
                source_dir.display()
            ))
        })?;
        while let Some(entry) = entries.next_entry().await.map_err(|error| {
            AppError::internal(format!("git_sync: read snapshot source entry: {error}"))
        })? {
            let name = entry.file_name();
            if depth == 0 && name == ".git" {
                continue;
            }
            budget.entries += 1;
            if budget.entries > MAX_REPO_ENTRIES {
                return Err(AppError::bad_request(
                    "repository contains too many entries",
                ));
            }
            let source_path = entry.path();
            let destination_path = destination_dir.join(&name);
            let file_type = entry.file_type().await.map_err(|error| {
                AppError::internal(format!(
                    "git_sync: stat snapshot source {}: {error}",
                    source_path.display()
                ))
            })?;
            if file_type.is_dir() {
                tokio::fs::create_dir(&destination_path)
                    .await
                    .map_err(|error| {
                        AppError::internal(format!(
                            "git_sync: create snapshot directory {}: {error}",
                            destination_path.display()
                        ))
                    })?;
                stack.push((source_path, destination_path, depth + 1));
            } else if file_type.is_file() {
                budget.files += 1;
                if budget.files > MAX_REPO_FILES {
                    return Err(AppError::bad_request("repository contains too many files"));
                }
                let size = entry
                    .metadata()
                    .await
                    .map_err(|error| {
                        AppError::internal(format!(
                            "git_sync: stat snapshot file {}: {error}",
                            source_path.display()
                        ))
                    })?
                    .len();
                if size > MAX_REPO_FILE_BYTES
                    || budget.bytes.saturating_add(size) > MAX_REPO_TOTAL_BYTES
                {
                    return Err(AppError::bad_request("repository exceeds the size limit"));
                }
                tokio::fs::copy(&source_path, &destination_path)
                    .await
                    .map_err(|error| {
                        AppError::internal(format!(
                            "git_sync: copy snapshot file {}: {error}",
                            source_path.display()
                        ))
                    })?;
                budget.bytes = budget.bytes.saturating_add(size);
            } else if file_type.is_symlink() {
                copy_safe_symlink(source, destination, &source_path, &destination_path).await?;
            } else {
                return Err(AppError::bad_request(
                    "repository contains an unsupported file type",
                ));
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
async fn copy_safe_symlink(
    source_root: &Path,
    destination_root: &Path,
    source: &Path,
    destination: &Path,
) -> AppResult<()> {
    let target = tokio::fs::canonicalize(source).await.map_err(|error| {
        AppError::bad_request(format!(
            "repository contains an invalid symbolic link {}: {error}",
            source.display()
        ))
    })?;
    let relative = target
        .strip_prefix(source_root)
        .map_err(|_| AppError::bad_request("repository symbolic link escapes the checkout"))?;
    if relative
        .components()
        .next()
        .is_some_and(|component| component.as_os_str() == ".git")
    {
        return Err(AppError::bad_request(
            "repository symbolic link targets internal Git metadata",
        ));
    }
    tokio::fs::symlink(destination_root.join(relative), destination)
        .await
        .map_err(|error| {
            AppError::internal(format!(
                "git_sync: copy repository symbolic link {}: {error}",
                source.display()
            ))
        })
}

#[cfg(not(unix))]
async fn copy_safe_symlink(
    _source_root: &Path,
    _destination_root: &Path,
    _source: &Path,
    _destination: &Path,
) -> AppResult<()> {
    Err(AppError::bad_request(
        "repository symbolic links are unsupported on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn snapshot_is_immutable_scoped_and_filesystem_serialized() {
        let root = std::env::temp_dir().join(format!(
            "rsctf-checkout-snapshot-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let checkout = root.join("repos/7");
        let manifest = checkout.join("event/web/challenge.yaml");
        tokio::fs::create_dir_all(manifest.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::create_dir_all(checkout.join(".git"))
            .await
            .unwrap();
        tokio::fs::write(&manifest, b"name: first\n").await.unwrap();
        tokio::fs::write(checkout.join(".git/config"), b"secret")
            .await
            .unwrap();

        let snapshot = CheckoutSnapshot::create(&checkout, 7).await.unwrap();
        let snap_manifest = snapshot.path().join("event/web/challenge.yaml");
        assert_eq!(
            snapshot.manifest_identity(&snap_manifest).await.unwrap(),
            "binding/7/event/web/challenge.yaml"
        );
        assert!(!snapshot.path().join(".git").exists());
        tokio::fs::write(&manifest, b"name: second\n")
            .await
            .unwrap();
        assert_eq!(
            tokio::fs::read(&snap_manifest).await.unwrap(),
            b"name: first\n"
        );
        assert!(matches!(
            CheckoutSnapshot::create(&checkout, 7).await,
            Err(AppError::Conflict(_))
        ));

        snapshot.cleanup().await.unwrap();
        let retry = CheckoutSnapshot::create(&checkout, 7).await.unwrap();
        retry.cleanup().await.unwrap();
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
