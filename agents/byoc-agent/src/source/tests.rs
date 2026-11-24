use super::*;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "rsctf-source-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn manifest(&self, value: &[u8]) {
        fs::write(self.0.join(REGISTRATION), value).unwrap();
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn only_registered_source_is_sampled_with_git_blob_and_exec_mode() {
    let root = Root::new();
    let mut scanner = Scanner::default();
    assert_eq!(scanner.sample(&root.0).unwrap(), Observation::NotRegistered);
    root.manifest(b"main\nextra\n");
    fs::write(root.0.join("main"), b"").unwrap();
    fs::write(root.0.join("secret"), b"must stay private").unwrap();
    let Observation::Observed { files } = scanner.sample(&root.0).unwrap() else {
        panic!()
    };
    assert_eq!(files.len(), 2);
    assert_eq!(files["extra"], None);
    let first = files["main"].as_ref().unwrap();
    assert_eq!(
        first.blob,
        "473a0f4c3be8a93681a267e3b1e9a7dcda1185436fe141f7749120a303721813"
    );
    assert!(!first.executable);
    fs::set_permissions(root.0.join("main"), fs::Permissions::from_mode(0o755)).unwrap();
    let Observation::Observed { files } = scanner.sample(&root.0).unwrap() else {
        panic!()
    };
    assert!(files["main"].as_ref().unwrap().executable);
    fs::rename(root.0.join("main"), root.0.join("extra")).unwrap();
    let Observation::Observed { files } = scanner.sample(&root.0).unwrap() else {
        panic!()
    };
    assert_eq!(files["main"], None);
    assert_eq!(files["extra"].as_ref().unwrap().blob, first.blob);
    let encoded = serde_json::to_string(&files).unwrap();
    assert!(!encoded.contains("secret"));
}

#[test]
fn registration_cannot_follow_links_or_read_special_files() {
    let root = Root::new();
    let outside = Root::new();
    fs::write(outside.0.join("private"), b"outside").unwrap();
    symlink(&outside.0, root.0.join("linked-dir")).unwrap();
    symlink(outside.0.join("private"), root.0.join("linked-file")).unwrap();
    fs::hard_link(outside.0.join("private"), root.0.join("alias")).unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(root.0.join("socket")).unwrap();
    let mut scanner = Scanner::default();
    for path in [
        "linked-dir/private",
        "linked-file",
        "alias",
        "socket",
        "../outside",
        "/etc/passwd",
        ".git/config",
        ".rsctf-files",
        "a//b",
    ] {
        root.manifest(path.as_bytes());
        assert!(scanner.sample(&root.0).is_err(), "{path}");
    }
    fs::remove_file(root.0.join(REGISTRATION)).unwrap();
    symlink(outside.0.join("private"), root.0.join(REGISTRATION)).unwrap();
    assert!(scanner.sample(&root.0).is_err());
}

#[test]
fn registration_and_file_budgets_reject_partial_snapshots() {
    let root = Root::new();
    let mut scanner = Scanner::default();
    root.manifest(b"app\n");
    fs::write(root.0.join("app"), b"source").unwrap();
    scanner.sample(&root.0).unwrap();
    let large = File::create(root.0.join("large")).unwrap();
    large.set_len(MAX_FILE + 1).unwrap();
    for list in [
        b"app\nlarge".to_vec(),
        b"app\napp".to_vec(),
        vec![b'x'; MAX_MANIFEST as usize + 1],
        (0..=MAX_FILES)
            .map(|i| format!("file-{i}\n"))
            .collect::<String>()
            .into_bytes(),
    ] {
        root.manifest(&list);
        assert!(scanner.sample(&root.0).is_err());
        assert!(scanner.previous.contains_key("app"));
    }
    root.manifest(b"");
    assert_eq!(scanner.sample(&root.0).unwrap(), Observation::NotRegistered);
    assert!(scanner.previous.is_empty());
}

#[test]
fn steady_state_reuses_fingerprints_and_only_hashes_changed_files() {
    let root = Root::new();
    let names: Vec<_> = (0..64).map(|i| format!("file-{i}")).collect();
    root.manifest(names.join("\n").as_bytes());
    let contents = vec![0x55; 256 * 1024];
    for name in &names {
        fs::write(root.0.join(name), &contents).unwrap();
    }
    let mut scanner = Scanner::default();
    let t = std::time::Instant::now();
    scanner.sample(&root.0).unwrap();
    let cold = t.elapsed();
    assert_eq!(scanner.hashed_bytes, 16 * 1024 * 1024);
    let t = std::time::Instant::now();
    for _ in 0..25 {
        scanner.sample(&root.0).unwrap();
    }
    let warm = t.elapsed() / 25;
    assert_eq!(scanner.hashed_bytes, 16 * 1024 * 1024);
    fs::write(root.0.join(&names[0]), b"changed").unwrap();
    scanner.sample(&root.0).unwrap();
    assert_eq!(scanner.hashed_bytes, 16 * 1024 * 1024 + 7);
    println!("64 registered files, 16 MiB: first scan {cold:?}, mean unchanged scan {warm:?} over 25 samples; 0 payload bytes rehashed until an edit, then 7 bytes.");
}

#[test]
fn automatic_source_follows_edits_without_a_manifest_and_respects_overrides() {
    let root = Root::new();
    let outside = Root::new();
    let mut scanner = Scanner {
        auto_discover: true,
        ..Default::default()
    };
    let read = |scanner: &mut Scanner| match scanner.sample(&root.0).unwrap() {
        Observation::Observed { files } => files,
        other => panic!("expected an observed workspace, got {other:?}"),
    };
    assert!(read(&mut scanner).is_empty());
    for directory in ["src", ".git", "node_modules", "secrets"] {
        fs::create_dir(root.0.join(directory)).unwrap();
        fs::write(root.0.join(directory).join("main.py"), b"first").unwrap();
    }
    fs::write(root.0.join(".env"), b"private").unwrap();
    fs::write(root.0.join("rsctf-ad.conf"), b"private").unwrap();
    fs::write(outside.0.join("escaped.py"), b"outside").unwrap();
    symlink(&outside.0, root.0.join("lib")).unwrap();
    symlink(outside.0.join("escaped.py"), root.0.join("alias.py")).unwrap();
    let initial = read(&mut scanner);
    assert_eq!(initial.keys().collect::<Vec<_>>(), ["src/main.py"]);
    fs::write(root.0.join("src/main.py"), b"next").unwrap();
    assert_ne!(initial, read(&mut scanner));
    fs::remove_file(root.0.join("src/main.py")).unwrap();
    assert!(read(&mut scanner).is_empty());
    fs::write(root.0.join("Dockerfile"), b"FROM scratch").unwrap();
    root.manifest(b"only-this\n");
    assert_eq!(
        read(&mut scanner),
        BTreeMap::from([("only-this".into(), None)])
    );
    root.manifest(b"");
    assert_eq!(scanner.sample(&root.0).unwrap(), Observation::NotRegistered);
}

#[test]
fn automatic_source_rejects_partial_snapshots_when_over_budget() {
    let root = Root::new();
    let mut scanner = Scanner {
        auto_discover: true,
        ..Default::default()
    };
    for n in 0..=MAX_FILES {
        fs::write(root.0.join(format!("{n}.rs")), b"").unwrap();
    }
    assert!(scanner.sample(&root.0).is_err());
    assert!(scanner.previous.is_empty());
}
