//! In-process-orchestrated A&D checker sandbox — replaces the per-check Docker
//! container. The checker runs as an unprivileged subprocess confined by Landlock
//! (filesystem), a seccomp denylist (kernel attack surface), a dropped uid,
//! `PR_SET_NO_NEW_PRIVS`, rlimits (memory/CPU/procs/file size), a stripped
//! environment (only the `RSCTF_*` contract), and a wall-clock timeout.
//!
//! rsctf re-execs ITSELF as the sandbox launcher (`__checker_sandbox …`): the
//! launcher runs in a fresh single-threaded process, so building the Landlock /
//! seccomp rules (which allocate) is safe — unlike a post-fork `pre_exec` in the
//! multi-threaded server. The launcher confines itself, then `execve`s the
//! checker. **Fail closed**: any confinement step that fails exits 3
//! (`InternalError`) and NEVER execs the checker unconfined.

use std::collections::BTreeMap;
use std::ffi::CString;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use ipnet::IpNet;

mod preflight;
pub use preflight::{confinement_probe_main, preflight_checker_confinement, PREFLIGHT_ARG};

/// Exit code the launcher uses when confinement can't be established — mapped to
/// `AdCheckStatus::InternalError` (an infrastructure verdict).
pub const SANDBOX_FAIL_EXIT: i32 = 3;

/// The magic first arg that turns an rsctf invocation into the sandbox launcher.
pub const LAUNCH_ARG: &str = "__checker_sandbox";

const DEFAULT_CHECKER_UID_BASE: u32 = 60_000;
const DEFAULT_CHECKER_PROCESS_BUDGET: u32 = 32;
const MAX_CHECKER_PROCESS_BUDGET: u32 = 256;
const CHECKER_EGRESS_CHAIN: &str = "RSCTF_CHECKER_EGRESS";
const FIREWALL_LOCK_WAIT_SECONDS: &str = "5";

/// Checker leases mutate one shared netfilter chain. Serialize their short
/// command sequences in-process so a checker burst does not fill the kernel's
/// xtables wait queue and time out otherwise valid fail-closed rule checks.
/// VPN reconciliation remains a separate process-wide caller; `-w` covers
/// that bounded cross-component contention.
fn checker_firewall_command_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn compatible_path_access(
    requested: landlock::BitFlags<landlock::AccessFs>,
    abi: landlock::ABI,
    is_directory: bool,
) -> landlock::BitFlags<landlock::AccessFs> {
    if is_directory {
        requested
    } else {
        // Directory-only rights (ReadDir, MakeReg, Refer, …) make rust-landlock
        // trim a file rule in best-effort mode and report the whole ruleset as
        // only partially enforced. Ask the kernel for exactly the rights that
        // can apply to a non-directory inode instead.
        requested & landlock::AccessFs::from_file(abi)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CheckerUidRange {
    base: u32,
    count: u32,
}

impl CheckerUidRange {
    fn end(self) -> u32 {
        self.base + self.count - 1
    }

    fn owner_match(self) -> String {
        if self.count == 1 {
            self.base.to_string()
        } else {
            format!("{}-{}", self.base, self.end())
        }
    }
}

/// A small reserved UID range gives every live checker tree a distinct kernel
/// identity. Besides preventing sibling signals, this makes `RLIMIT_NPROC`
/// apply to one checker tree rather than to every concurrent checker. The pool
/// is also the process-wide custom-checker concurrency bound.
fn configured_checker_uid_range() -> Result<CheckerUidRange, String> {
    let legacy_base = std::env::var("RSCTF_CHECKER_UID").ok();
    let explicit_base = std::env::var("RSCTF_CHECKER_UID_BASE").ok();
    let base = explicit_base
        .as_deref()
        .or(legacy_base.as_deref())
        .map(str::parse::<u32>)
        .transpose()
        .map_err(|_| "RSCTF_CHECKER_UID_BASE must be an integer".to_string())?
        .unwrap_or(DEFAULT_CHECKER_UID_BASE);
    // Preserve a safe upgrade path for the old singleton UID setting by
    // serializing checker processes until the operator reserves a range.
    let default_count = if explicit_base.is_none() && legacy_base.is_some() {
        1
    } else {
        DEFAULT_CHECKER_PROCESS_BUDGET
    };
    let count = std::env::var("RSCTF_CHECKER_PROCESS_BUDGET")
        .ok()
        .map(|value| value.parse::<u32>())
        .transpose()
        .map_err(|_| "RSCTF_CHECKER_PROCESS_BUDGET must be an integer".to_string())?
        .unwrap_or(default_count);
    if base == 0 {
        return Err("checker UID range cannot include root".to_string());
    }
    if count == 0 || count > MAX_CHECKER_PROCESS_BUDGET {
        return Err(format!(
            "RSCTF_CHECKER_PROCESS_BUDGET must be between 1 and {MAX_CHECKER_PROCESS_BUDGET}"
        ));
    }
    let end = base
        .checked_add(count - 1)
        .ok_or_else(|| "checker UID range overflows".to_string())?;
    if end > 65_534 {
        return Err("checker UID range must end at or below 65534".to_string());
    }
    let runtime_uid = nix::unistd::geteuid().as_raw();
    if (base..=end).contains(&runtime_uid) {
        return Err(format!(
            "checker UID range {base}-{end} overlaps the rsctf runtime UID {runtime_uid}"
        ));
    }
    Ok(CheckerUidRange { base, count })
}

struct CheckerUidPool {
    range: CheckerUidRange,
    available: Mutex<Vec<u32>>,
    changed: tokio::sync::Notify,
}

impl CheckerUidPool {
    fn new(range: CheckerUidRange) -> Self {
        Self {
            range,
            available: Mutex::new((range.base..=range.end()).rev().collect()),
            changed: tokio::sync::Notify::new(),
        }
    }

    async fn acquire(&'static self) -> CheckerUidLease {
        loop {
            // Register before inspecting the pool so a release between the
            // check and await cannot be lost.
            let changed = self.changed.notified();
            if let Some(uid) = self
                .available
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .pop()
            {
                return CheckerUidLease { uid, pool: self };
            }
            changed.await;
        }
    }
}

struct CheckerUidLease {
    uid: u32,
    pool: &'static CheckerUidPool,
}

impl Drop for CheckerUidLease {
    fn drop(&mut self) {
        self.pool
            .available
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(self.uid);
        self.pool.changed.notify_one();
    }
}

fn checker_uid_pool() -> Result<&'static CheckerUidPool, String> {
    static POOL: OnceLock<CheckerUidPool> = OnceLock::new();
    let configured = configured_checker_uid_range()?;
    if let Some(pool) = POOL.get() {
        return if pool.range == configured {
            Ok(pool)
        } else {
            Err("checker UID configuration changed after startup".to_string())
        };
    }
    let _ = POOL.set(CheckerUidPool::new(configured));
    let pool = POOL.get().expect("checker UID pool initialized");
    if pool.range == configured {
        Ok(pool)
    } else {
        Err("checker UID configuration raced with different values".to_string())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Launcher (runs in the re-exec'd child; never returns except by exec/exit)
// ─────────────────────────────────────────────────────────────────────────────

/// Entry point for `rsctf __checker_sandbox <uid> <mem_mb> <cpu_s> <program> [args…]`.
/// Reads `SBX_READ`/`SBX_EXEC` (colon-separated allowed paths) from the env,
/// confines this process, and execs `program`. Diverges (exec or `exit`).
pub fn launcher_main() -> ! {
    use landlock::{
        Access, AccessFs, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset, RulesetAttr,
        RulesetCreatedAttr, RulesetStatus, ABI,
    };
    use nix::sys::resource::{setrlimit, Resource};
    use nix::unistd::{execve, setgid, setgroups, setuid, Gid, Uid};

    fn die(m: &str) -> ! {
        eprintln!("SANDBOX_FAIL: {m}");
        std::process::exit(SANDBOX_FAIL_EXIT);
    }

    let a: Vec<String> = std::env::args().collect();
    // a[0]=rsctf, a[1]=__checker_sandbox, a[2]=uid, a[3]=mem_mb, a[4]=cpu_s, a[5]=program, a[6..]=args
    if a.len() < 6 {
        die("launcher: too few args");
    }
    let uid: u32 = a[2].parse().unwrap_or_else(|_| die("bad uid"));
    let mem_mb: u64 = a[3].parse().unwrap_or_else(|_| die("bad mem"));
    let cpu_s: u64 = a[4].parse().unwrap_or_else(|_| die("bad cpu"));
    let program = a[5].clone();

    // 1. Build the Landlock ruleset while still privileged (open the path fds).
    let abi = ABI::V3;
    let read = AccessFs::from_read(abi);
    let read_exec = AccessFs::from_read(abi) | AccessFs::Execute;
    let mut rs = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::from_all(abi))
        .unwrap_or_else(|_| die("handle_access"))
        .create()
        .unwrap_or_else(|_| die("create ruleset"));
    for p in std::env::var("SBX_EXEC")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
    {
        let Ok(metadata) = std::fs::metadata(p) else {
            continue;
        };
        let fd = PathFd::new(p).unwrap_or_else(|_| die("open exec rule path"));
        let access = compatible_path_access(read_exec, abi, metadata.is_dir());
        rs = rs
            .add_rule(PathBeneath::new(fd, access).set_compatibility(CompatLevel::HardRequirement))
            .unwrap_or_else(|_| die("add exec rule"));
    }
    for p in std::env::var("SBX_READ")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
    {
        let Ok(metadata) = std::fs::metadata(p) else {
            continue;
        };
        let fd = PathFd::new(p).unwrap_or_else(|_| die("open read rule path"));
        let access = compatible_path_access(read, abi, metadata.is_dir());
        rs = rs
            .add_rule(PathBeneath::new(fd, access).set_compatibility(CompatLevel::HardRequirement))
            .unwrap_or_else(|_| die("add read rule"));
    }
    // Read+write only under the per-run scratch dir (tempfiles/cache/cookies)
    // and explicitly listed non-persistent devices such as /dev/null.
    let read_write = AccessFs::from_read(abi) | AccessFs::from_write(abi);
    for p in std::env::var("SBX_WRITE")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
    {
        let Ok(metadata) = std::fs::metadata(p) else {
            continue;
        };
        let fd = PathFd::new(p).unwrap_or_else(|_| die("open write rule path"));
        let access = compatible_path_access(read_write, abi, metadata.is_dir());
        rs = rs
            .add_rule(PathBeneath::new(fd, access).set_compatibility(CompatLevel::HardRequirement))
            .unwrap_or_else(|_| die("add write rule"));
    }

    // 2. no_new_privs — required for landlock_restrict_self / seccomp without CAP_SYS_ADMIN.
    nix::sys::prctl::set_no_new_privs().unwrap_or_else(|_| die("no_new_privs"));

    // 3. Drop privileges (setgroups → setgid → setuid) and verify it stuck.
    if uid != 0 {
        setgroups(&[]).unwrap_or_else(|_| die("setgroups"));
        setgid(Gid::from_raw(uid)).unwrap_or_else(|_| die("setgid"));
        setuid(Uid::from_raw(uid)).unwrap_or_else(|_| die("setuid"));
        if setuid(Uid::from_raw(0)).is_ok() {
            die("regained uid 0 — privilege drop failed");
        }
    }

    // 4. Apply Landlock — fail closed unless the kernel actually enforced it.
    let status = rs.restrict_self().unwrap_or_else(|_| die("restrict_self"));
    if status.ruleset != RulesetStatus::FullyEnforced {
        die("Landlock ABI v3 restrictions were not fully enforced by the kernel");
    }

    // 5. Resource caps (apply after dropping so NPROC counts the checker uid).
    let bytes = mem_mb.saturating_mul(1024 * 1024);
    setrlimit(Resource::RLIMIT_AS, bytes, bytes).unwrap_or_else(|_| die("rlimit as"));
    setrlimit(Resource::RLIMIT_CPU, cpu_s, cpu_s).unwrap_or_else(|_| die("rlimit cpu"));
    setrlimit(Resource::RLIMIT_NPROC, 64, 64).unwrap_or_else(|_| die("rlimit nproc"));
    setrlimit(Resource::RLIMIT_FSIZE, 8 * 1024 * 1024, 8 * 1024 * 1024)
        .unwrap_or_else(|_| die("rlimit fsize"));

    // 6. seccomp denylist — block kernel-surface syscalls no network checker needs.
    if let Err(e) = apply_seccomp_denylist() {
        die(&format!("seccomp: {e}"));
    }

    // 7. execve the checker with an EXPLICIT allowlisted environment — only the
    //    `RSCTF_*` contract and a minimal Python-runtime set. This drops the `SBX_*`
    //    launcher config and, as defense-in-depth, guarantees no host var leaks to
    //    the checker even if the launcher itself was handed a dirty env. Confinement
    //    (Landlock/no_new_privs/uid/rlimits/seccomp) all survive the exec.
    let prog = CString::new(program.as_str()).unwrap_or_else(|_| die("prog cstr"));
    let argv: Vec<CString> = a[5..]
        .iter()
        .map(|s| CString::new(s.as_str()).unwrap_or_else(|_| die("arg cstr")))
        .collect();
    let envp: Vec<CString> = std::env::vars()
        .filter(|(k, _)| {
            k.starts_with("RSCTF_")
                || matches!(
                    k.as_str(),
                    "HOME"
                        | "TMPDIR"
                        | "LANG"
                        | "LC_CTYPE"
                        | "PYTHONDONTWRITEBYTECODE"
                        | "PYTHONUNBUFFERED"
                )
        })
        .filter_map(|(k, v)| CString::new(format!("{k}={v}")).ok())
        .collect();
    let error = execve(&prog, &argv, &envp).expect_err("execve only returns on failure");
    die(&format!("execve: {error}"));
}

/// A conservative seccomp denylist: default-allow, but block syscalls that are
/// pure attack surface for a confined checker (ptrace/process memory, mount &
/// namespace ops, module/kexec, keyrings, bpf, perf, swap, reboot, etc.). None are
/// used by the supported Python network-checker runtimes, so nothing legitimate
/// breaks.
fn apply_seccomp_denylist() -> Result<(), String> {
    use seccompiler::{
        SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter, SeccompRule,
    };
    let denied: &[libc::c_long] = &[
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
        libc::SYS_kill,
        libc::SYS_tkill,
        libc::SYS_tgkill,
        libc::SYS_rt_sigqueueinfo,
        libc::SYS_rt_tgsigqueueinfo,
        libc::SYS_pidfd_send_signal,
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_pivot_root,
        libc::SYS_chroot,
        libc::SYS_unshare,
        libc::SYS_setns,
        libc::SYS_keyctl,
        libc::SYS_add_key,
        libc::SYS_request_key,
        libc::SYS_bpf,
        libc::SYS_perf_event_open,
        libc::SYS_init_module,
        libc::SYS_finit_module,
        libc::SYS_delete_module,
        libc::SYS_kexec_load,
        libc::SYS_reboot,
        libc::SYS_swapon,
        libc::SYS_swapoff,
        libc::SYS_acct,
        libc::SYS_quotactl,
        libc::SYS_ioperm,
        libc::SYS_iopl,
    ];
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> =
        denied.iter().map(|&n| (n, Vec::new())).collect();
    // Checkers are a network-probe contract, not arbitrary process launchers.
    // Permit pthread-style clone calls but reject new processes. This makes the
    // UID pool plus the outer container's PID cgroup the real aggregate bound,
    // without relying solely on a per-real-UID RLIMIT that may be shared by
    // multiple Pods on a node.
    for syscall in [libc::SYS_fork, libc::SYS_vfork] {
        rules.insert(syscall, Vec::new());
    }
    let non_thread_clone = SeccompCondition::new(
        0,
        SeccompCmpArgLen::Qword,
        SeccompCmpOp::MaskedEq(libc::CLONE_THREAD as u64),
        0,
    )
    .map_err(|error| error.to_string())?;
    rules.insert(
        libc::SYS_clone,
        vec![SeccompRule::new(vec![non_thread_clone]).map_err(|error| error.to_string())?],
    );
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,                     // default: allow
        SeccompAction::Errno(libc::EPERM as u32), // denied: EPERM
        std::env::consts::ARCH
            .try_into()
            .map_err(|_| "arch".to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let prog: seccompiler::BpfProgram = filter.try_into().map_err(|e| format!("{e:?}"))?;
    seccompiler::apply_filter(&prog).map_err(|e| e.to_string())?;

    // clone3 hides its flags behind a pointer, which classic seccomp cannot
    // inspect. Report ENOSYS so libc pthread creation falls back to the
    // inspectable clone syscall above; process-oriented clone3 stays disabled.
    let clone3_rules = BTreeMap::from([(libc::SYS_clone3, Vec::<SeccompRule>::new())]);
    let clone3_filter = SeccompFilter::new(
        clone3_rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::ENOSYS as u32),
        std::env::consts::ARCH
            .try_into()
            .map_err(|_| "arch".to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let clone3_program: seccompiler::BpfProgram = clone3_filter
        .try_into()
        .map_err(|error| format!("{error:?}"))?;
    seccompiler::apply_filter(&clone3_program).map_err(|error| error.to_string())
}

// ─────────────────────────────────────────────────────────────────────────────
// Egress firewall (installed once at startup)
// ─────────────────────────────────────────────────────────────────────────────

/// Default-deny egress for the checker UID range. The startup chain contains
/// only REJECT; each live UID temporarily receives one exact target IP/TCP-port
/// rule after that target is verified inside an A&D service or VPN-client
/// subnet. Everything else, including DNS, peer services, and arbitrary
/// loopback ports where a local Redis/database may be listening.
/// Closes the residual risk of a confined checker reaching unauthenticated
/// control-plane services (e.g. rsctf's own auth-less redis) — the env strip
/// already denies it *credentials*, this denies it *reach*. Needs NET_ADMIN +
/// the iptables `owner` match. Startup fails when either required firewall
/// family cannot be installed; running a round engine without this layer would
/// silently expose the control plane. Idempotent across restarts.
pub fn setup_checker_egress() -> Result<(), String> {
    let uid_range = checker_uid_pool()?.range;
    let uid = uid_range.owner_match();
    let allowed_networks = checker_allowed_networks()?;
    install_checker_firewall("iptables", CHECKER_EGRESS_CHAIN, &uid)?;
    if ipv6_is_available() {
        install_checker_firewall("ip6tables", CHECKER_EGRESS_CHAIN, &uid)?;
    }
    tracing::info!(uid_range = %uid, allowed_networks = ?allowed_networks,
        "checker egress firewall installed (one exact target per leased checker UID)");
    Ok(())
}

fn checker_allowed_networks() -> Result<Vec<IpNet>, String> {
    crate::services::ad_vpn::validate_checker_service_routes()?;
    let mut values = crate::services::ad_vpn::service_route_cidrs()?;
    values.push(crate::services::ad_vpn::client_cidr());
    values
        .into_iter()
        .map(|value| {
            value
                .parse::<IpNet>()
                .map(|network| network.trunc())
                .map_err(|error| format!("invalid checker target network {value:?}: {error}"))
        })
        .collect()
}

fn checker_target_is_allowed(target: std::net::IpAddr) -> Result<bool, String> {
    Ok(checker_allowed_networks()?
        .into_iter()
        .any(|network| network.contains(&target)))
}

fn ipv6_is_available() -> bool {
    std::net::TcpListener::bind("[::1]:0").is_ok()
}

fn firewall_status(program: &str, args: &[&str]) -> Result<bool, String> {
    let _guard = checker_firewall_command_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::process::Command::new(program)
        .arg("-w")
        .arg(FIREWALL_LOCK_WAIT_SECONDS)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .map_err(|error| format!("execute {program}: {error}"))
}

fn classify_firewall_check_exit(code: Option<i32>) -> Result<bool, String> {
    match code {
        Some(0) => Ok(true),
        // iptables documents status 1 for a `-C` rule that is not present.
        Some(1) => Ok(false),
        Some(code) => Err(format!("firewall rule check exited with status {code}")),
        None => Err("firewall rule check terminated by a signal".to_string()),
    }
}

/// Unlike generic mutation commands, `iptables -C` has a meaningful
/// not-present status. Preserve every other failure so cleanup cannot recycle a
/// checker UID while an indeterminate ACCEPT rule may still exist.
fn firewall_rule_exists(program: &str, args: &[&str]) -> Result<bool, String> {
    let _guard = checker_firewall_command_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let status = std::process::Command::new(program)
        .arg("-w")
        .arg(FIREWALL_LOCK_WAIT_SECONDS)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|error| format!("execute {program}: {error}"))?;
    classify_firewall_check_exit(status.code()).map_err(|error| format!("{program}: {error}"))
}

fn require_firewall_rule(program: &str, args: &[&str], action: &str) -> Result<(), String> {
    if firewall_status(program, args)? {
        Ok(())
    } else {
        Err(format!(
            "{program} could not {action}; checker isolation requires NET_ADMIN and the owner match"
        ))
    }
}

fn install_checker_firewall(program: &str, chain: &str, uid: &str) -> Result<(), String> {
    if firewall_status(program, &["-S", chain])? {
        require_firewall_rule(program, &["-F", chain], "flush its checker chain")?;
    } else {
        require_firewall_rule(program, &["-N", chain], "create its checker chain")?;
    }
    require_firewall_rule(
        program,
        &["-A", chain, "-j", "REJECT"],
        "install the checker default reject",
    )?;

    // Older releases used the system `nobody` identity. Remove that historical
    // jump when it is no longer the configured checker identity; otherwise
    // flushing this shared chain would unexpectedly deny an unrelated daemon
    // that also runs as uid 65534 after an in-place upgrade.
    if uid != "65534" {
        remove_firewall_jump(program, chain, "65534")?;
    }
    remove_firewall_jump(program, chain, uid)?;

    // Insert at position one. Appending after a broad OUTPUT accept rule would
    // not enforce a default deny.
    let insert = firewall_jump_args("-I", chain, uid, true);
    require_firewall_rule(program, &insert, "install the checker OUTPUT jump")?;
    let verify = firewall_jump_args("-C", chain, uid, false);
    require_firewall_rule(program, &verify, "verify the checker OUTPUT jump")
}

fn remove_firewall_jump(program: &str, chain: &str, uid: &str) -> Result<(), String> {
    for _ in 0..32 {
        let check = firewall_jump_args("-C", chain, uid, false);
        if !firewall_rule_exists(program, &check)? {
            return Ok(());
        }
        let delete = firewall_jump_args("-D", chain, uid, false);
        require_firewall_rule(program, &delete, "remove a stale checker jump")?;
    }
    Err(format!(
        "{program} retained too many stale checker OUTPUT jumps for uid {uid}"
    ))
}

fn firewall_jump_args<'a>(
    operation: &'a str,
    chain: &'a str,
    uid: &'a str,
    insert_first: bool,
) -> Vec<&'a str> {
    let mut arguments = vec![
        operation,
        "OUTPUT",
        "-m",
        "owner",
        "--uid-owner",
        uid,
        "-j",
        chain,
    ];
    if insert_first {
        arguments.insert(2, "1");
    }
    arguments
}

fn target_rule_args(operation: &str, uid: u32, target: std::net::IpAddr, port: u16) -> Vec<String> {
    let mut args = vec![operation.to_string(), CHECKER_EGRESS_CHAIN.to_string()];
    if operation == "-I" {
        args.push("1".to_string());
    }
    args.extend([
        "-m".to_string(),
        "owner".to_string(),
        "--uid-owner".to_string(),
        uid.to_string(),
        "-d".to_string(),
        target.to_string(),
        "-p".to_string(),
        "tcp".to_string(),
        "--dport".to_string(),
        port.to_string(),
        "-j".to_string(),
        "ACCEPT".to_string(),
    ]);
    args
}

fn firewall_status_owned(program: &str, args: &[String]) -> Result<bool, String> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    firewall_status(program, &refs)
}

fn firewall_rule_exists_owned(program: &str, args: &[String]) -> Result<bool, String> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    firewall_rule_exists(program, &refs)
}

/// Owns both one checker UID and its one exact network permission. A failed
/// rule removal permanently poisons (leaks) that UID until process restart, so
/// it can never be recycled with stale reachability.
struct CheckerEgressLease {
    uid: Option<CheckerUidLease>,
    program: &'static str,
    target: std::net::IpAddr,
    port: u16,
    installed: bool,
}

impl CheckerEgressLease {
    fn install(uid: CheckerUidLease, target: std::net::IpAddr, port: u16) -> Result<Self, String> {
        if !checker_target_is_allowed(target)? {
            return Err(format!(
                "checker target {target}:{port} is outside the configured service and VPN-client networks"
            ));
        }
        let program = if target.is_ipv4() {
            "iptables"
        } else {
            "ip6tables"
        };
        let mut lease = Self {
            uid: Some(uid),
            program,
            target,
            port,
            installed: false,
        };
        if let Err(error) = lease.remove_rule() {
            lease.poison_uid();
            return Err(error);
        }
        let insert = target_rule_args("-I", lease.uid(), target, port);
        // Treat an indeterminate command failure as possibly installed and
        // force an exact cleanup before this UID can return to the pool.
        lease.installed = true;
        if !firewall_status_owned(program, &insert)? {
            let error = format!("{program} could not install the exact checker target rule");
            if lease.remove_rule().is_err() {
                lease.poison_uid();
            }
            return Err(error);
        }
        let verify = target_rule_args("-C", lease.uid(), target, port);
        if !firewall_status_owned(program, &verify)? {
            let error = format!("{program} could not verify the exact checker target rule");
            if lease.remove_rule().is_err() {
                lease.poison_uid();
            }
            return Err(error);
        }
        Ok(lease)
    }

    fn uid(&self) -> u32 {
        self.uid
            .as_ref()
            .expect("active checker egress lease owns its UID")
            .uid
    }

    fn remove_rule(&mut self) -> Result<(), String> {
        let check = target_rule_args("-C", self.uid(), self.target, self.port);
        let delete = target_rule_args("-D", self.uid(), self.target, self.port);
        for _ in 0..32 {
            if !firewall_rule_exists_owned(self.program, &check)? {
                self.installed = false;
                return Ok(());
            }
            if !firewall_status_owned(self.program, &delete)? {
                return Err(format!(
                    "{} could not remove an exact checker target rule",
                    self.program
                ));
            }
        }
        Err(format!(
            "{} retained too many duplicate checker target rules",
            self.program
        ))
    }

    fn poison_uid(&mut self) {
        self.installed = false;
        if let Some(uid) = self.uid.take() {
            std::mem::forget(uid);
        }
    }

    fn close(mut self) -> Result<(), String> {
        if self.installed {
            if let Err(error) = self.remove_rule() {
                self.poison_uid();
                return Err(error);
            }
        }
        Ok(())
    }
}

impl Drop for CheckerEgressLease {
    fn drop(&mut self) {
        if self.installed {
            if let Err(error) = self.remove_rule() {
                tracing::error!(%error, uid = self.uid(), target = %self.target, port = self.port,
                    "checker target rule cleanup failed; poisoning its UID until restart");
                self.poison_uid();
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Runner (called from the async checker; spawns the launcher)
// ─────────────────────────────────────────────────────────────────────────────

struct SandboxRunGuard {
    scratch: String,
    process_group: Option<i32>,
}

impl SandboxRunGuard {
    fn new(scratch: String) -> Self {
        Self {
            scratch,
            process_group: None,
        }
    }

    fn arm(&mut self, pid: Option<u32>) {
        self.process_group = pid.and_then(|pid| i32::try_from(pid).ok());
    }

    fn kill_group(&mut self) {
        if let Some(process_group) = self.process_group.take() {
            // The child starts its own process group. A negative pid targets the
            // complete checker tree, including subprocesses the Python checker
            // may have started.
            unsafe {
                libc::kill(-process_group, libc::SIGKILL);
            }
        }
    }
}

impl Drop for SandboxRunGuard {
    fn drop(&mut self) {
        self.kill_group();
        // The parent runs with CAP_CHOWN but deliberately without broad DAC
        // capabilities. Take the scratch root back from the checker UID before
        // traversing it, otherwise a mode-0700 directory can survive cleanup.
        let _ = nix::unistd::chown(
            self.scratch.as_str(),
            Some(nix::unistd::geteuid()),
            Some(nix::unistd::getegid()),
        );
        let _ = std::fs::remove_dir_all(&self.scratch);
    }
}

/// Spawn the sandboxed checker (`venv_python run_py …`) via the self-re-exec
/// launcher, grant its leased UID only `target:target_port`, enforce a wall-clock
/// `timeout`, and return the checker's exit code or an explicit wall-time
/// exhaustion. Keeping timeout distinct lets callers attribute a shortened
/// platform deadline to infrastructure rather than to the participant.
/// `read_paths`/`exec_paths` are the Landlock allowlist; `env` is the exact set
/// passed to the checker (the caller passes only the `RSCTF_*` contract — every
/// other host env var, incl. rsctf's secrets, is stripped by `env_clear`).
pub enum SandboxOutcome {
    Exit(i32),
    TimedOut,
}

async fn acquire_checker_uid(
    pool: &'static CheckerUidPool,
    timeout: Duration,
) -> std::io::Result<CheckerUidLease> {
    tokio::time::timeout(timeout, pool.acquire())
        .await
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "checker admission capacity exhausted before execution",
            )
        })
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    venv_python: &str,
    run_py: &str,
    target: std::net::IpAddr,
    target_port: u16,
    exec_paths: &[String],
    read_paths: &[String],
    env: &[(String, String)],
    mem_mb: u64,
    timeout: Duration,
) -> std::io::Result<SandboxOutcome> {
    let started = tokio::time::Instant::now();
    let pool = checker_uid_pool().map_err(std::io::Error::other)?;
    let uid_lease = acquire_checker_uid(pool, timeout).await?;
    let remaining = timeout.saturating_sub(started.elapsed());
    let egress = tokio::task::spawn_blocking(move || {
        CheckerEgressLease::install(uid_lease, target, target_port)
    });
    let egress = match tokio::time::timeout(remaining, egress).await {
        Ok(Ok(Ok(lease))) => lease,
        Ok(Ok(Err(error))) => return Err(std::io::Error::other(error)),
        Ok(Err(error)) => return Err(std::io::Error::other(error.to_string())),
        // The detached blocking task still owns the UID. If it completes an
        // insertion, dropping its unobserved output removes the rule first.
        Err(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "checker egress admission exhausted before execution",
            ));
        }
    };
    let self_exe = std::env::current_exe()?;
    let cpu_s = timeout.as_secs().max(1) + 2; // CPU rlimit ≥ wall timeout
                                              // Fresh writable scratch owned by the checker uid — the only persistent
                                              // place it can write (tempfiles/cache/cookies). Isolated per run and
                                              // removed afterward. /dev/null is separately allowed for libraries that
                                              // open os.devnull themselves (pwntools does this during import).
    let scratch = make_scratch(egress.uid())?;
    let mut cleanup = SandboxRunGuard::new(scratch.clone());
    let mut cmd = tokio::process::Command::new(self_exe);
    cmd.arg(LAUNCH_ARG)
        .arg(egress.uid().to_string())
        .arg(mem_mb.to_string())
        .arg(cpu_s.to_string())
        .arg(venv_python)
        .arg(run_py);
    cmd.env_clear(); // strip ALL host env — secrets never reach the checker
    cmd.env("SBX_EXEC", exec_paths.join(":"));
    cmd.env("SBX_READ", read_paths.join(":"));
    cmd.env("SBX_WRITE", sandbox_write_paths(&scratch));
    for (k, v) in env {
        cmd.env(k, v);
    }
    // Point HOME + TMPDIR at the writable scratch (override any caller HOME).
    cmd.env("HOME", &scratch);
    cmd.env("TMPDIR", &scratch);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    // New process group so a timeout can kill the whole tree.
    cmd.process_group(0);
    cmd.kill_on_drop(true);

    let mut child = cmd.spawn()?;
    cleanup.arm(child.id());
    // UID-pool contention is part of the caller's SLA window. It cannot extend
    // a round merely because another game saturated this process's budget.
    let remaining = timeout.saturating_sub(started.elapsed());
    let result = match tokio::time::timeout(remaining, child.wait()).await {
        Ok(Ok(status)) => Ok(SandboxOutcome::Exit(
            status.code().unwrap_or(SANDBOX_FAIL_EXIT),
        )),
        Ok(Err(error)) => Err(error),
        Err(_) => {
            // Timed out — kill the process group. A checker that can't complete
            // in the window means the service didn't respond → Offline (exit 2).
            cleanup.kill_group();
            let _ = child.kill().await;
            Ok(SandboxOutcome::TimedOut)
        }
    };
    // Clean up any daemonized descendants even after the main checker exits.
    cleanup.kill_group();
    drop(cleanup);
    let egress_cleanup = tokio::task::spawn_blocking(move || egress.close())
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))?
        .map_err(std::io::Error::other);
    match (result, egress_cleanup) {
        (_, Err(error)) => Err(error),
        (result, Ok(())) => result,
    }
}

/// Landlock read/write allowlist for one checker process. The scratch directory
/// is its only persistent writable location; the exact null device is included
/// because common network libraries open `os.devnull` during import.
fn sandbox_write_paths(scratch: &str) -> String {
    if null_device_available() {
        format!("{scratch}:/dev/null")
    } else {
        scratch.to_string()
    }
}

#[cfg(unix)]
fn null_device_available() -> bool {
    use std::os::unix::fs::FileTypeExt;

    std::fs::metadata("/dev/null").is_ok_and(|metadata| metadata.file_type().is_char_device())
}

/// Create a fresh scratch directory (mode 0700) owned by the checker uid so the
/// confined checker can write tempfiles there and nowhere else. Unique via a
/// process-lifetime counter (no wall clock / RNG, which the workflow env forbids).
fn make_scratch(uid: u32) -> std::io::Result<String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let base = std::env::var("RSCTF_CHECKER_SCRATCH").unwrap_or_else(|_| "/tmp".to_string());
    let dir = format!("{base}/rsctf-chk-{}-{}", std::process::id(), n);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    // Tighten permissions while the directory is still root-owned. The
    // control role deliberately has CAP_CHOWN but not CAP_FOWNER, so chmod
    // would fail after ownership is transferred to the checker identity.
    let mut perms = std::fs::metadata(&dir)?.permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o700);
    std::fs::set_permissions(&dir, perms)?;
    nix::unistd::chown(
        dir.as_str(),
        Some(nix::unistd::Uid::from_raw(uid)),
        Some(nix::unistd::Gid::from_raw(uid)),
    )?;
    Ok(dir)
}

/// In-process TCP reachability probe — the built-in check for a service with NO
/// custom checker. Replaces the alpine `nc -z` container entirely: `Ok` if the
/// port accepts a connection within `timeout`, else `Offline`. No subprocess.
pub async fn tcp_probe(host: &str, port: u16, timeout: Duration) -> bool {
    matches!(
        tokio::time::timeout(timeout, tokio::net::TcpStream::connect((host, port))).await,
        Ok(Ok(_))
    )
}

#[cfg(test)]
mod tests;
