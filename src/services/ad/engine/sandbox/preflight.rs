//! Startup proof that the checker launcher can install its real confinement.
//!
//! The parent creates one readable-but-unlisted canary and re-execs rsctf
//! through the same launcher used for official checkers. The confined child
//! proves Landlock denied that canary, seccomp filter mode is active, privilege
//! dropping stuck, its scratch directory remains writable, and the exact null
//! device required by supported checker libraries can be opened.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{checker_uid_pool, make_scratch, sandbox_write_paths, LAUNCH_ARG, SANDBOX_FAIL_EXIT};

/// Internal argv marker executed only after the sandbox launcher has confined
/// and re-execed this binary.
pub const PREFLIGHT_ARG: &str = "__checker_confinement_probe";

const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(5);
const PROBE_FAIL_EXIT: i32 = 4;

struct PreflightArtifacts {
    scratch: String,
    denied_dir: Option<PathBuf>,
}

impl Drop for PreflightArtifacts {
    fn drop(&mut self) {
        // The child owns the mode-0700 scratch directory. The parent retains
        // CAP_CHOWN (not CAP_DAC_OVERRIDE), so reclaim the directory before
        // traversing it during cleanup.
        let _ = nix::unistd::chown(
            self.scratch.as_str(),
            Some(nix::unistd::geteuid()),
            Some(nix::unistd::getegid()),
        );
        let _ = std::fs::remove_dir_all(&self.scratch);
        if let Some(denied_dir) = &self.denied_dir {
            let _ = std::fs::remove_dir_all(denied_dir);
        }
    }
}

/// Run before any round worker or topology heartbeat starts. This deliberately
/// leases no target egress rule: the probe performs no network operation and
/// remains behind the UID range's default-deny OUTPUT chain.
pub async fn preflight_checker_confinement() -> Result<(), String> {
    let pool = checker_uid_pool()?;
    let uid_lease = tokio::time::timeout(PREFLIGHT_TIMEOUT, pool.acquire())
        .await
        .map_err(|_| "timed out leasing a checker UID for confinement preflight".to_string())?;
    let uid = uid_lease.uid;
    let scratch = make_scratch(uid).map_err(|error| format!("create probe scratch: {error}"))?;
    let mut artifacts = PreflightArtifacts {
        scratch,
        denied_dir: None,
    };
    let (denied_dir, denied_file) = make_denied_canary()?;
    artifacts.denied_dir = Some(denied_dir);

    let self_exe = std::env::current_exe()
        .map_err(|error| format!("resolve checker launcher executable: {error}"))?;
    let self_exe = self_exe
        .to_str()
        .ok_or_else(|| "checker launcher executable path is not UTF-8".to_string())?;
    let cpu_seconds = PREFLIGHT_TIMEOUT.as_secs() + 2;
    let mut command = tokio::process::Command::new(self_exe);
    command
        .arg(LAUNCH_ARG)
        .arg(uid.to_string())
        // The preflight re-execs the full rsctf binary, whose mapped debug or
        // instrumented image can be much larger than a Python checker. This is
        // an address-space ceiling, not allocated startup memory.
        .arg("1024")
        .arg(cpu_seconds.to_string())
        .arg(self_exe)
        .arg(PREFLIGHT_ARG)
        .arg(uid.to_string())
        .arg(&artifacts.scratch)
        .arg(&denied_file)
        .env_clear()
        .env("SBX_EXEC", preflight_exec_paths(self_exe).join(":"))
        .env("SBX_READ", "")
        .env("SBX_WRITE", sandbox_write_paths(&artifacts.scratch))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let output = tokio::time::timeout(PREFLIGHT_TIMEOUT, command.output())
        .await
        .map_err(|_| "checker confinement preflight timed out".to_string())?
        .map_err(|error| format!("spawn checker confinement preflight: {error}"))?;
    if !output.status.success() {
        let code = output.status.code().unwrap_or(SANDBOX_FAIL_EXIT);
        let diagnostic = bounded_stderr(&output.stderr);
        return Err(if diagnostic.is_empty() {
            format!("checker confinement child exited {code}")
        } else {
            format!("checker confinement child exited {code}: {diagnostic}")
        });
    }

    drop(uid_lease);
    drop(artifacts);
    tracing::info!(uid, "checker Landlock/seccomp confinement preflight passed");
    Ok(())
}

fn preflight_exec_paths(self_exe: &str) -> Vec<String> {
    [
        self_exe,
        "/usr/lib",
        "/usr/lib64",
        "/lib",
        "/lib64",
        "/etc/ld.so.cache",
    ]
    .into_iter()
    .filter(|path| Path::new(path).exists())
    .map(str::to_string)
    .collect()
}

fn make_denied_canary() -> Result<(PathBuf, PathBuf), String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);

    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let base = std::env::var("RSCTF_CHECKER_SCRATCH").unwrap_or_else(|_| "/tmp".to_string());
    let directory = Path::new(&base).join(format!(
        "rsctf-chk-denied-{}-{sequence}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    let result = (|| {
        std::fs::create_dir_all(&directory)
            .map_err(|error| format!("create denied probe canary directory: {error}"))?;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755))
            .map_err(|error| format!("make denied probe canary traversable: {error}"))?;
        let file = directory.join("readable-without-landlock");
        std::fs::write(&file, b"landlock-canary")
            .map_err(|error| format!("write denied probe canary: {error}"))?;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644))
            .map_err(|error| format!("make denied probe canary readable: {error}"))?;
        Ok((directory.clone(), file))
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&directory);
    }
    result
}

fn bounded_stderr(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .trim()
        .chars()
        .take(512)
        .collect()
}

/// Minimal child-side proof. It runs after the launcher has already installed
/// Landlock, no-new-privileges, uid/gid isolation, rlimits, and seccomp.
pub fn confinement_probe_main() -> ! {
    fn fail(message: &str) -> ! {
        eprintln!("SANDBOX_PREFLIGHT_FAIL: {message}");
        std::process::exit(PROBE_FAIL_EXIT);
    }

    let arguments: Vec<String> = std::env::args().collect();
    if arguments.len() != 5 {
        fail("unexpected arguments");
    }
    let expected_uid = arguments[2]
        .parse::<u32>()
        .unwrap_or_else(|_| fail("invalid expected uid"));
    if nix::unistd::geteuid().as_raw() != expected_uid
        || nix::unistd::getegid().as_raw() != expected_uid
    {
        fail("checker uid/gid drop did not stick");
    }
    let no_new_privileges = unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) };
    if no_new_privileges != 1 {
        fail("no_new_privs is not active");
    }
    let seccomp_mode = unsafe { libc::prctl(libc::PR_GET_SECCOMP, 0, 0, 0, 0) };
    if seccomp_mode != libc::SECCOMP_MODE_FILTER as libc::c_int {
        fail("seccomp filter mode is not active");
    }

    let scratch_probe = Path::new(&arguments[3]).join("allowed-write");
    std::fs::write(&scratch_probe, b"ok")
        .unwrap_or_else(|_| fail("allowed scratch is not writable"));
    let _ = std::fs::remove_file(scratch_probe);
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
        .unwrap_or_else(|_| fail("allowed null device is not readable and writable"));
    match std::fs::read(&arguments[4]) {
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {}
        Err(_) => fail("denied canary failed for a reason other than Landlock permission"),
        Ok(_) => fail("Landlock allowed an unlisted readable canary"),
    }
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::{bounded_stderr, preflight_exec_paths};

    #[test]
    fn preflight_exec_allowlist_is_narrow_and_contains_the_program() {
        let paths = preflight_exec_paths("/bin/true");
        assert!(paths.iter().any(|path| path == "/bin/true"));
        assert!(!paths.iter().any(|path| path == "/tmp"));
        assert!(!paths.iter().any(|path| path == "/etc"));
    }

    #[test]
    fn child_diagnostic_is_bounded() {
        let diagnostic = bounded_stderr(&vec![b'x'; 1024]);
        assert_eq!(diagnostic.len(), 512);
    }
}
