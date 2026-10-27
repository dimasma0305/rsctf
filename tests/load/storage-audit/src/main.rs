//! Fixed-rate comparison of the production buffered and file-based blob APIs.
//! This measures a component, not HTTP, capture offloading, or durable references.
#![allow(dead_code, unused_imports)]

use futures::TryStreamExt;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::io::AsyncWriteExt;

#[path = "../../../../src/utils/codec.rs"]
pub(crate) mod codec;
#[path = "../../../../src/utils/content_disposition.rs"]
pub(crate) mod content_disposition;
#[path = "../../../../src/storage/mod.rs"]
mod storage;
// Only error construction is substituted: no HTTP/SQL dependencies or store
// algorithms are replaced in the measured successful path.
mod utils {
    pub(crate) use crate::{codec, content_disposition};
    pub mod error {
        pub type AppResult<T> = Result<T, AppError>;
        #[derive(Debug)]
        pub struct AppError(String);
        impl std::fmt::Display for AppError {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }
        impl std::error::Error for AppError {}
        macro_rules! constructor {
            ($($name:ident),*) => { $(pub fn $name(value: impl Into<String>) -> Self { Self(value.into()) })* };
        }
        impl AppError {
            constructor!(
                internal,
                bad_request,
                not_found,
                conflict,
                payload_too_large,
                unavailable
            );
            pub fn too_many_requests(_: u64) -> Self {
                Self("upload admission full".into())
            }
        }
    }
}
use storage::{BlobStorage, LocalBlobStorage, S3BlobStorage};

fn usage() -> (f64, i64) {
    let mut value = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_SELF, value.as_mut_ptr()) },
        0
    );
    let value = unsafe { value.assume_init() };
    let seconds = |time: libc::timeval| time.tv_sec as f64 + time.tv_usec as f64 / 1e6;
    (
        seconds(value.ru_utime) + seconds(value.ru_stime),
        value.ru_maxrss,
    )
}
fn io_usage() -> std::collections::BTreeMap<String, u64> {
    std::fs::read_to_string("/proc/self/io")
        .unwrap()
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            Some((key.to_owned(), value.trim().parse().ok()?))
        })
        .collect()
}
fn distribution(mut values: Vec<f64>) -> serde_json::Value {
    values.sort_by(f64::total_cmp);
    let p = |percent: f64| values[((values.len() - 1) as f64 * percent).round() as usize];
    json!({"avg_ms": values.iter().sum::<f64>() / values.len() as f64,
        "p50_ms": p(0.5), "p90_ms": p(0.9), "p95_ms": p(0.95), "p99_ms": p(0.99), "max_ms": p(1.0)})
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let (mode, backend, scenario) = (args[1].clone(), args[2].as_str(), args[3].as_str());
    assert!(matches!(mode.as_str(), "buffered" | "file"));
    assert!(matches!(backend, "local" | "s3"));
    assert!(matches!(scenario, "cold" | "warm"));
    let operations = std::env::var("STORAGE_AUDIT_OPERATIONS")
        .unwrap_or("12".into())
        .parse::<usize>()?;
    let rate = std::env::var("STORAGE_AUDIT_RATE")
        .unwrap_or("2".into())
        .parse::<f64>()?;
    let size = std::env::var("STORAGE_AUDIT_MIB")
        .unwrap_or("64".into())
        .parse::<u64>()?
        * 1024
        * 1024;
    assert!((1..=512 * 1024 * 1024).contains(&size) && operations > 0 && rate > 0.0);
    let root = std::env::temp_dir().join(format!("rsctf-storage-audit-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir(&root).await?;
    let store: Arc<dyn BlobStorage> = if backend == "local" {
        Arc::new(LocalBlobStorage::new(root.join("blobs")))
    } else {
        Arc::new(S3BlobStorage::try_from_env()?.expect("disposable S3 credentials"))
    };
    let mut sources = Vec::new();
    // Sparse immutable fixtures bound harness disk usage. S3 transfers remain
    // uncompressed. Each file has a unique hash, so cold requests cannot dedup.
    for index in 0..operations {
        let source = root.join(format!("source-{index}"));
        let mut file = tokio::fs::File::create(&source).await?;
        file.set_len(size).await?;
        let id = uuid::Uuid::new_v4();
        file.write_all(id.as_bytes()).await?;
        file.sync_all().await?;
        drop(file);
        let mut digest = Sha256::new();
        digest.update(id.as_bytes());
        let zeros = vec![0; 1024 * 1024];
        let mut remaining = size - 16;
        while remaining != 0 {
            let length = remaining.min(zeros.len() as u64) as usize;
            digest.update(&zeros[..length]);
            remaining -= length as u64;
        }
        let hash = hex::encode(digest.finalize());
        if scenario == "warm" {
            store
                .store_file("fixture.pcap", tokio::fs::File::open(&source).await?, size)
                .await?;
        }
        sources.push((source, hash));
    }
    println!("{}", json!({"event": "start"}));
    tokio::time::sleep(Duration::from_millis(100)).await;
    let cpu_before = usage().0;
    let io_before = io_usage();
    let start = Instant::now();
    let mut jobs = tokio::task::JoinSet::new();
    for (index, (source, hash)) in sources.iter().enumerate() {
        let scheduled = start + Duration::from_secs_f64(index as f64 / rate);
        tokio::time::sleep_until(scheduled.into()).await;
        let (source, store, mode, hash) =
            (source.clone(), store.clone(), mode.clone(), hash.clone());
        jobs.spawn(async move {
            let began = Instant::now();
            let blob = if mode == "file" {
                store
                    .store_file("fixture.pcap", tokio::fs::File::open(source).await?, size)
                    .await?
            } else {
                // Include the file-to-Vec materialization required by the old API.
                let bytes = tokio::fs::read(source).await?;
                store.store("fixture.pcap", &bytes).await?
            };
            assert_eq!(blob.hash, hash);
            assert_eq!(blob.size as u64, size);
            anyhow::Ok((
                began.elapsed().as_secs_f64() * 1000.0,
                scheduled.elapsed().as_secs_f64() * 1000.0,
            ))
        });
    }
    let mut service = Vec::new();
    let mut scheduled = Vec::new();
    while let Some(result) = jobs.join_next().await {
        let (a, b) = result??;
        service.push(a);
        scheduled.push(b);
    }
    tokio::time::sleep_until((start + Duration::from_secs_f64(operations as f64 / rate)).into())
        .await;
    let elapsed = start.elapsed().as_secs_f64();
    let usage = usage();
    let io_delta = io_usage()
        .into_iter()
        .map(|(key, value)| {
            let before = io_before[&key];
            (key, value - before)
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    println!("{}", json!({"event": "stop"}));
    tokio::time::sleep(Duration::from_millis(100)).await;
    // Every measured object is read back and hashed after the timing window.
    for (_, hash) in &sources {
        assert_eq!(store.size(hash).await?, size);
        let mut stream = store.stream_range(hash, 0..size).await?;
        let mut actual = Sha256::new();
        while let Some(bytes) = stream.try_next().await? {
            actual.update(&bytes);
        }
        assert_eq!(&hex::encode(actual.finalize()), hash);
        store.delete(hash).await?;
        assert!(!store.exists(hash).await);
    }
    tokio::fs::remove_dir_all(&root).await?;
    println!(
        "{}",
        json!({"event": "result", "mode": mode, "backend": backend,
        "scenario": scenario, "operations": operations, "rate": rate, "bytes": size,
        "elapsed_seconds": elapsed, "cpu_percent": (usage.0 - cpu_before) / elapsed * 100.0,
        "peak_rss_mib": usage.1 as f64 / 1024.0, "io": io_delta,
        "service": distribution(service), "scheduled": distribution(scheduled), "integrity_errors": 0})
    );
    Ok(())
}
