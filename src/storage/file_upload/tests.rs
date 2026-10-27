use super::*;
use crate::storage::{BlobStorage, LocalBlobStorage};
use tokio::io::AsyncSeekExt;

#[tokio::test]
async fn file_upload_rejects_mutation_and_limits_and_publishes_verified_content() {
    let root = std::env::temp_dir().join(format!("blob-file-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir(&root).await.unwrap();
    let source = root.join("source");
    tokio::fs::write(&source, b"original").await.unwrap();
    assert!(matches!(
        FileUpload::prepare(tokio::fs::File::open(&source).await.unwrap(), 7).await,
        Err(AppError::PayloadTooLarge(_))
    ));
    for replacement in [b"modified".as_slice(), b"short", b"longer than original"] {
        tokio::fs::write(&source, b"original").await.unwrap();
        let mut upload = FileUpload::prepare(tokio::fs::File::open(&source).await.unwrap(), 128)
            .await
            .unwrap();
        assert!(
            upload.metadata("test").is_err(),
            "unverified bytes cannot be published"
        );
        tokio::fs::write(&source, replacement).await.unwrap();
        assert!(matches!(
            upload.next_chunk(128).await,
            Err(AppError::Conflict(_))
        ));
    }
    let storage = LocalBlobStorage::new(root.join("blobs"));
    let bytes = (0..(READ_CHUNK_BYTES * 3 + 17))
        .map(|i| (i % 251) as u8)
        .collect::<Vec<_>>();
    tokio::fs::write(&source, &bytes).await.unwrap();
    let mut file = tokio::fs::File::open(&source).await.unwrap();
    // The API means the whole regular file, regardless of its caller's cursor.
    file.seek(std::io::SeekFrom::Start(19)).await.unwrap();
    let blob = storage
        .store_file("capture.pcap", file, bytes.len() as u64)
        .await
        .unwrap();
    assert_eq!(blob.hash, crate::utils::codec::sha256_hex(&bytes));
    assert_eq!(blob.size, bytes.len() as i64);
    assert_eq!(storage.load(&blob.hash).await.unwrap(), bytes);
    let destination = root
        .join("blobs")
        .join(&blob.hash[..2])
        .join(&blob.hash[2..4])
        .join(&blob.hash);
    let before = tokio::fs::metadata(&destination)
        .await
        .unwrap()
        .modified()
        .unwrap();
    let again = storage
        .store_file(
            "again.pcap",
            tokio::fs::File::open(&source).await.unwrap(),
            bytes.len() as u64,
        )
        .await
        .unwrap();
    assert_eq!(again.hash, blob.hash);
    assert_eq!(
        tokio::fs::metadata(&destination)
            .await
            .unwrap()
            .modified()
            .unwrap(),
        before,
        "duplicate file upload must not rewrite the existing blob"
    );
    tokio::fs::write(&source, b"").await.unwrap();
    let empty = storage
        .store_file("empty", tokio::fs::File::open(&source).await.unwrap(), 1)
        .await
        .unwrap();
    assert_eq!(empty.hash, crate::utils::codec::sha256_hex(b""));
    assert_eq!(storage.load(&empty.hash).await.unwrap(), b"");
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[tokio::test]
#[ignore = "requires disposable S3-compatible RSCTF_S3_* storage"]
async fn real_s3_file_upload_is_atomic_deduplicated_and_range_readable() {
    use futures::TryStreamExt;
    use tokio::io::AsyncWriteExt;
    let storage = crate::storage::S3BlobStorage::try_from_env()
        .unwrap()
        .unwrap();
    let root = std::env::temp_dir().join(format!("blob-s3-file-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir(&root).await.unwrap();
    let source = root.join("source");
    let mut file = tokio::fs::File::create(&source).await.unwrap();
    let mut chunk = vec![0x3a; 1024 * 1024];
    // Unique content prevents this disposable test from touching other blobs.
    chunk[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    let mut digest = Sha256::new();
    for _ in 0..17 {
        file.write_all(&chunk).await.unwrap();
        digest.update(&chunk);
    }
    file.sync_all().await.unwrap();
    drop(file);
    let hash = hex::encode(digest.finalize());
    let size = 17 * 1024 * 1024;
    let first = tokio::fs::File::open(&source).await.unwrap();
    let second = tokio::fs::File::open(&source).await.unwrap();
    let (first, second) = tokio::join!(
        storage.store_file("capture.pcap", first, size),
        storage.store_file("capture.pcap", second, size)
    );
    assert_eq!(first.unwrap().hash, hash);
    assert_eq!(second.unwrap().hash, hash);
    assert_eq!(storage.size(&hash).await.unwrap(), size);
    let parts: Vec<_> = storage
        .stream_range(&hash, 4..32)
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    let actual = parts.into_iter().flatten().collect::<Vec<_>>();
    assert_eq!(actual, chunk[4..32]);
    assert_eq!(
        crate::utils::codec::sha256_hex(&storage.load_bounded(&hash, size as usize).await.unwrap()),
        hash
    );
    storage.delete(&hash).await.unwrap();
    assert!(!storage.exists(&hash).await);
    tokio::fs::remove_dir_all(root).await.unwrap();
}
