use super::*;
use object_store::{memory::InMemory, path::Path, ObjectStoreExt, PutResult, UploadPart};
use std::sync::Arc;
use tokio::sync::Notify;

#[derive(Clone, Copy, Debug)]
enum Fault {
    Part,
    Complete,
    Block,
    None,
}

#[derive(Debug)]
struct ControlledUpload {
    inner: Box<dyn MultipartUpload>,
    fault: Fault,
    started: Arc<Notify>,
    aborted: Arc<Notify>,
}

fn unavailable() -> object_store::Error {
    object_store::Error::Generic {
        store: "upload-test",
        source: std::io::Error::other("simulated store interruption").into(),
    }
}

#[async_trait::async_trait]
impl MultipartUpload for ControlledUpload {
    fn put_part(&mut self, bytes: PutPayload) -> UploadPart {
        let started = self.started.clone();
        match self.fault {
            Fault::Part => Box::pin(async { Err(unavailable()) }),
            Fault::Block => Box::pin(async move {
                started.notify_one();
                std::future::pending().await
            }),
            _ => self.inner.put_part(bytes),
        }
    }
    async fn complete(&mut self) -> object_store::Result<PutResult> {
        if matches!(self.fault, Fault::Complete) {
            return Err(unavailable());
        }
        self.inner.complete().await
    }
    async fn abort(&mut self) -> object_store::Result<()> {
        let result = self.inner.abort().await;
        self.aborted.notify_one();
        result
    }
}

#[tokio::test]
async fn interrupted_changed_and_cancelled_uploads_never_publish_partial_objects() {
    let store = InMemory::new();
    let source = std::env::temp_dir().join(format!("multipart-{}", uuid::Uuid::new_v4()));
    for fault in [Fault::Part, Fault::Complete, Fault::Block, Fault::None] {
        tokio::fs::write(&source, b"original").await.unwrap();
        let input = FileUpload::prepare(tokio::fs::File::open(&source).await.unwrap(), 16)
            .await
            .unwrap();
        let key = Path::from(input.hash.clone());
        let started = Arc::new(Notify::new());
        let aborted = Arc::new(Notify::new());
        let controlled = ControlledUpload {
            inner: store.put_multipart(&key).await.unwrap(),
            fault,
            started: started.clone(),
            aborted: aborted.clone(),
        };
        if matches!(fault, Fault::None) {
            // The input passed its first hash, then changed while the remote
            // multipart session was being opened. It must never be completed.
            tokio::fs::write(&source, b"modified").await.unwrap();
        }
        let task = tokio::spawn(async move {
            publish_multipart(Box::new(controlled), input, "capture.pcap").await
        });
        if matches!(fault, Fault::Block) {
            tokio::time::timeout(Duration::from_secs(1), started.notified())
                .await
                .unwrap();
            assert!(
                store.head(&key).await.is_err(),
                "incomplete bytes became visible"
            );
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        } else {
            assert!(task.await.unwrap().is_err());
        }
        tokio::time::timeout(Duration::from_secs(1), aborted.notified())
            .await
            .unwrap();
        assert!(
            store.head(&key).await.is_err(),
            "failed upload left a visible object"
        );
    }
    tokio::fs::remove_file(source).await.unwrap();
}
