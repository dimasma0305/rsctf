use super::super::ContainerManager;
use super::*;

fn archive_header(entry_type: tar::EntryType, size: u64) -> Vec<u8> {
    let mut header = tar::Header::new_gnu();
    header.set_path("preview").unwrap();
    header.set_mode(0o600);
    header.set_entry_type(entry_type);
    header.set_size(size);
    header.set_cksum();
    header.as_bytes().to_vec()
}

#[test]
fn large_regular_file_returns_only_a_truthful_bounded_preview() {
    let limit = 16 * 1024;
    let declared = 1024 * 1024 * 1024u64;
    let mut archive = archive_header(tar::EntryType::Regular, declared);
    archive.extend(std::iter::repeat_n(b'x', limit));

    let file = parse_file_archive(&archive, limit).unwrap();
    assert_eq!(file.size, declared);
    assert_eq!(file.bytes.len(), limit);
    assert!(file.truncated);
}

#[test]
fn fifo_and_device_like_entries_are_never_opened_as_files() {
    for entry_type in [
        tar::EntryType::Fifo,
        tar::EntryType::Block,
        tar::EntryType::Char,
    ] {
        let archive = archive_header(entry_type, 0);
        assert!(matches!(
            parse_file_archive(&archive, 1024),
            Err(AppError::BadRequest(_))
        ));
    }
}

#[test]
fn docker_archive_collection_stops_at_the_memory_cap() {
    let mut output = vec![1; 8];
    assert!(append_file_archive_chunk(&mut output, &[2; 8], 12).unwrap());
    assert_eq!(output.len(), 12);
    assert_eq!(&output[8..], &[2; 4]);
}

#[tokio::test]
#[ignore = "requires a disposable scoped Docker container in RSCTF_TEST_FORENSICS_CONTAINER_ID"]
async fn real_docker_large_file_and_fifo_are_bounded_without_exec() {
    let id = std::env::var("RSCTF_TEST_FORENSICS_CONTAINER_ID")
        .expect("RSCTF_TEST_FORENSICS_CONTAINER_ID is required");
    let manager = DockerContainerManager::connect().unwrap();
    let large = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        manager.read_file(&id, "/tmp/large", 240 * 1024),
    )
    .await
    .expect("large-file archive read timed out")
    .unwrap();
    assert_eq!(large.bytes.len(), 240 * 1024);
    assert!(large.size >= 1024 * 1024);
    assert!(large.truncated);

    let fifo = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        manager.read_file(&id, "/tmp/blocked", 240 * 1024),
    )
    .await
    .expect("FIFO archive metadata read timed out");
    assert!(matches!(fifo, Err(AppError::BadRequest(_))));
}
