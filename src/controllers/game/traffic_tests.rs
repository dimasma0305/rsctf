use super::*;
use std::io::Cursor;

#[test]
fn capture_pages_reject_unbounded_windows() {
    assert_eq!(
        CapturePageQuery {
            skip: 4_096,
            count: 100,
        }
        .normalized()
        .unwrap(),
        (4_096, 100)
    );
    assert!(CapturePageQuery { skip: 0, count: 0 }.normalized().is_err());
    assert!(CapturePageQuery {
        skip: 0,
        count: 101
    }
    .normalized()
    .is_err());
    assert!(CapturePageQuery {
        skip: 4_097,
        count: 1
    }
    .normalized()
    .is_err());
}

#[test]
fn capture_page_metadata_advances_without_repeating_or_overrunning() {
    assert_eq!(inventory_page::next_capture_skip(0, 50, 121), Some(50));
    assert_eq!(inventory_page::next_capture_skip(100, 21, 121), None);
    assert_eq!(inventory_page::next_capture_skip(121, 0, 121), None);
}

#[test]
fn archive_scan_filters_non_pcaps() {
    let dir = std::env::temp_dir().join(format!("rsctf-capture-list-{}", Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    for name in ["one.pcap", "two.PCAP", "three.pcap"] {
        std::fs::File::create(dir.join(name)).unwrap();
    }
    std::fs::File::create(dir.join("ignore.txt")).unwrap();

    assert_eq!(list_pcaps(&dir).unwrap().len(), 3);

    std::fs::remove_dir_all(&dir).unwrap();
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
fn deployment_budget_reserves_the_full_per_export_ceiling() {
    assert_eq!(
        MAX_CAPTURE_ARCHIVE_DEPLOYMENT_BYTES,
        MAX_CAPTURE_ARCHIVE_DEPLOYMENT_JOBS * MAX_CAPTURE_ARCHIVE_BYTES as i64
    );
    assert!(CAPTURE_ARCHIVE_LEASE_SECONDS > 2 * 10);
}

#[test]
fn archive_overload_is_retryable() {
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
