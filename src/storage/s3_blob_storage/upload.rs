//! Verified multipart uploads through object_store, with one in-flight part.

use object_store::{
    MultipartUpload, ObjectStore, ObjectStoreExt, PutMultipartOptions, PutOptions, PutPayload,
};
use std::time::Duration;

use super::{blob_attributes, map_err, S3BlobStorage};
use crate::storage::file_upload::{FileUpload, MULTIPART_CHUNK_BYTES};
use crate::storage::StoredBlob;
use crate::utils::error::AppResult;

struct PendingUpload {
    upload: Option<Box<dyn MultipartUpload>>,
    input: Option<FileUpload>,
}

impl Drop for PendingUpload {
    fn drop(&mut self) {
        if let (Some(mut upload), Ok(runtime)) =
            (self.upload.take(), tokio::runtime::Handle::try_current())
        {
            let input = self.input.take();
            runtime.spawn(async move {
                // Keep admission until cleanup ends. Repeated cancellation
                // cannot create an unbounded queue of remote abort requests.
                let _input = input;
                let _ = tokio::time::timeout(Duration::from_secs(10), upload.abort()).await;
            });
        }
        // Process death cannot run async cleanup. S3 deployments must retain
        // an AbortIncompleteMultipartUpload lifecycle rule as a backstop.
    }
}

pub(super) async fn store_file(
    storage: &S3BlobStorage,
    name: &str,
    file: tokio::fs::File,
    max_bytes: u64,
) -> AppResult<StoredBlob> {
    let mut input = FileUpload::prepare(file, max_bytes).await?;
    let key = storage.key_for(&input.hash).expect("verified SHA-256 hash");
    match storage.client.head(&key).await {
        Ok(object) if object.size == input.size => {
            return Ok(input.existing_metadata(name));
        }
        Ok(_) | Err(object_store::Error::NotFound { .. }) => {}
        Err(error) => return Err(map_err(error)),
    }
    let attributes = blob_attributes(name);
    if input.size == 0 {
        input.next_chunk(1).await?;
        let metadata = input.metadata(name)?;
        storage
            .client
            .put_opts(
                &key,
                PutPayload::from(Vec::<u8>::new()),
                PutOptions {
                    attributes,
                    ..Default::default()
                },
            )
            .await
            .map_err(map_err)?;
        return Ok(metadata);
    }
    let upload = storage
        .client
        .put_multipart_opts(
            &key,
            PutMultipartOptions {
                attributes,
                ..Default::default()
            },
        )
        .await
        .map_err(map_err)?;
    publish_multipart(upload, input, name).await
}

async fn publish_multipart(
    upload: Box<dyn MultipartUpload>,
    input: FileUpload,
    name: &str,
) -> AppResult<StoredBlob> {
    let mut pending = PendingUpload {
        upload: Some(upload),
        input: Some(input),
    };
    let outcome: AppResult<StoredBlob> = async {
        let upload = pending
            .upload
            .as_mut()
            .expect("pending upload owns its handle");
        let input = pending
            .input
            .as_mut()
            .expect("pending upload owns its file and admission");
        while let Some(bytes) = input.next_chunk(MULTIPART_CHUNK_BYTES).await? {
            // Await each part before reading another. Neither a slow store nor
            // a large file can accumulate an unbounded queue of part buffers.
            upload
                .put_part(PutPayload::from(bytes))
                .await
                .map_err(map_err)?;
        }
        let metadata = input.metadata(name)?;
        upload.complete().await.map_err(map_err)?;
        Ok(metadata)
    }
    .await;
    if outcome.is_ok() {
        pending.upload.take();
    }
    outcome
}

#[cfg(test)]
mod tests;
