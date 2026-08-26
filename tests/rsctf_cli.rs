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
    assert!(String::from_utf8_lossy(&help.stdout).contains("Usage: rsctf challenge check"));

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
