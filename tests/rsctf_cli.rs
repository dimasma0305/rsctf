use std::process::Command;

fn rsctf() -> Command {
    Command::new(env!("CARGO_BIN_EXE_rsctf"))
}

fn test_root() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("rsctf-main-cli-{}", uuid::Uuid::new_v4().simple()))
}

fn write(path: &std::path::Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

#[test]
fn challenge_help_and_version_do_not_start_the_server() {
    let help = rsctf()
        .args(["challenge", "check", "--help"])
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("rsctf challenge check"));

    let version = rsctf()
        .args(["challenge", "check", "--version"])
        .output()
        .unwrap();
    assert!(version.status.success());
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        format!("rsctf {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn challenge_check_uses_the_main_binary_without_runtime_configuration() {
    let root = test_root();
    write(
        &root.join(".gzevent"),
        "title: Main CLI fixture\nhidden: true\n",
    );
    write(
        &root.join("challenges/Misc/example/challenge.yaml"),
        "name: Example\ntype: StaticAttachment\ncategory: Misc\nflags:\n  - rsctf{fixture}\n",
    );
    write(
        &root.join("challenges/Misc/example/dist/readme.txt"),
        "fixture\n",
    );

    let output = rsctf()
        .args(["challenge", "check", "--deny-warnings"])
        .arg(&root)
        .env_clear()
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout)
        .contains("checked 1 event(s) and 1 challenge(s): 0 error(s), 0 warning(s)"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn challenge_check_returns_usage_status_for_unknown_options() {
    let status = rsctf()
        .args(["challenge", "check", "--unknown"])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(2));
}

#[test]
fn challenge_matrix_is_emitted_by_the_main_binary() {
    let root = test_root();
    write(
        &root.join(".gzevent"),
        "title: Matrix fixture\nhidden: true\n",
    );
    write(
        &root.join("challenges/Jeopardy/Web/example/challenge.yaml"),
        "name: Example service\ntype: StaticContainer\ncategory: Web\nflags:\n  - rsctf{fixture}\ncontainer:\n  exposePort: 8080\n",
    );
    write(
        &root.join("challenges/Jeopardy/Web/example/src/Dockerfile"),
        "FROM scratch\n",
    );

    let output = rsctf()
        .args(["challenge", "matrix"])
        .arg(&root)
        .env_clear()
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let matrix: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        matrix,
        serde_json::json!({
            "include": [{
                "name": "Jeopardy/Web/example",
                "context": "challenges/Jeopardy/Web/example/src",
                "tag": "jeopardy-web-example",
                "kind": "service",
            }]
        })
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn challenge_matrix_refuses_an_invalid_repository() {
    let root = test_root();
    std::fs::create_dir_all(&root).unwrap();

    let output = rsctf()
        .args(["challenge", "matrix"])
        .arg(&root)
        .env_clear()
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stdout)
        .contains("repository contains no .gzevent manifest"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn patch_preview_compares_two_archives_locally_and_rejects_invalid_input() {
    use std::io::Write;
    let dir = test_root();
    std::fs::create_dir(&dir).unwrap();
    let current = dir.join("current.zip");
    let candidate = dir.join("candidate.zip");
    for path in [&current, &candidate] {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("Dockerfile", options).unwrap();
        zip.write_all(b"FROM scratch\n").unwrap();
        if path == &candidate {
            zip.start_file("src/config", options.unix_permissions(0o755))
                .unwrap();
            zip.write_all(b"private source").unwrap();
        }
        zip.finish().unwrap();
    }
    let preview = || {
        rsctf()
            .args(["challenge", "patch-preview"])
            .arg(&current)
            .arg(&candidate)
            .env_clear()
            .output()
            .unwrap()
    };
    let result = preview();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(
        report["changes"],
        serde_json::json!([{"path":"src/config","kind":"Added"}])
    );
    assert_ne!(report["currentTree"], report["candidateTree"]);
    let context = |tree: &str| {
        rsctf()
            .args(["challenge", "patch-context"])
            .arg(&candidate)
            .arg(tree)
            .env_clear()
            .output()
            .unwrap()
    };
    let candidate_tree = report["candidateTree"].as_str().unwrap();
    let exported = context(candidate_tree);
    assert!(exported.status.success(), "{:?}", exported.stderr);
    let mut tar = tar::Archive::new(exported.stdout.as_slice());
    let mut entries = tar.entries().unwrap();
    let dockerfile = entries.next().unwrap().unwrap();
    assert_eq!(dockerfile.path_bytes().as_ref(), b"Dockerfile");
    drop(dockerfile);
    let mut config = entries.next().unwrap().unwrap();
    assert_eq!(config.path_bytes().as_ref(), b"src/config");
    assert_eq!(config.header().mode().unwrap(), 0o755);
    let mut content = String::new();
    std::io::Read::read_to_string(&mut config, &mut content).unwrap();
    assert_eq!(content, "private source");
    assert!(entries.next().is_none());
    let stale = context(report["currentTree"].as_str().unwrap());
    assert!(!stale.status.success() && stale.stdout.is_empty());
    // The ZIP writer enforces unique names. Rename the second member in both
    // headers to model archives written by clients that permit duplicate paths.
    let mut bytes = std::fs::read(&candidate).unwrap();
    let offsets: Vec<_> = bytes
        .windows(b"src/config".len())
        .enumerate()
        .filter_map(|(index, name)| (name == b"src/config").then_some(index))
        .collect();
    assert_eq!(offsets.len(), 2);
    for offset in offsets {
        bytes[offset..offset + b"Dockerfile".len()].copy_from_slice(b"Dockerfile");
    }
    std::fs::write(&candidate, bytes).unwrap();
    let duplicate = preview();
    let rejected = context(candidate_tree);
    assert!(!rejected.status.success() && rejected.stdout.is_empty());
    assert_eq!(duplicate.status.code(), Some(1));
    assert!(duplicate.stdout.is_empty());
    assert!(String::from_utf8_lossy(&duplicate.stderr).contains("duplicate"));
    std::fs::write(&candidate, b"invalid ZIP").unwrap();
    let invalid = preview();
    assert_eq!(invalid.status.code(), Some(1));
    assert!(invalid.stdout.is_empty());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("candidate archive"));
    std::fs::remove_dir_all(dir).unwrap();
}
