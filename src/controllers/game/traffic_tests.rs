use super::*;
use std::io::{Cursor, Read as _, Write as _};
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn blocking_work_retains_admission_after_waiter_cancellation() {
    let gate = Arc::new(tokio::sync::Semaphore::new(1));
    let permit = gate.clone().acquire_owned().await.unwrap();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (finish_tx, finish_rx) = std::sync::mpsc::channel();
    let waiter = tokio::spawn(spawn_blocking_with_permit(permit, move || {
        let _ = started_tx.send(());
        let _ = finish_rx.recv();
    }));

    started_rx.await.unwrap();
    waiter.abort();
    let _ = waiter.await;
    assert!(gate.clone().try_acquire_owned().is_err());

    finish_tx.send(()).unwrap();
    let released = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if let Ok(permit) = gate.clone().try_acquire_owned() {
                break permit;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("blocking work retained the permit after it completed");
    drop(released);
}

#[test]
fn traffic_zip_writer_streams_a_valid_archive() {
    let (sender, mut receiver) = tokio::sync::mpsc::channel::<CaptureZipChunk>(1);
    let worker = std::thread::spawn(move || {
        let writer = CaptureZipStreamWriter::new(sender);
        let mut zip = zip::ZipWriter::new_stream(writer);
        zip.start_file("capture.pcap", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"pcap-data").unwrap();
        zip.finish().unwrap().into_inner().finish().unwrap();
    });

    let mut bytes = Vec::new();
    while let Some(chunk) = receiver.blocking_recv() {
        bytes.extend_from_slice(&chunk.unwrap());
    }
    worker.join().unwrap();

    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut contents = Vec::new();
    archive
        .by_name("capture.pcap")
        .unwrap()
        .read_to_end(&mut contents)
        .unwrap();
    assert_eq!(contents, b"pcap-data");
}

#[test]
fn deployment_budget_and_heartbeat_leave_failure_margin() {
    assert_eq!(
        MAX_CAPTURE_ARCHIVE_DEPLOYMENT_BYTES,
        MAX_CAPTURE_ARCHIVE_DEPLOYMENT_JOBS * MAX_CAPTURE_ARCHIVE_BYTES as i64
    );
    assert!(CAPTURE_ARCHIVE_LEASE_SECONDS as u64 > 2 * CAPTURE_ARCHIVE_HEARTBEAT_SECONDS);
    assert!(CAPTURE_ARCHIVE_DATABASE_SECONDS < CAPTURE_ARCHIVE_HEARTBEAT_SECONDS);
}

#[test]
fn archive_overload_is_retryable() {
    use axum::response::IntoResponse as _;

    let response =
        AppError::retryable_unavailable("capture archive busy", CAPTURE_ARCHIVE_RETRY_SECONDS)
            .into_response();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response.headers()[header::RETRY_AFTER].to_str().unwrap(),
        CAPTURE_ARCHIVE_RETRY_SECONDS.to_string().as_str()
    );
}

#[tokio::test]
async fn pre_stream_owner_drop_cancels_heartbeat_owner() {
    let (completed, released) = tokio::sync::oneshot::channel();
    let (_lease_failed, lease_failure) = tokio::sync::oneshot::channel();
    let owner = CaptureArchiveLeaseOwner {
        completed: Some(completed),
        lease_failed: Some(lease_failure),
    };

    drop(owner);
    released.await.unwrap();
}

#[test]
fn pre_stream_owner_rejects_a_lost_lease() {
    let (completed, _released) = tokio::sync::oneshot::channel();
    let (lease_failed, lease_failure) = tokio::sync::oneshot::channel();
    let mut owner = CaptureArchiveLeaseOwner {
        completed: Some(completed),
        lease_failed: Some(lease_failure),
    };
    lease_failed.send(()).unwrap();

    assert!(owner.ensure_alive().is_err());
}

#[tokio::test]
async fn response_stream_retains_admission_until_disconnect() {
    let admission = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
    let permit = admission.clone().try_acquire_owned().unwrap();
    let (_sender, receiver) = tokio::sync::mpsc::channel(1);
    let (completed, released) = tokio::sync::oneshot::channel();
    let (_lease_failed, lease_failure) = tokio::sync::oneshot::channel();
    let stream = CaptureArchiveStream {
        inner: tokio_stream::wrappers::ReceiverStream::new(receiver),
        _permit: permit,
        completed: Some(completed),
        lease_failed: Box::pin(lease_failure),
        deadline: Box::pin(tokio::time::sleep(std::time::Duration::from_secs(30))),
        terminal: false,
    };

    assert!(admission.clone().try_acquire_owned().is_err());
    drop(stream);
    released.await.unwrap();
    assert!(admission.try_acquire_owned().is_ok());
}

#[tokio::test]
async fn final_buffered_archive_chunk_retains_admission_until_eof() {
    use futures::StreamExt as _;

    let admission = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
    let permit = admission.clone().try_acquire_owned().unwrap();
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    sender
        .send(Ok(bytes::Bytes::from_static(b"last-archive-byte")))
        .await
        .unwrap();
    drop(sender);
    let (completed, released) = tokio::sync::oneshot::channel();
    let (_lease_failed, lease_failure) = tokio::sync::oneshot::channel();
    let mut stream = CaptureArchiveStream {
        inner: tokio_stream::wrappers::ReceiverStream::new(receiver),
        _permit: permit,
        completed: Some(completed),
        lease_failed: Box::pin(lease_failure),
        deadline: Box::pin(tokio::time::sleep(std::time::Duration::from_secs(30))),
        terminal: false,
    };

    assert_eq!(
        stream.next().await.unwrap().unwrap(),
        bytes::Bytes::from_static(b"last-archive-byte")
    );
    assert!(admission.clone().try_acquire_owned().is_err());
    assert!(stream.next().await.is_none());
    released.await.unwrap();
    // The stream object still owns admission after the sender reaches EOF;
    // it is released only after Axum drops the completed response body.
    assert!(admission.clone().try_acquire_owned().is_err());
    drop(stream);
    assert!(admission.try_acquire_owned().is_ok());
}
