use super::{
    classify_firewall_check_exit, compatible_path_access, firewall_jump_args,
    install_checker_firewall, null_device_available, sandbox_write_paths, target_rule_args,
    CheckerUidPool, CheckerUidRange, SandboxRunGuard,
};
use landlock::{AccessFs, ABI};

#[test]
fn dropped_guard_reclaims_scratch() {
    let scratch =
        std::env::temp_dir().join(format!("rsctf-sandbox-guard-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("create test scratch");

    drop(SandboxRunGuard::new(scratch.to_string_lossy().into_owned()));

    assert!(!scratch.exists());
}

#[test]
fn checker_firewall_fails_closed_when_tooling_is_unavailable() {
    let result = install_checker_firewall(
        "/definitely/missing/rsctf-iptables",
        "RSCTF_CHECKER_TEST",
        "65534",
    );
    assert!(result.is_err());
}

#[test]
fn stale_jump_delete_argv_contains_one_complete_rule() {
    assert_eq!(
        firewall_jump_args("-D", "RSCTF_CHECKER_EGRESS", "60000-60031", false),
        [
            "-D",
            "OUTPUT",
            "-m",
            "owner",
            "--uid-owner",
            "60000-60031",
            "-j",
            "RSCTF_CHECKER_EGRESS",
        ]
    );
}

#[test]
fn firewall_check_does_not_treat_operational_failure_as_absence() {
    assert!(classify_firewall_check_exit(Some(0)).unwrap());
    assert!(!classify_firewall_check_exit(Some(1)).unwrap());
    assert!(classify_firewall_check_exit(Some(4)).is_err());
    assert!(classify_firewall_check_exit(None).is_err());
}

#[test]
fn landlock_file_rules_exclude_directory_only_access() {
    let abi = ABI::V3;
    let requested = AccessFs::from_read(abi) | AccessFs::from_write(abi);
    let file = compatible_path_access(requested, abi, false);
    assert!(file.contains(AccessFs::ReadFile));
    assert!(file.contains(AccessFs::WriteFile));
    assert!(file.contains(AccessFs::Truncate));
    assert!(!file.contains(AccessFs::ReadDir));
    assert!(!file.contains(AccessFs::MakeReg));
    assert!(!file.contains(AccessFs::Refer));
    assert_eq!(compatible_path_access(requested, abi, true), requested);
}

#[test]
fn checker_write_allowlist_has_only_scratch_and_optional_null_device() {
    let paths = sandbox_write_paths("/tmp/rsctf-checker-test-scratch");
    let paths: Vec<_> = paths.split(':').collect();
    assert_eq!(paths.first(), Some(&"/tmp/rsctf-checker-test-scratch"));
    assert!(paths.len() <= 2);
    if null_device_available() {
        assert_eq!(paths.get(1), Some(&"/dev/null"));
    }
}

#[test]
fn dynamic_rule_is_one_uid_one_ip_and_one_tcp_port() {
    let args = target_rule_args("-I", 60_007, "10.13.40.9".parse().unwrap(), 31337);
    assert_eq!(args[0..3], ["-I", "RSCTF_CHECKER_EGRESS", "1"]);
    assert!(args.windows(2).any(|pair| pair == ["--uid-owner", "60007"]));
    assert!(args.windows(2).any(|pair| pair == ["-d", "10.13.40.9"]));
    assert!(args.windows(2).any(|pair| pair == ["--dport", "31337"]));
    assert!(!args.iter().any(|value| value.contains('/')));
    assert!(args.windows(2).any(|pair| pair == ["-p", "tcp"]));
}

#[tokio::test]
async fn uid_pool_never_reuses_a_live_checker_identity() {
    let pool = Box::leak(Box::new(CheckerUidPool::new(CheckerUidRange {
        base: 61_000,
        count: 2,
    })));
    let first = pool.acquire().await;
    let second = pool.acquire().await;
    assert_ne!(first.uid, second.uid);
    let released = first.uid;
    drop(first);
    let replacement = tokio::time::timeout(std::time::Duration::from_millis(50), pool.acquire())
        .await
        .expect("released UID becomes available");
    assert_eq!(replacement.uid, released);
}
