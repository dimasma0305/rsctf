use super::*;
use std::io::Write;

fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, data) in entries {
        zip.start_file(*name, options).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn limits() -> ArchiveLimits {
    ArchiveLimits {
        entries: 2,
        file_bytes: 1_024,
        total_bytes: 1_024,
        compression_ratio: 20,
        path_components: 4,
    }
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("rsctf-{tag}-{}", Uuid::new_v4()))
}

#[test]
fn zip_limits_reject_entry_and_expansion_bombs() {
    let dir = temp_dir("zip-limits");
    std::fs::create_dir_all(&dir).unwrap();

    let too_many = archive(&[("a", b"a"), ("b", b"b"), ("c", b"c")]);
    assert!(extract_zip_with_limits(&too_many, &dir, limits()).is_err());

    let large = vec![b'x'; 1_025];
    let expanded = archive(&[("large", &large)]);
    assert!(extract_zip_with_limits(&expanded, &dir, limits()).is_err());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn zip_slip_entries_are_rejected_before_extraction() {
    let dir = temp_dir("zip-slip");
    std::fs::create_dir_all(&dir).unwrap();
    let outside_name = format!("rsctf-zip-slip-outside-{}", Uuid::new_v4());
    let outside = dir.parent().unwrap().join(&outside_name);
    let escape = format!("../{outside_name}");
    let bytes = archive(&[(&escape, b"nope"), ("ok/file", b"ok")]);

    assert!(extract_zip_with_limits(&bytes, &dir, limits()).is_err());
    assert!(!outside.exists());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn zip_noncanonical_entry_aliases_are_rejected() {
    let dir = temp_dir("zip-noncanonical");
    std::fs::create_dir_all(&dir).unwrap();
    for name in [
        "nested/../file",
        "nested/./file",
        "nested//file",
        "nested\\file",
    ] {
        let bytes = archive(&[(name, b"nope")]);
        assert!(
            extract_zip_with_limits(&bytes, &dir, limits()).is_err(),
            "noncanonical ZIP name was accepted: {name}"
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn repository_subpaths_are_canonical_descendants() {
    let root = temp_dir("repo-subpath");
    std::fs::create_dir_all(root.join("inside")).unwrap();

    let rel = validate_subpath(Some("inside")).unwrap().unwrap();
    assert_eq!(
        resolve_subpath(&root, Some(&rel)).unwrap(),
        std::fs::canonicalize(root.join("inside")).unwrap()
    );
    assert!(validate_subpath(Some("../outside")).is_err());
    assert!(resolve_subpath(&root, Some(std::path::Path::new("missing"))).is_err());
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn repository_subpaths_reject_symlink_escapes() {
    use std::os::unix::fs::symlink;

    let root = temp_dir("repo-subpath-root");
    let outside = temp_dir("repo-subpath-outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    symlink(&outside, root.join("link")).unwrap();

    let rel = validate_subpath(Some("link")).unwrap().unwrap();
    assert!(resolve_subpath(&root, Some(&rel)).is_err());
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(outside);
}
