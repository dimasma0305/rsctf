//! Blob storage: content-addressed roundtrip and path-traversal rejection.

use rsctf::storage::{BlobStorage, LocalBlobStorage};

fn tmp(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("rsctf-{tag}-{}", std::process::id()))
}

#[tokio::test]
async fn store_and_load_roundtrip() {
    let dir = tmp("blob");
    let s = LocalBlobStorage::new(&dir);
    let blob = s.store("hello.txt", b"hello world").await.unwrap();
    assert_eq!(blob.hash.len(), 64);
    assert_eq!(s.load(&blob.hash).await.unwrap(), b"hello world");
    assert!(s.exists(&blob.hash).await);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn rejects_path_traversal_and_malformed_hashes() {
    let dir = tmp("blob-trav");
    let s = LocalBlobStorage::new(&dir);
    let long_non_hex = "g".repeat(64);
    for bad in [
        "../../../../etc/passwd",
        "/etc/passwd",
        "..",
        "aa/bb/cc",
        "short",
        long_non_hex.as_str(),
    ] {
        assert!(s.load(bad).await.is_err(), "load({bad}) must be rejected");
        assert!(!s.exists(bad).await, "exists({bad}) must be false");
        // delete is a no-op (idempotent) for a rejected hash, never a traversal.
        assert!(s.delete(bad).await.is_ok());
    }
    let _ = std::fs::remove_dir_all(&dir);
}
