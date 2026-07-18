//! The A&D SLA checker — the process-sandboxed checker executor, its
//! exit-code → [`AdCheckStatus`] mapping, verdict persistence, and the pure
//! in-memory scheduler marker used by the tests.
use super::persistence::AdProbeResult;
use super::*;
use futures::StreamExt;

mod diagnostics;
mod koth;
pub(super) use diagnostics::{bounded_diagnostic, bounded_optional_diagnostic};

/// Max concurrent service probes per checker pass. Scale the mostly I/O-bound
/// sandbox work with available CPUs while preserving a hard fork/memory ceiling.
pub(super) fn checker_concurrency() -> usize {
    std::env::var("RSCTF_AD_CHECKER_CONCURRENCY")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| (1..=256).contains(value))
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map_or(32, |cpus| cpus.get().saturating_mul(8))
                .clamp(32, 128)
        })
}

/// Port of `AdRoundScheduler`: the 5s-cadence background loop that bootstraps
/// round 1 after warmup and auto-advances expired rounds through [`plan_round`].
///
/// The durable loop is implemented: `advance_round` (query active A&D games, read
/// the latest round, insert the next round + rotated flags) and `run_checker`
/// (per-service SLA verdicts) are driven by the cron supervisor, which also
/// flushes the scoreboard cache after each advance. This struct remains the
/// pure in-memory scheduler used by the tests; the durable path
/// can't be written honestly here — only the pure per-game decision
/// ([`needs_advance`] → [`plan_round`]) it drives, which is fully implemented
/// and tested above.
pub struct RoundScheduler;

impl RoundScheduler {
    /// Poll cadence RSCTF uses (5s). Drift between round-end and next-round-start
    /// is bounded by this.
    pub const POLL_SECONDS: u64 = 5;
}

/// The result the checker executor returns for one probe. Port of the slice
/// of `AdCheckerService` output the scoring engine consumes.
#[derive(Clone, Debug, PartialEq)]
pub struct CheckerVerdict {
    pub status: AdCheckStatus,
    /// Flags the checker exfiltrated from OTHER teams while probing (seeds the
    /// attack graph). Empty for a pure SLA-only checker.
    pub stolen_flags: Vec<String>,
}

/// Run the A&D SLA checker tick for one round. Custom checkers execute as
/// resource-limited local subprocesses; the built-in fallback is an in-process
/// TCP probe.
///
/// For every registered [`ad_team_service`] in `game_id` this:
/// 1. resolves the round's planted [`ad_flag`] for that service (the value the
///    checker retrieves via the `RSCTF_FLAG` env contract),
/// 2. launches the prepared checker through the rsctf process sandbox with an
///    enochecker3-style env contract pointed at the service's `host:port`,
/// 3. waits for it to exit (bounded by a timeout), reads the exit code + output,
///    maps the exit code to an [`AdCheckStatus`] (custom checker: `0→Ok`,
///    `1→Mumble`, `2→Offline`, else `InternalError`; the built-in probe
///    reports `Ok` or `Offline`), and
/// 4. UPDATEs (or inserts) the `(round, service)` [`ad_check_result`] row with
///    the verdict, message, and check time.
///
/// Infrastructure faults become [`AdCheckStatus::InternalError`]. Official SLA
/// carries the service's prior adjudicated result; an isolated first error is
/// local zero, while an all-service first-error outage voids the shared sample.
/// Only a clean checker exit may report `Offline`/`Mumble`, and a checker wall
/// timeout is `Offline`.
pub(crate) async fn run_checker(
    db: &DatabaseConnection,
    containers: &dyn crate::services::container::ContainerManager,
    game_id: i32,
    round_id: i32,
    lease: &RoundFinishLease,
) -> AppResult<()> {
    // Resolve the round; it must belong to this game. An unknown/foreign round
    // has nothing to check (warmup / bad id) — no-op rather than error.
    let round = match ad_round::Entity::find_by_id(round_id).one(db).await? {
        Some(r) if r.game_id == game_id => r,
        _ => return Ok(()),
    };

    // Only currently authorized services are probed this tick.
    let services = super::active_ad_services(db, game_id).await?;
    // Per-challenge checker DIRECTORY map + probe timeout. Needed by BOTH the A&D
    // probes and the KotH hill check, so computed regardless of A&D services — a
    // pure-KotH game has zero services but its hills must still be checked.
    // `ad_checker_image` holds the prepared checker dir (`None`/empty → built-in probe).
    let checker_dirs: std::collections::HashMap<i32, Option<String>> =
        game_challenge::Entity::find()
            .filter(game_challenge::Column::GameId.eq(game_id))
            .filter(game_challenge::Column::IsEnabled.eq(true))
            .filter(game_challenge::Column::ReviewStatus.eq(ChallengeReviewStatus::Active))
            .all(db)
            .await?
            .into_iter()
            .map(|c| (c.id, c.ad_checker_image.filter(|s| !s.trim().is_empty())))
            .collect();
    let timeout = std::time::Duration::from_secs(checker_timeout_secs());

    // Run A&D and KotH passes concurrently. Sequential execution let a large A&D
    // field consume the per-game deadline before a shared hill was ever sampled.
    let ad_pass = async {
        if services.is_empty() {
            return Ok::<(), AppError>(());
        }
        // This round's planted flags, batched in one query (was an N+1 per service).
        let service_ids: Vec<i32> = services.iter().map(|s| s.id).collect();
        let flags: std::collections::HashMap<i32, String> = ad_flag::Entity::find()
            .filter(ad_flag::Column::RoundId.eq(round_id))
            .filter(ad_flag::Column::TeamServiceId.is_in(service_ids))
            .all(db)
            .await?
            .into_iter()
            .map(|f| (f.team_service_id, f.flag))
            .collect();
        // A failed current-flag publication is platform evidence, not a service
        // verdict. Its placeholder was completed as an immutable InternalError
        // void in the publication transaction; do not spend checker capacity or
        // risk attributing the platform's missing flag to the participant.
        let delivery_voids: std::collections::HashSet<i32> = sqlx::query_scalar::<_, i32>(
            r#"SELECT team_service_id
                     FROM "AdFlagDeliveryResults"
                    WHERE round_id = $1 AND delivered = FALSE"#,
        )
        .bind(round_id)
        .fetch_all(db.get_postgres_connection_pool())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .into_iter()
        .collect();

        // Probe services with bounded concurrency. Sequentially, one offline service
        // burning the full (up to 600s) timeout serializes behind every other, so a pass
        // of N services could take N × timeout — blowing the round window AND blocking the
        // single cron supervisor task (reaper, other games) for minutes.
        let round_number = round.number;
        // Owned per-service probe inputs — no borrows of `checker_dirs`/`flags`/`services`
        // held across the concurrent await (keeps the future `Send` for the cron task).
        #[allow(clippy::type_complexity)]
        let inputs: Vec<(
            i32,
            String,
            i32,
            i32,
            i32,
            Option<String>,
            Option<String>,
            Option<String>,
        )> = services
            .iter()
            .filter(|svc| !delivery_voids.contains(&svc.id))
            .map(|svc| {
                (
                    svc.id,
                    svc.host.clone(),
                    svc.port,
                    svc.participation_id,
                    svc.challenge_id,
                    svc.container_id.clone(),
                    checker_dirs.get(&svc.challenge_id).cloned().flatten(),
                    flags.get(&svc.id).cloned(),
                )
            })
            .collect();
        // Run persistence and probe production as sibling futures owned by this
        // checker pass. Cancelling the round pipeline drops both before its
        // durable lease can be released; unresolved placeholders remain NULL so
        // the next lease owner can recover them. No result writer may outlive its
        // owning pipeline and race a replacement owner's real verdict.
        let (result_tx, result_rx) = tokio::sync::mpsc::unbounded_channel();
        let producer = async move {
            let mut probes = futures::stream::iter(inputs)
                .map(
                    |(sid, host, port, team_id, chal_id, container_id, dir, flag)| async move {
                        let flag_aware =
                            dir.is_some() && flag.as_deref().is_some_and(|value| !value.is_empty());
                        let (status, message) = run_check(
                            dir.as_deref(),
                            &host,
                            port,
                            round_number,
                            team_id,
                            chal_id,
                            flag.as_deref(),
                            timeout,
                        )
                        .await;
                        let observed_at = Utc::now();
                        AdProbeResult {
                            service_id: sid,
                            participation_id: team_id,
                            challenge_id: chal_id,
                            host,
                            port,
                            container_id,
                            status,
                            message,
                            flag_verified: flag_aware && status == AdCheckStatus::Ok,
                            observed_at,
                        }
                    },
                )
                .buffer_unordered(checker_concurrency());
            while let Some(result) = probes.next().await {
                result_tx.send(result).map_err(|_| {
                    AppError::internal("A&D checker result writer stopped before probe completion")
                })?;
            }
            Ok::<(), AppError>(())
        };
        let writer = super::persistence::record_check_result_batches(
            db, game_id, round_id, lease, result_rx,
        );
        tokio::try_join!(producer, writer)?;
        Ok(())
    };

    let koth_pass = koth::check_hills(
        db,
        containers,
        game_id,
        &round,
        &checker_dirs,
        timeout,
        lease,
    );
    let (ad_result, koth_result) = tokio::join!(ad_pass, koth_pass);
    ad_result?;
    koth_result?;

    Ok(())
}

/// Checker run timeout in seconds — RSCTF `Ad:Checker:TimeoutSeconds`, clamped to
/// `1..=600`, compatibility default 30. High-density deployments can explicitly
/// lower it after matching the value to their challenge checker SLA.
fn checker_timeout_secs() -> u64 {
    std::env::var("RSCTF_AD_CHECKER_TIMEOUT_SECONDS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&s| (1..=600).contains(&s))
        .unwrap_or(30)
}

/// Functional readiness gate used by the crown-cycle state machine. It shares
/// the production checker executor and timeout instead of treating container
/// liveness or an open socket as proof that a pristine replacement is healthy.
pub(crate) async fn validate_koth_functional_readiness(
    checker_dir: Option<&str>,
    host: &str,
    port: i32,
    round_number: i32,
    challenge_id: i32,
) -> (AdCheckStatus, Option<String>) {
    let (status, message) = run_check(
        checker_dir,
        host,
        port,
        round_number,
        0,
        challenge_id,
        None,
        std::time::Duration::from_secs(checker_timeout_secs()),
    )
    .await;
    (status, bounded_optional_diagnostic(message))
}

/// Core check: probe `host:port` with the sandboxed checker (`checker_dir`) or the
/// built-in in-process TCP probe. Shared by A&D services and KotH hills — KotH
/// passes `flag = None` (a health-only check). The subprocess runs inside rsctf's
/// own network namespace (already on `rsctf-ad`), so `host` is directly reachable.
#[allow(clippy::too_many_arguments)]
async fn run_check(
    checker_dir: Option<&str>,
    host: &str,
    port: i32,
    round_number: i32,
    team_id: i32,
    challenge_id: i32,
    flag: Option<&str>,
    timeout: std::time::Duration,
) -> (AdCheckStatus, Option<String>) {
    let host = host.trim();
    if host.is_empty() || port <= 0 || port > 65535 {
        return (AdCheckStatus::Offline, Some("no endpoint".to_string()));
    }
    let port_u16 = port as u16;

    let Some(dir) = checker_dir else {
        return if super::sandbox::tcp_probe(host, port_u16, timeout).await {
            (AdCheckStatus::Ok, None)
        } else {
            (AdCheckStatus::Offline, Some("tcp probe failed".to_string()))
        };
    };

    // Custom checker → sandboxed subprocess (`<dir>/venv/bin/python3 <dir>/src/run.py`).
    let venv_python = format!("{dir}/venv/bin/python3");
    let run_py = format!("{dir}/src/run.py");
    if !std::path::Path::new(&run_py).exists() {
        return (
            AdCheckStatus::InternalError,
            Some("checker not prepared (run.py missing)".to_string()),
        );
    }
    let _execution_lease =
        match crate::services::git_sync::acquire_checker_execution_lease(std::path::Path::new(dir))
        {
            Ok(lease) => lease,
            Err(error) => {
                return (
                    AdCheckStatus::InternalError,
                    Some(bounded_diagnostic(format!(
                        "checker execution lease failed: {error}"
                    ))),
                );
            }
        };

    // Resolve names in the trusted parent before dropping privileges. Checker
    // UIDs have no DNS egress at all: even an approved-but-compromised checker
    // cannot tunnel the current flag through a recursive resolver.
    let started = tokio::time::Instant::now();
    let target_ip = if let Ok(address) = host.parse::<std::net::IpAddr>() {
        address
    } else {
        let resolved = tokio::time::timeout(timeout, tokio::net::lookup_host((host, port_u16)))
            .await
            .ok()
            .and_then(Result::ok)
            .and_then(|mut addresses| addresses.next());
        let Some(address) = resolved else {
            return (
                AdCheckStatus::Offline,
                Some("checker target resolution failed".to_string()),
            );
        };
        address.ip()
    };
    let remaining = timeout.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        return (
            AdCheckStatus::Offline,
            Some("checker target resolution timed out".to_string()),
        );
    }

    // enochecker3-style RSCTF_* environment contract. This is the ONLY env the
    // sandbox passes — every host var (incl. rsctf secrets) is
    // stripped by `env_clear` in `sandbox::run`.
    let mut env: Vec<(String, String)> = vec![
        ("RSCTF_ACTION".into(), "check".into()),
        ("RSCTF_TARGET_IP".into(), target_ip.to_string()),
        ("RSCTF_TARGET_PORT".into(), port.to_string()),
        ("RSCTF_ROUND".into(), round_number.to_string()),
        ("RSCTF_TEAM_ID".into(), team_id.to_string()),
        ("RSCTF_CHALLENGE_ID".into(), challenge_id.to_string()),
        // Python needs these to start + find CA certs; no secrets.
        ("PYTHONDONTWRITEBYTECODE".into(), "1".into()),
        ("HOME".into(), dir.to_string()),
    ];
    if let Some(f) = flag {
        if !f.is_empty() {
            env.push(("RSCTF_FLAG".into(), f.to_string()));
        }
    }

    match super::sandbox::run(
        &venv_python,
        &run_py,
        target_ip,
        port_u16,
        &sandbox_exec_paths(&venv_python),
        &sandbox_read_paths(dir),
        &env,
        checker_mem_mb(),
        remaining,
    )
    .await
    {
        Ok(code) => (
            map_exit_code(code as i64, true),
            (code != 0).then(|| format!("checker exit {code}")),
        ),
        Err(e) => (
            AdCheckStatus::InternalError,
            Some(bounded_diagnostic(format!("sandbox spawn error: {e}"))),
        ),
    }
}

/// Landlock read+exec allowlist for the checker: the venv (its `bin/` + site
/// packages), the real interpreter the venv `python3` symlinks to, and the shared
/// library dirs the loader needs. Deliberately NOT `/usr/bin` or `/bin` broadly,
/// so `os.system`/`subprocess` can't exec `/bin/sh` or other host binaries.
fn sandbox_exec_paths(venv_python: &str) -> Vec<String> {
    let venv_bin = std::path::Path::new(venv_python)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut paths = vec![
        venv_bin,
        "/usr/lib".to_string(),
        "/usr/lib64".to_string(),
        "/lib".to_string(),
        "/lib64".to_string(),
        "/etc/ld.so.cache".to_string(),
    ];
    // The venv's python3 is a symlink to the system interpreter — allow its target.
    if let Ok(real) = std::fs::canonicalize(venv_python) {
        paths.push(real.to_string_lossy().into_owned());
    }
    paths
}

/// Landlock read allowlist: the checker's own dir (its `src/` + `venv/` deps) plus
/// the DNS/TLS configuration that network-client libraries may need. Nothing
/// else on the host is readable.
fn sandbox_read_paths(dir: &str) -> Vec<String> {
    let mut paths = vec![dir.to_string()];
    for p in [
        "/etc/resolv.conf",
        "/etc/hosts",
        "/etc/nsswitch.conf",
        "/etc/gai.conf",
        "/etc/ssl",
        "/usr/share/ca-certificates",
        "/etc/pki",
    ] {
        if std::path::Path::new(p).exists() {
            paths.push(p.to_string());
        }
    }
    paths
}

/// Per-check memory cap (MB) for the checker sandbox (`RSCTF_AD_CHECKER_MEM_MB`).
fn checker_mem_mb() -> u64 {
    std::env::var("RSCTF_AD_CHECKER_MEM_MB")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&m: &u64| (64..=2048).contains(&m))
        .unwrap_or(256)
}

/// enochecker3 exit-code → [`AdCheckStatus`] (RSCTF `AdCheckMapping.FromExitCode`).
fn map_exit_code(exit_code: i64, use_custom: bool) -> AdCheckStatus {
    if !use_custom {
        // Built-in TCP probe: 0 → up, anything else → down.
        return if exit_code == 0 {
            AdCheckStatus::Ok
        } else {
            AdCheckStatus::Offline
        };
    }
    match exit_code {
        0 => AdCheckStatus::Ok,
        1 => AdCheckStatus::Mumble,
        2 => AdCheckStatus::Offline,
        _ => AdCheckStatus::InternalError,
    }
}

#[cfg(test)]
mod scheduling_tests {
    use super::*;

    #[test]
    fn checker_parallelism_and_timeout_are_bounded() {
        assert!((1..=256).contains(&checker_concurrency()));
        assert!((1..=600).contains(&checker_timeout_secs()));
    }
}
