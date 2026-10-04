//! Host-config construction: Docker `init`, PID bounds, inherited health-check
//! clamping, and the bounded local log driver.

use bollard::models::{ContainerConfig, HealthConfig, ImageInspect};

use super::bounded_log_config;
use super::docker::{
    challenge_host_config, clamped_image_health_config, docker_network_mode,
    restricted_tmpfs_mounts, MIN_HEALTH_INTERVAL_NANOS,
};
use super::tests::fingerprint_spec;

#[test]
fn linux_challenge_containers_run_under_init_with_bounded_pids() {
    // The local backend never creates Windows containers (the worker agent owns
    // that path), so every HostConfig it builds is a Linux one.
    let spec = fingerprint_spec();
    let config = challenge_host_config(&spec, false, None, None);

    assert_eq!(config.init, Some(true));
    assert_eq!(config.pids_limit, Some(512));
    assert_eq!(
        config.memory,
        Some(i64::from(spec.memory_limit) * 1024 * 1024)
    );
    assert_eq!(
        config.nano_cpus,
        Some(i64::from(spec.cpu_count) * 1_000_000_000)
    );
    assert_eq!(config.network_mode, docker_network_mode(&spec));
    assert_eq!(config.cap_drop, None);
    assert_eq!(config.readonly_rootfs, None);
    assert!(config.log_config.is_some());

    let restricted = challenge_host_config(&spec, true, None, None);
    assert_eq!(restricted.init, Some(true));
    assert_eq!(restricted.cap_drop, Some(vec!["ALL".to_string()]));
    assert_eq!(restricted.readonly_rootfs, Some(true));
    assert_eq!(restricted.tmpfs, Some(restricted_tmpfs_mounts()));
}

#[test]
fn inherited_image_health_checks_are_clamped_to_the_steady_floor() {
    let second = 1_000_000_000_i64;
    let image = |healthcheck: Option<HealthConfig>| ImageInspect {
        config: Some(ContainerConfig {
            healthcheck,
            ..Default::default()
        }),
        ..Default::default()
    };
    let fast = HealthConfig {
        test: Some(vec![
            "CMD-SHELL".to_string(),
            "curl -f localhost".to_string(),
        ]),
        interval: Some(2 * second),
        timeout: Some(3 * second),
        retries: Some(7),
        start_period: Some(40 * second),
        start_interval: Some(second),
    };

    // Below the floor: only the steady interval changes.
    let clamped = clamped_image_health_config(&image(Some(fast.clone()))).expect("clamped");
    assert_eq!(clamped.interval, Some(MIN_HEALTH_INTERVAL_NANOS));
    assert_eq!(MIN_HEALTH_INTERVAL_NANOS, 15 * second);
    assert_eq!(clamped.test, fast.test);
    assert_eq!(clamped.timeout, fast.timeout);
    assert_eq!(clamped.retries, fast.retries);
    assert_eq!(clamped.start_period, fast.start_period);
    assert_eq!(clamped.start_interval, fast.start_interval);

    // At or above the floor, or left to Docker's 30 s default: inherit as is.
    for interval in [Some(15 * second), Some(60 * second), Some(0), None] {
        let slow = HealthConfig {
            interval,
            ..fast.clone()
        };
        assert_eq!(clamped_image_health_config(&image(Some(slow))), None);
    }

    // No health check, an explicitly disabled one, or an empty command never
    // gains a probe.
    assert_eq!(clamped_image_health_config(&image(None)), None);
    assert_eq!(clamped_image_health_config(&ImageInspect::default()), None);
    let disabled = HealthConfig {
        test: Some(vec!["NONE".to_string()]),
        ..fast.clone()
    };
    assert_eq!(clamped_image_health_config(&image(Some(disabled))), None);
    let inherit_only = HealthConfig {
        test: Some(Vec::new()),
        ..fast.clone()
    };
    assert_eq!(
        clamped_image_health_config(&image(Some(inherit_only))),
        None
    );
    let no_command = HealthConfig { test: None, ..fast };
    assert_eq!(clamped_image_health_config(&image(Some(no_command))), None);
}

#[test]
fn challenge_container_logs_are_bounded() {
    let log_config = bounded_log_config();
    let options = log_config.config.expect("local driver options");

    assert_eq!(log_config.typ.as_deref(), Some("local"));
    assert_eq!(options.get("max-size").map(String::as_str), Some("5m"));
    assert_eq!(options.get("max-file").map(String::as_str), Some("3"));
}
