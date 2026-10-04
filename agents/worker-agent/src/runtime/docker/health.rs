//! Inherited image health checks: keep the image's probe, never poll faster
//! than the steady floor.

use bollard::models::{HealthConfig, ImageInspect};

/// Steady-state floor for an inherited image health check. Docker runs each
/// probe as a container exec, so a fleet of images polling every second or two
/// turns into daemon CPU and process churn at event scale.
pub(super) const MIN_HEALTH_INTERVAL_NANOS: i64 = 15_000_000_000;

pub(super) fn image_health_config(image: &ImageInspect) -> Option<HealthConfig> {
    image.config.as_ref()?.healthcheck.clone()
}

/// Inherit the image's health check but never poll faster than
/// [`MIN_HEALTH_INTERVAL_NANOS`] once the start period has elapsed.
///
/// Returns `None` whenever the container should simply inherit the image
/// definition: no health check, an explicitly disabled one (`NONE`), or an
/// interval that is already at or above the floor. A container-level
/// `Healthcheck` replaces the image's whole block rather than merging into it,
/// so the clamp clones every field (command, timeout, retries, start period,
/// start interval) and changes only the steady interval.
pub(super) fn clamped_health_config(health: Option<&HealthConfig>) -> Option<HealthConfig> {
    let health = health?;
    let test = health.test.as_ref()?;
    if test.is_empty() || test.first().is_some_and(|command| command == "NONE") {
        return None;
    }
    health
        .interval
        .filter(|interval| *interval > 0 && *interval < MIN_HEALTH_INTERVAL_NANOS)?;
    Some(HealthConfig {
        interval: Some(MIN_HEALTH_INTERVAL_NANOS),
        ..health.clone()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inherited_image_health_checks_are_clamped_to_the_steady_floor() {
        let second = 1_000_000_000_i64;
        let fast = HealthConfig {
            test: Some(vec!["CMD".to_string(), "/probe".to_string()]),
            interval: Some(2 * second),
            timeout: Some(3 * second),
            retries: Some(7),
            start_period: Some(40 * second),
            start_interval: Some(second),
        };

        // Below the floor: only the steady interval changes.
        let clamped = clamped_health_config(Some(&fast)).expect("clamped");
        assert_eq!(clamped.interval, Some(MIN_HEALTH_INTERVAL_NANOS));
        assert_eq!(MIN_HEALTH_INTERVAL_NANOS, 15 * second);
        assert_eq!(clamped.test, fast.test);
        assert_eq!(clamped.timeout, fast.timeout);
        assert_eq!(clamped.retries, fast.retries);
        assert_eq!(clamped.start_period, fast.start_period);
        assert_eq!(clamped.start_interval, fast.start_interval);

        // At or above the floor, or left to Docker's 30 s default: inherit.
        for interval in [Some(15 * second), Some(60 * second), Some(0), None] {
            let slow = HealthConfig {
                interval,
                ..fast.clone()
            };
            assert_eq!(clamped_health_config(Some(&slow)), None);
        }

        // No health check, a disabled one, or an empty command never gains a
        // probe.
        assert_eq!(clamped_health_config(None), None);
        let disabled = HealthConfig {
            test: Some(vec!["NONE".to_string()]),
            ..fast.clone()
        };
        assert_eq!(clamped_health_config(Some(&disabled)), None);
        let no_command = HealthConfig { test: None, ..fast };
        assert_eq!(clamped_health_config(Some(&no_command)), None);

        let image = ImageInspect {
            config: Some(bollard::models::ContainerConfig {
                healthcheck: Some(no_command.clone()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(image_health_config(&image), Some(no_command));
        assert_eq!(image_health_config(&ImageInspect::default()), None);
    }
}
