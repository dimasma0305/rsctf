use super::{archive_matches_file, OperatingSystem};
use tar::{Builder, EntryType, Header};

async fn verifies(os: OperatingSystem, files: &[(&str, EntryType, &[u8])]) -> bool {
    let mut builder = Builder::new(Vec::new());
    for &(name, kind, bytes) in files {
        let mut header = Header::new_gnu();
        header.set_mode(0o600);
        header.set_entry_type(kind);
        if kind.is_symlink() || kind.is_hard_link() {
            header.set_link_name("other").unwrap();
        }
        header.set_size(bytes.len() as u64);
        builder.append_data(&mut header, name, bytes).unwrap();
    }
    archive_matches_file(
        builder.into_inner().unwrap(),
        "flag".into(),
        b"current".to_vec(),
        os,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn readback_requires_only_the_requested_regular_file() {
    let os = OperatingSystem::Linux;
    let target = ("flag", EntryType::Regular, b"current".as_slice());
    let other = ("other", EntryType::Regular, b"current".as_slice());
    assert!(verifies(os, &[target]).await);
    assert!(!verifies(os, &[]).await);
    assert!(!verifies(os, &[other]).await);
    assert!(!verifies(os, &[target, other]).await);
    assert!(!verifies(os, &[other, target]).await);
    assert!(!verifies(os, &[("sub/flag", target.1, target.2)]).await);
    assert!(!verifies(os, &[("flag", target.1, b"earlier")]).await);
    for kind in [EntryType::Directory, EntryType::Symlink, EntryType::Link] {
        assert!(!verifies(os, &[("flag", kind, b"")]).await);
    }
}

#[tokio::test]
async fn readback_uses_the_guest_filename_case() {
    let file = [("FLAG", EntryType::Regular, b"current".as_slice())];
    assert!(!verifies(OperatingSystem::Linux, &file).await);
    assert!(verifies(OperatingSystem::Windows, &file).await);
}
