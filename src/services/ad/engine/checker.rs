//! The A&D SLA checker — the process-sandboxed checker executor, its
//! exit-code → [`AdCheckStatus`] mapping, verdict persistence, and the pure
//! in-memory scheduler marker used by the tests.
use super::*;
use futures::StreamExt;

mod ad;
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

pub(crate) const DEFAULT_CHECKER_GRACE_SECONDS: i32 = 3;
const DEFAULT_CHECKER_WINDOW_FRACTION: f64 = 0.5;
const CHECKER_PERSISTENCE_MARGIN: std::time::Duration = std::time::Duration::from_secs(2);
const MIN_CHECKER_PROBE_BUDGET: std::time::Duration = std::time::Duration::from_secs(1);
const MIN_CHECKER_SCHEDULING_SLACK: std::time::Duration = std::time::Duration::from_secs(1);

const CHECKER_TIMING_SQL: &str = r#"SELECT GREATEST(1, FLOOR(EXTRACT(EPOCH FROM
                         (round.end_time_utc - round.start_time_utc))))::integer,
                  game.ad_getflag_window_fraction,
                  game.ad_min_grace_period_seconds
             FROM "AdRounds" round
             JOIN "Games" game ON game.id = round.game_id
            WHERE round.id = $1 AND round.game_id = $2"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CheckerSchedule {
    grace: std::time::Duration,
    maximum_jitter: std::time::Duration,
    probe_budget: std::time::Duration,
}

fn checker_schedule(
    tick_seconds: i32,
    grace_seconds: i32,
    window_fraction: f64,
    available_after_publication: std::time::Duration,
    configured_probe_timeout: std::time::Duration,
) -> Option<CheckerSchedule> {
    let tick_millis = u64::try_from(tick_seconds.clamp(30, 600)).unwrap_or(60) * 1_000;
    let grace_millis = u64::try_from(grace_seconds.clamp(1, 60)).unwrap_or(3) * 1_000;
    let fraction = if window_fraction.is_finite() {
        window_fraction.clamp(0.05, 0.9)
    } else {
        DEFAULT_CHECKER_WINDOW_FRACTION
    };
    let available_millis =
        u64::try_from(available_after_publication.as_millis()).unwrap_or(u64::MAX);
    let persistence_millis =
        u64::try_from(CHECKER_PERSISTENCE_MARGIN.as_millis()).unwrap_or(u64::MAX);
    let runway_millis = available_millis
        .checked_sub(grace_millis)?
        .checked_sub(persistence_millis)?;
    let minimum_probe_millis =
        u64::try_from(MIN_CHECKER_PROBE_BUDGET.as_millis()).unwrap_or(u64::MAX);
    if runway_millis < minimum_probe_millis {
        return None;
    }
    let configured_millis = u64::try_from(configured_probe_timeout.as_millis()).unwrap_or(u64::MAX);
    let scheduling_slack_millis =
        u64::try_from(MIN_CHECKER_SCHEDULING_SLACK.as_millis()).unwrap_or(u64::MAX);
    let probe_millis = if runway_millis >= configured_millis.saturating_add(scheduling_slack_millis)
    {
        configured_millis
    } else {
        (runway_millis / 2).max(minimum_probe_millis)
    };
    let jitter_millis = (((tick_millis as f64) * fraction).floor() as u64)
        .min(runway_millis.saturating_sub(probe_millis));
    Some(CheckerSchedule {
        grace: std::time::Duration::from_millis(grace_millis),
        maximum_jitter: std::time::Duration::from_millis(jitter_millis),
        probe_budget: std::time::Duration::from_millis(probe_millis),
    })
}

fn checker_delay_from_entropy(schedule: CheckerSchedule, entropy: u64) -> std::time::Duration {
    let jitter_millis = u64::try_from(schedule.maximum_jitter.as_millis()).unwrap_or(u64::MAX);
    let jitter = entropy % jitter_millis.saturating_add(1);
    schedule
        .grace
        .saturating_add(std::time::Duration::from_millis(jitter))
}

fn random_checker_delay(schedule: CheckerSchedule) -> std::time::Duration {
    let mut entropy = [0_u8; 8];
    rand::fill(&mut entropy);
    checker_delay_from_entropy(schedule, u64::from_le_bytes(entropy))
}

fn checker_start_instant(
    published_at: chrono::DateTime<Utc>,
    delay: std::time::Duration,
    wall_now: chrono::DateTime<Utc>,
    monotonic_now: tokio::time::Instant,
) -> tokio::time::Instant {
    let requested = chrono::Duration::from_std(delay)
        .ok()
        .and_then(|delay| published_at.checked_add_signed(delay))
        .unwrap_or(published_at);
    let remaining = requested
        .signed_duration_since(wall_now)
        .to_std()
        .unwrap_or_default();
    monotonic_now + remaining
}

fn checker_available_after_delivery(
    delivered_at: chrono::DateTime<Utc>,
    wall_now: chrono::DateTime<Utc>,
    monotonic_now: tokio::time::Instant,
    effective_deadline: tokio::time::Instant,
) -> std::time::Duration {
    let remaining = effective_deadline.saturating_duration_since(monotonic_now);
    match wall_now.signed_duration_since(delivered_at).to_std() {
        Ok(delivery_age) => remaining.saturating_add(delivery_age),
        Err(_) => delivered_at
            .signed_duration_since(wall_now)
            .to_std()
            .map_or(std::time::Duration::ZERO, |until_delivery| {
                remaining.saturating_sub(until_delivery)
            }),
    }
}

fn checker_probe_can_start(
    effective_deadline: tokio::time::Instant,
    probe_budget: std::time::Duration,
    completion_margin: std::time::Duration,
    now: tokio::time::Instant,
) -> bool {
    effective_deadline
        .checked_duration_since(now)
        .and_then(|remaining| remaining.checked_sub(completion_margin))
        .is_some_and(|remaining| remaining >= probe_budget)
}

fn deadline_limited_probe_budget(
    effective_deadline: tokio::time::Instant,
    configured: std::time::Duration,
    completion_margin: std::time::Duration,
    now: tokio::time::Instant,
) -> Option<std::time::Duration> {
    let runway = effective_deadline
        .checked_duration_since(now)?
        .checked_sub(completion_margin)
        .filter(|remaining| *remaining >= MIN_CHECKER_PROBE_BUDGET * 2)?;
    if runway >= configured.saturating_add(MIN_CHECKER_SCHEDULING_SLACK) {
        // Preserve the complete participant-attributed timeout whenever it
        // fits, shrinking jitter/queue slack before weakening the SLA contract.
        Some(configured)
    } else {
        // Recovery near a deadline gets one smaller platform-attributed pass
        // and retains the other half for queueing and scheduler wake-up skew.
        Some(runway / 2)
    }
}

fn exhausted_budget_status(timeout_is_platform_limited: bool) -> AdCheckStatus {
    if timeout_is_platform_limited {
        AdCheckStatus::InternalError
    } else {
        AdCheckStatus::Offline
    }
}

fn probe_budget_is_platform_limited(
    planned: std::time::Duration,
    nominal: std::time::Duration,
) -> bool {
    planned < nominal
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
    pipeline_deadline: tokio::time::Instant,
    delivery_receipts: tokio::sync::mpsc::UnboundedReceiver<FlagDeliveryReceipt>,
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
    let (tick_seconds, window_fraction, grace_seconds): (i32, Option<f64>, Option<i32>) =
        sqlx::query_as(CHECKER_TIMING_SQL)
            .bind(round_id)
            .bind(game_id)
            .fetch_one(db.get_postgres_connection_pool())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    let window_fraction = window_fraction.unwrap_or(DEFAULT_CHECKER_WINDOW_FRACTION);
    let grace_seconds = grace_seconds.unwrap_or(DEFAULT_CHECKER_GRACE_SECONDS);
    let wall_now = Utc::now();
    let monotonic_now = tokio::time::Instant::now();
    let round_remaining = round
        .end_time_utc
        .signed_duration_since(wall_now)
        .to_std()
        .unwrap_or_default();
    let effective_deadline = pipeline_deadline.min(monotonic_now + round_remaining);
    // The nominal schedule defines the participant-facing timeout contract for
    // this game's tick/grace settings. Only a later runtime/deadline reduction
    // is infrastructure-attributed; an expiry at this nominal budget is a real
    // participant Offline verdict.
    let nominal_schedule = checker_schedule(
        tick_seconds,
        grace_seconds,
        window_fraction,
        std::time::Duration::from_secs(u64::try_from(tick_seconds).unwrap_or(60)).saturating_sub(
            std::time::Duration::from_secs(
                FLAG_DELIVERY_PUBLICATION_RESERVE_SECONDS + CHECKER_SCHEDULER_OUTER_MARGIN_SECONDS,
            ),
        ),
        timeout,
    );
    let nominal_probe_budget = nominal_schedule.map(|schedule| schedule.probe_budget);
    let current_probe_cap = nominal_probe_budget.and_then(|nominal_budget| {
        deadline_limited_probe_budget(
            effective_deadline,
            nominal_budget,
            CHECKER_PERSISTENCE_MARGIN,
            monotonic_now,
        )
    });

    // Run A&D and KotH passes concurrently. Sequential execution let a large A&D
    // field consume the per-game deadline before a shared hill was ever sampled.
    let ad_pass = ad::check_services(
        db,
        services,
        game_id,
        round_id,
        lease,
        checker_dirs.clone(),
        ad::AdCheckerTiming {
            round_number: round.number,
            tick_seconds,
            grace_seconds,
            window_fraction,
            current_probe_cap,
            nominal_probe_budget,
            effective_deadline,
        },
        delivery_receipts,
    );

    let koth_pass = koth::check_hills(
        db,
        containers,
        game_id,
        &round,
        &checker_dirs,
        timeout,
        lease,
        effective_deadline,
        tick_seconds,
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
        false,
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
    timeout_is_platform_limited: bool,
) -> (AdCheckStatus, Option<String>) {
    let host = host.trim();
    if host.is_empty() || port <= 0 || port > 65535 {
        return (AdCheckStatus::Offline, Some("no endpoint".to_string()));
    }
    let port_u16 = port as u16;

    let Some(dir) = checker_dir else {
        return match tokio::time::timeout(timeout, tokio::net::TcpStream::connect((host, port_u16)))
            .await
        {
            Ok(Ok(_)) => (AdCheckStatus::Ok, None),
            Ok(Err(_)) => (AdCheckStatus::Offline, Some("tcp probe failed".to_string())),
            Err(_) if timeout_is_platform_limited => (
                AdCheckStatus::InternalError,
                Some("platform deadline exhausted the checker probe budget".to_string()),
            ),
            Err(_) => (
                AdCheckStatus::Offline,
                Some("tcp probe timed out".to_string()),
            ),
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
        let address =
            match tokio::time::timeout(timeout, tokio::net::lookup_host((host, port_u16))).await {
                Ok(Ok(mut addresses)) => addresses.next(),
                Ok(Err(_)) => None,
                Err(_) => {
                    return (
                        exhausted_budget_status(timeout_is_platform_limited),
                        Some(
                            if timeout_is_platform_limited {
                                "platform deadline exhausted target resolution"
                            } else {
                                "checker target resolution timed out"
                            }
                            .to_string(),
                        ),
                    );
                }
            };
        let Some(address) = address else {
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
            exhausted_budget_status(timeout_is_platform_limited),
            Some(
                if timeout_is_platform_limited {
                    "platform deadline exhausted target resolution"
                } else {
                    "checker target resolution timed out"
                }
                .to_string(),
            ),
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
        Ok(super::sandbox::SandboxOutcome::Exit(code)) => (
            map_exit_code(code as i64, true),
            (code != 0).then(|| format!("checker exit {code}")),
        ),
        Ok(super::sandbox::SandboxOutcome::TimedOut) if timeout_is_platform_limited => (
            AdCheckStatus::InternalError,
            Some("platform deadline exhausted the checker probe budget".to_string()),
        ),
        Ok(super::sandbox::SandboxOutcome::TimedOut) => (
            AdCheckStatus::Offline,
            Some("checker timed out".to_string()),
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
    use sqlx::{Connection, PgConnection};

    #[test]
    fn checker_parallelism_and_timeout_are_bounded() {
        assert!((1..=256).contains(&checker_concurrency()));
        assert!((1..=600).contains(&checker_timeout_secs()));
    }

    #[test]
    fn checker_delay_has_a_grace_floor_and_bounded_independent_jitter() {
        let available = std::time::Duration::from_secs(59);
        let schedule =
            checker_schedule(60, 3, 0.5, available, std::time::Duration::from_secs(30)).unwrap();
        let minimum = checker_delay_from_entropy(schedule, 0);
        let maximum = checker_delay_from_entropy(schedule, u64::MAX);
        assert_eq!(minimum, std::time::Duration::from_secs(3));
        assert!(maximum >= minimum);
        assert!(maximum <= std::time::Duration::from_secs(33));
        assert!(
            maximum + schedule.probe_budget + CHECKER_PERSISTENCE_MARGIN <= available,
            "the latest probe must retain its complete budget and persistence margin"
        );
        assert_ne!(
            checker_delay_from_entropy(schedule, 1),
            checker_delay_from_entropy(schedule, 2)
        );
    }

    #[test]
    fn nominal_timeout_expiry_remains_participant_attributed() {
        let default = checker_schedule(
            60,
            3,
            0.5,
            std::time::Duration::from_secs(60),
            std::time::Duration::from_secs(30),
        )
        .unwrap();
        assert_eq!(default.probe_budget, std::time::Duration::from_secs(30));

        // Even an extreme accepted short-tick configuration defines its
        // reduced budget nominally. It is not mislabeled as a runtime outage.
        let short = checker_schedule(
            30,
            18,
            0.5,
            std::time::Duration::from_secs(22),
            std::time::Duration::from_secs(30),
        )
        .unwrap();
        assert_eq!(short.probe_budget, std::time::Duration::from_secs(1));
        assert_eq!(
            exhausted_budget_status(probe_budget_is_platform_limited(
                short.probe_budget,
                short.probe_budget,
            )),
            AdCheckStatus::Offline
        );
    }

    #[test]
    fn pending_roster_requires_unresolved_identity_matched_delivery() {
        assert!(ad::PENDING_AD_SERVICES_SQL.contains("result.sla_credit IS NULL"));
        assert!(ad::PENDING_AD_SERVICES_SQL.contains("delivery.delivered = TRUE"));
        assert!(ad::PENDING_AD_SERVICES_SQL
            .contains("delivery.container_id IS NOT DISTINCT FROM service.container_id"));
    }

    #[tokio::test]
    async fn late_service_delivery_without_checker_runway_is_offline() {
        let result = ad::classify_no_window_for_test(AdCheckStatus::Offline).await;
        assert_eq!(result.status, AdCheckStatus::Offline);
        assert_eq!(
            result.message.as_deref(),
            Some("flag delivery completed too late for a full checker probe")
        );
    }

    #[test]
    fn delayed_delivery_keeps_each_service_on_its_own_safe_window() {
        let wall_now = Utc::now();
        let monotonic_now = tokio::time::Instant::now();
        // Six seconds of participant-controlled delivery latency leave 23s in
        // a 30s tick. The validated max grace still retains the one-second
        // nominal participant probe plus jitter, persistence, and the outer
        // scheduler margin.
        let deadline = monotonic_now + std::time::Duration::from_secs(23);
        let nominal = checker_schedule(
            30,
            18,
            0.5,
            std::time::Duration::from_secs(22),
            std::time::Duration::from_secs(30),
        )
        .unwrap();
        let cap = deadline_limited_probe_budget(
            deadline,
            nominal.probe_budget,
            CHECKER_PERSISTENCE_MARGIN,
            monotonic_now,
        )
        .unwrap();
        let delayed_available =
            checker_available_after_delivery(wall_now, wall_now, monotonic_now, deadline);
        let delayed = checker_schedule(30, 18, 0.5, delayed_available, cap).unwrap();
        assert_eq!(delayed.probe_budget, nominal.probe_budget);
        assert!(!probe_budget_is_platform_limited(
            delayed.probe_budget,
            nominal.probe_budget,
        ));

        // A service delivered six seconds earlier keeps its own larger jitter
        // window; the slow service cannot shift or void it.
        let early_available = checker_available_after_delivery(
            wall_now - chrono::Duration::seconds(6),
            wall_now,
            monotonic_now,
            deadline,
        );
        let early = checker_schedule(30, 18, 0.5, early_available, cap).unwrap();
        assert!(early.maximum_jitter > delayed.maximum_jitter);
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn recovery_roster_excludes_completed_replaced_and_midround_services() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
        let mut connection = PgConnection::connect(&database_url).await.unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TEMP TABLE "AdCheckResults" (
              round_id INTEGER NOT NULL, team_service_id INTEGER NOT NULL,
              sla_credit DOUBLE PRECISION
            );
            CREATE TEMP TABLE "AdFlags" (
              round_id INTEGER NOT NULL, team_service_id INTEGER NOT NULL
            );
            CREATE TEMP TABLE "AdFlagDeliveryResults" (
              round_id INTEGER NOT NULL, team_service_id INTEGER NOT NULL,
              delivery_kind TEXT NOT NULL, container_id TEXT, delivered BOOLEAN NOT NULL,
              completed_at TIMESTAMPTZ NOT NULL
            );
            CREATE TEMP TABLE "AdTeamServices" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, container_id TEXT
            );
            INSERT INTO "AdTeamServices" VALUES
              (1,7,'one'), (2,7,'two'), (3,7,'replacement'), (4,7,NULL), (5,7,'five');
            INSERT INTO "AdCheckResults" VALUES
              (101,1,NULL), (101,2,1.0), (101,3,NULL), (101,4,NULL), (101,5,NULL);
            INSERT INTO "AdFlags" VALUES (101,1), (101,2), (101,3), (101,4);
            INSERT INTO "AdFlagDeliveryResults" VALUES
              (101,1,'Managed','one',TRUE,clock_timestamp()),
              (101,2,'Managed','two',TRUE,clock_timestamp()),
              (101,3,'Managed','old-three',TRUE,clock_timestamp()),
              (101,4,'External',NULL,TRUE,clock_timestamp());
            "#,
        )
        .execute(&mut connection)
        .await
        .unwrap();
        let mut pending: Vec<i32> =
            sqlx::query_as::<_, (i32, chrono::DateTime<Utc>)>(ad::PENDING_AD_SERVICES_SQL)
                .bind(101_i32)
                .bind(7_i32)
                .fetch_all(&mut connection)
                .await
                .unwrap()
                .into_iter()
                .map(|row| row.0)
                .collect();
        pending.sort_unstable();
        assert_eq!(pending, vec![1, 4]);
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn checker_timing_uses_the_authoritative_round_duration() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
        let mut connection = PgConnection::connect(&database_url).await.unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TEMP TABLE "Games" (
              id INTEGER PRIMARY KEY, ad_tick_seconds INTEGER,
              ad_getflag_window_fraction DOUBLE PRECISION,
              ad_min_grace_period_seconds INTEGER
            );
            CREATE TEMP TABLE "AdRounds" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              start_time_utc TIMESTAMPTZ NOT NULL, end_time_utc TIMESTAMPTZ NOT NULL
            );
            INSERT INTO "Games" VALUES (7, 600, 0.5, 3);
            INSERT INTO "AdRounds" VALUES
              (101, 7, TIMESTAMPTZ '2026-01-01 00:00:00+00',
                       TIMESTAMPTZ '2026-01-01 00:00:30+00');
            "#,
        )
        .execute(&mut connection)
        .await
        .unwrap();
        let timing: (i32, Option<f64>, Option<i32>) = sqlx::query_as(CHECKER_TIMING_SQL)
            .bind(101_i32)
            .bind(7_i32)
            .fetch_one(&mut connection)
            .await
            .unwrap();
        assert_eq!(timing, (30, Some(0.5), Some(3)));
    }

    #[test]
    fn checker_schedule_respects_short_ticks_and_the_outer_pipeline_cap() {
        for (tick, grace, available) in [(30, 18, 22), (600, 60, 240)] {
            let available = std::time::Duration::from_secs(available);
            let schedule = checker_schedule(
                tick,
                grace,
                0.9,
                available,
                std::time::Duration::from_secs(600),
            )
            .unwrap();
            let latest = checker_delay_from_entropy(schedule, u64::MAX);
            assert!(latest + schedule.probe_budget + CHECKER_PERSISTENCE_MARGIN <= available);
            assert!(schedule.probe_budget >= MIN_CHECKER_PROBE_BUDGET);
        }
    }

    #[test]
    fn checker_schedule_rejects_a_window_without_a_real_probe_budget() {
        assert!(checker_schedule(
            30,
            25,
            0.5,
            std::time::Duration::from_millis(27_999),
            std::time::Duration::from_secs(30),
        )
        .is_none());
    }

    #[test]
    fn checker_start_preserves_grace_even_when_the_round_deadline_is_too_close() {
        let wall_now = Utc::now();
        let monotonic_now = tokio::time::Instant::now();
        let starts = checker_start_instant(
            wall_now,
            std::time::Duration::from_secs(30),
            wall_now,
            monotonic_now,
        );
        assert_eq!(starts, monotonic_now + std::time::Duration::from_secs(30));
    }

    #[test]
    fn late_probe_is_rejected_instead_of_receiving_a_shortened_timeout() {
        let now = tokio::time::Instant::now();
        assert!(checker_probe_can_start(
            now + std::time::Duration::from_secs(10),
            std::time::Duration::from_secs(8),
            CHECKER_PERSISTENCE_MARGIN,
            now,
        ));
        assert!(!checker_probe_can_start(
            now + std::time::Duration::from_secs(9),
            std::time::Duration::from_secs(8),
            CHECKER_PERSISTENCE_MARGIN,
            now,
        ));
    }

    #[test]
    fn deadline_budget_is_planned_once_and_never_shortened_at_probe_start() {
        let now = tokio::time::Instant::now();
        let deadline = now + std::time::Duration::from_secs(20);
        assert_eq!(
            deadline_limited_probe_budget(
                deadline,
                std::time::Duration::from_secs(30),
                CHECKER_PERSISTENCE_MARGIN,
                now,
            ),
            Some(std::time::Duration::from_secs(9))
        );
        assert_eq!(
            deadline_limited_probe_budget(
                now + CHECKER_PERSISTENCE_MARGIN,
                std::time::Duration::from_secs(30),
                CHECKER_PERSISTENCE_MARGIN,
                now,
            ),
            None
        );
    }

    #[test]
    fn late_recovery_replans_one_safe_pass_from_the_remaining_runway() {
        let now = tokio::time::Instant::now();
        let deadline = now + std::time::Duration::from_secs(7);
        let cap = deadline_limited_probe_budget(
            deadline,
            std::time::Duration::from_secs(30),
            CHECKER_PERSISTENCE_MARGIN,
            now,
        )
        .unwrap();
        let schedule =
            checker_schedule(60, 3, 0.5, std::time::Duration::from_secs(59), cap).unwrap();
        assert_eq!(
            schedule.probe_budget,
            std::time::Duration::from_millis(2_500)
        );
        assert!(checker_probe_can_start(
            deadline,
            schedule.probe_budget,
            CHECKER_PERSISTENCE_MARGIN,
            now + std::time::Duration::from_millis(100),
        ));
    }

    #[test]
    fn deadline_limited_timeouts_are_platform_attributed() {
        assert_eq!(exhausted_budget_status(true), AdCheckStatus::InternalError);
        assert_eq!(exhausted_budget_status(false), AdCheckStatus::Offline);
    }
}
