use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use rsctf::services::ad_engine::sandbox::{LAUNCH_ARG, PREFLIGHT_ARG};

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("rsctf-checker-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create checker sandbox fixture");
        Self { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn make_canary(fixture: &Fixture) -> PathBuf {
    std::fs::set_permissions(&fixture.root, std::fs::Permissions::from_mode(0o755))
        .expect("make fixture traversable");
    let canary = fixture.path("readable-canary");
    std::fs::write(&canary, b"readable without Landlock").expect("write canary");
    std::fs::set_permissions(&canary, std::fs::Permissions::from_mode(0o644))
        .expect("make canary readable");
    canary
}

fn exec_allowlist(binary: &str) -> String {
    [
        binary,
        "/usr/lib",
        "/usr/lib64",
        "/lib",
        "/lib64",
        "/etc/ld.so.cache",
    ]
    .into_iter()
    .filter(|path| Path::new(path).exists())
    .collect::<Vec<_>>()
    .join(":")
}

#[test]
fn confinement_probe_rejects_an_unconfined_invocation() {
    let fixture = Fixture::new("unconfined");
    let scratch = fixture.path("scratch");
    std::fs::create_dir(&scratch).expect("create scratch");
    let canary = make_canary(&fixture);
    let binary = env!("CARGO_BIN_EXE_rsctf");
    let output = Command::new(binary)
        .arg(PREFLIGHT_ARG)
        .arg(nix::unistd::geteuid().as_raw().to_string())
        .arg(&scratch)
        .arg(&canary)
        .output()
        .expect("run unconfined probe");
    assert!(!output.status.success());
}

#[test]
fn real_launcher_installs_landlock_and_seccomp_before_probe_exec() {
    // Production checker roles require the identity-changing capabilities. A
    // non-root developer test still exercises the fail-closed probe above.
    if !nix::unistd::geteuid().is_root() {
        return;
    }

    let fixture = Fixture::new("confined");
    let scratch = fixture.path("scratch");
    std::fs::create_dir(&scratch).expect("create scratch");
    std::fs::set_permissions(&scratch, std::fs::Permissions::from_mode(0o700))
        .expect("protect scratch");
    let checker_uid = 60_000;
    nix::unistd::chown(
        &scratch,
        Some(nix::unistd::Uid::from_raw(checker_uid)),
        Some(nix::unistd::Gid::from_raw(checker_uid)),
    )
    .expect("assign scratch to checker identity");
    let canary = make_canary(&fixture);
    // Cargo may place the test binary below a mode-0700 home directory. Copy
    // it into the traversable fixture so ordinary DAC does not masquerade as
    // a Landlock failure after the launcher drops to the checker UID.
    let source_binary = env!("CARGO_BIN_EXE_rsctf");
    let fixture_binary = fixture.path("rsctf");
    std::fs::copy(source_binary, &fixture_binary).expect("copy rsctf probe binary");
    std::fs::set_permissions(&fixture_binary, std::fs::Permissions::from_mode(0o755))
        .expect("make probe binary executable");
    let binary = fixture_binary.to_str().expect("UTF-8 fixture path");
    let write_allowlist = format!("{}:/dev/null", scratch.display());
    let output = Command::new(binary)
        .arg(LAUNCH_ARG)
        .arg(checker_uid.to_string())
        .arg("1024")
        .arg("3")
        .arg(binary)
        .arg(PREFLIGHT_ARG)
        .arg(checker_uid.to_string())
        .arg(&scratch)
        .arg(&canary)
        .env_clear()
        .env("SBX_EXEC", exec_allowlist(binary))
        .env("SBX_READ", "")
        .env("SBX_WRITE", write_allowlist)
        .output()
        .expect("run checker launcher preflight");

    assert!(
        output.status.success(),
        "launcher preflight failed with {:?}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}
