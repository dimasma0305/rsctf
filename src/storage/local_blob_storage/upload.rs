//! Atomic publication on one filesystem with bounded file buffers.

use nix::fcntl::{Flock, FlockArg};
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;

use super::LocalBlobStorage;
use crate::storage::file_upload::{io_error, FileUpload, READ_CHUNK_BYTES};
use crate::storage::StoredBlob;
use crate::utils::error::AppResult;

struct TemporaryBlob {
    path: PathBuf,
    input: Option<FileUpload>,
    lock: Option<Flock<std::fs::File>>,
    published: bool,
}

impl Drop for TemporaryBlob {
    fn drop(&mut self) {
        if self.published {
            return;
        }
        let path = self.path.clone();
        // Only this operation's create-new UUID path is eligible for removal.
        // A cancelled write may still own its descriptor; unlinking the name
        // leaves no partial content address visible to another reader.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let input = self.input.take();
            let lock = self.lock.take();
            runtime.spawn(async move {
                // Keep admission and the cross-process lock through unlink.
                // Cancelled uploads cannot accumulate unbounded cleanup tasks.
                let (_input, _lock) = (input, lock);
                let _ = tokio::fs::remove_file(path).await;
            });
        }
    }
}

pub(super) async fn store_file(
    storage: &LocalBlobStorage,
    name: &str,
    file: tokio::fs::File,
    max_bytes: u64,
) -> AppResult<StoredBlob> {
    let input = FileUpload::prepare(file, max_bytes).await?;
    let destination = storage
        .path_for(&input.hash)
        .expect("verified SHA-256 hash");
    match tokio::fs::symlink_metadata(&destination).await {
        Ok(metadata) if metadata.is_file() && metadata.len() == input.size => {
            return Ok(input.existing_metadata(name));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error(error)),
    }
    let parent = destination.parent().expect("sharded blob parent");
    tokio::fs::create_dir_all(parent).await.map_err(io_error)?;
    // A dedicated directory makes crash cleanup independent of blob count.
    // It shares the storage filesystem with the destination: rename is atomic.
    let staging = storage.root.join(".uploads");
    tokio::fs::create_dir_all(&staging)
        .await
        .map_err(io_error)?;
    let temporary_path = staging.join(format!("{}.upload", uuid::Uuid::new_v4()));
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let output = options.open(&temporary_path).await.map_err(io_error)?;
    let mut temporary = TemporaryBlob {
        path: temporary_path,
        input: Some(input),
        lock: None,
        published: false,
    };
    let file = output.into_std().await;
    let lock = Flock::lock(file, FlockArg::LockExclusiveNonblock)
        .map_err(|(_, error)| io_error(std::io::Error::from_raw_os_error(error as i32)))?;
    let mut output = tokio::fs::File::from_std(lock.try_clone().map_err(io_error)?);
    temporary.lock = Some(lock);
    let input = temporary
        .input
        .as_mut()
        .expect("pending upload owns admission");
    while let Some(bytes) = input.next_chunk(READ_CHUNK_BYTES).await? {
        output.write_all(&bytes).await.map_err(io_error)?;
    }
    let metadata = input.metadata(name)?;
    output.sync_all().await.map_err(io_error)?;
    drop(output);
    tokio::fs::rename(&temporary.path, &destination)
        .await
        .map_err(io_error)?;
    // Sync the containing directory so a committed reference survives a host
    // crash after successful publication, just like the verified file bytes.
    let parent = parent.to_path_buf();
    tokio::task::spawn_blocking(move || {
        std::fs::File::open(parent)?.sync_all()?;
        std::fs::File::open(staging)?.sync_all()
    })
    .await
    .map_err(|error| crate::utils::error::AppError::internal(error.to_string()))?
    .map_err(io_error)?;
    temporary.published = true;
    Ok(metadata)
}
