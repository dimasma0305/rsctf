//! Startup configuration for local Docker aggregate admission.
//!
//! CPU and container-count caps are opt-in. Memory defaults to the Docker
//! host's memory minus a reserve for the platform. Individual container limits
//! remain enforced by Docker regardless of aggregate admission settings.

use crate::utils::error::{AppError, AppResult};

pub const CPU_MILLIS_ENV: &str = "RSCTF_LOCAL_CONTAINER_CPU_MILLIS";
pub const MEMORY_BYTES_ENV: &str = "RSCTF_LOCAL_CONTAINER_MEMORY_BYTES";
pub const SLOTS_ENV: &str = "RSCTF_LOCAL_CONTAINER_SLOTS";
pub const RESERVE_FRACTION_ENV: &str = "RSCTF_LOCAL_CONTAINER_RESERVE_FRACTION";
pub const RESERVE_CPU_MILLIS_ENV: &str = "RSCTF_LOCAL_CONTAINER_RESERVE_CPU_MILLIS";
pub const RESERVE_MEMORY_BYTES_ENV: &str = "RSCTF_LOCAL_CONTAINER_RESERVE_MEMORY_BYTES";

/// Default share of the host kept back from challenge containers.
pub const DEFAULT_RESERVE_FRACTION: f64 = 0.25;
/// Absolute floors for the reserve when the fractional share is smaller.
pub const DEFAULT_RESERVE_MEMORY_BYTES: i64 = 2 * 1024 * 1024 * 1024;
/// A small development host still admits at least one ordinary container.
pub const MINIMUM_CPU_MILLIS: i64 = 1_000;
pub const MINIMUM_MEMORY_BYTES: i64 = 1024 * 1024 * 1024;
const MAX_CPU_MILLIS: i64 = 4_096_000;
const MAX_MEMORY_BYTES: i64 = 1 << 50;
const MAX_SLOTS: i32 = 65_535;
const MAX_RESERVE_FRACTION: f64 = 0.9;

/// Physical resources reported for the Docker daemon host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostResources {
    pub cpu_millis: i64,
    pub memory_bytes: i64,
}

/// Aggregate ceiling every admitted local container counts against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalCapacity {
    pub cpu_millis: Option<i64>,
    pub memory_bytes: i64,
    pub slots: Option<i32>,
}

fn invalid(name: &str, detail: &str) -> AppError {
    AppError::internal(format!("{name} {detail}"))
}

fn parse_bounded_i64(
    env: &dyn Fn(&str) -> Option<String>,
    name: &str,
    range: std::ops::RangeInclusive<i64>,
) -> AppResult<Option<i64>> {
    let Some(raw) = env(name) else {
        return Ok(None);
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let value = raw
        .parse::<i64>()
        .map_err(|_| invalid(name, "must be an integer"))?;
    if !range.contains(&value) {
        return Err(invalid(
            name,
            &format!("must be between {} and {}", range.start(), range.end()),
        ));
    }
    Ok(Some(value))
}

fn parse_reserve_fraction(env: &dyn Fn(&str) -> Option<String>) -> AppResult<f64> {
    let Some(raw) = env(RESERVE_FRACTION_ENV) else {
        return Ok(DEFAULT_RESERVE_FRACTION);
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(DEFAULT_RESERVE_FRACTION);
    }
    let value = raw
        .parse::<f64>()
        .map_err(|_| invalid(RESERVE_FRACTION_ENV, "must be a decimal fraction"))?;
    if !value.is_finite() || !(0.0..=MAX_RESERVE_FRACTION).contains(&value) {
        return Err(invalid(
            RESERVE_FRACTION_ENV,
            &format!("must be between 0 and {MAX_RESERVE_FRACTION}"),
        ));
    }
    Ok(value)
}

fn derived(total: i64, fraction: f64, absolute_reserve: i64, minimum: i64) -> i64 {
    // The fraction is bounded to [0, 0.9], so the product stays well inside
    // i64 for every accepted total.
    let fractional = (total as f64 * fraction) as i64;
    let reserve = fractional.max(absolute_reserve);
    total.saturating_sub(reserve).max(minimum)
}

/// Resolve the enforced capacity from the environment and the detected host.
pub fn resolve(
    env: &dyn Fn(&str) -> Option<String>,
    host: HostResources,
) -> AppResult<LocalCapacity> {
    let fraction = parse_reserve_fraction(env)?;
    // Validate the legacy setting for compatibility, but CPU is no longer
    // reserved from the host. Only an explicit aggregate CPU cap is enforced.
    parse_bounded_i64(env, RESERVE_CPU_MILLIS_ENV, 0..=MAX_CPU_MILLIS)?;
    let reserve_memory = parse_bounded_i64(env, RESERVE_MEMORY_BYTES_ENV, 0..=MAX_MEMORY_BYTES)?
        .unwrap_or(DEFAULT_RESERVE_MEMORY_BYTES);
    let cpu_millis =
        parse_bounded_i64(env, CPU_MILLIS_ENV, 0..=MAX_CPU_MILLIS)?.filter(|value| *value != 0);
    let memory_bytes = match parse_bounded_i64(env, MEMORY_BYTES_ENV, 1..=MAX_MEMORY_BYTES)? {
        Some(explicit) => explicit,
        None => derived(
            host.memory_bytes,
            fraction,
            reserve_memory,
            MINIMUM_MEMORY_BYTES,
        ),
    };
    let slots = parse_bounded_i64(env, SLOTS_ENV, 0..=i64::from(MAX_SLOTS))?
        .filter(|value| *value != 0)
        .map(|value| i32::try_from(value).expect("bounded slots fit in i32"));
    if memory_bytes > host.memory_bytes {
        tracing::warn!(
            cpu_millis = ?cpu_millis,
            memory_bytes,
            host_cpu_millis = host.cpu_millis,
            host_memory_bytes = host.memory_bytes,
            "explicit local container capacity exceeds the detected Docker host"
        );
    }
    Ok(LocalCapacity {
        cpu_millis,
        memory_bytes,
        slots,
    })
}

/// Read the configuration from the process environment.
pub fn from_env(host: HostResources) -> AppResult<LocalCapacity> {
    resolve(&|name| std::env::var(name).ok(), host)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const GIB: i64 = 1024 * 1024 * 1024;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    fn resolve_with(pairs: &[(&str, &str)], host: HostResources) -> AppResult<LocalCapacity> {
        let vars = env(pairs);
        resolve(&|name| vars.get(name).cloned(), host)
    }

    const LARGE_HOST: HostResources = HostResources {
        cpu_millis: 16_000,
        memory_bytes: 64 * GIB,
    };

    #[test]
    fn defaults_subtract_the_larger_of_fraction_and_absolute_reserve() {
        let capacity = resolve_with(&[], LARGE_HOST).unwrap();
        assert_eq!(capacity.cpu_millis, None);
        assert_eq!(capacity.memory_bytes, 48 * GIB);
        assert_eq!(capacity.slots, None);

        let small = resolve_with(
            &[],
            HostResources {
                cpu_millis: 2_000,
                memory_bytes: 4 * GIB,
            },
        )
        .unwrap();
        assert_eq!(small.cpu_millis, None);
        assert_eq!(small.memory_bytes, 2 * GIB);
        assert_eq!(small.slots, None);
    }

    #[test]
    fn a_tiny_development_host_still_admits_the_documented_minimum() {
        let tiny = resolve_with(
            &[],
            HostResources {
                cpu_millis: 1_000,
                memory_bytes: GIB,
            },
        )
        .unwrap();
        assert_eq!(tiny.cpu_millis, None);
        assert_eq!(tiny.memory_bytes, MINIMUM_MEMORY_BYTES);
        assert_eq!(tiny.slots, None);
    }

    #[test]
    fn explicit_values_override_without_a_reserve() {
        let capacity = resolve_with(
            &[
                (CPU_MILLIS_ENV, "15000"),
                (MEMORY_BYTES_ENV, "17179869184"),
                (SLOTS_ENV, "12"),
            ],
            LARGE_HOST,
        )
        .unwrap();
        assert_eq!(
            capacity,
            LocalCapacity {
                cpu_millis: Some(15_000),
                memory_bytes: 16 * GIB,
                slots: Some(12),
            }
        );
    }

    #[test]
    fn reserve_overrides_change_the_derived_default() {
        let capacity = resolve_with(
            &[
                (RESERVE_FRACTION_ENV, "0.5"),
                (RESERVE_CPU_MILLIS_ENV, "0"),
                (RESERVE_MEMORY_BYTES_ENV, "0"),
            ],
            LARGE_HOST,
        )
        .unwrap();
        assert_eq!(capacity.cpu_millis, None);
        assert_eq!(capacity.memory_bytes, 32 * GIB);
    }

    #[test]
    fn out_of_range_values_fail_startup() {
        for pairs in [
            vec![(CPU_MILLIS_ENV, "-1")],
            vec![(CPU_MILLIS_ENV, "abc")],
            vec![(MEMORY_BYTES_ENV, "-1")],
            vec![(SLOTS_ENV, "65536")],
            vec![(SLOTS_ENV, "-1")],
            vec![(RESERVE_FRACTION_ENV, "0.95")],
            vec![(RESERVE_FRACTION_ENV, "-0.1")],
            vec![(RESERVE_FRACTION_ENV, "nan")],
            vec![(RESERVE_CPU_MILLIS_ENV, "-5")],
        ] {
            assert!(
                resolve_with(&pairs, LARGE_HOST).is_err(),
                "{pairs:?} must be rejected"
            );
        }
        assert!(resolve_with(&[(CPU_MILLIS_ENV, " ")], LARGE_HOST).is_ok());
    }

    #[test]
    fn absent_blank_and_zero_caps_allow_cpu_sharing_without_a_container_count_cap() {
        let host = HostResources {
            cpu_millis: 8_000,
            memory_bytes: 16 * GIB,
        };
        let shared = resolve_with(&[], host).unwrap();
        assert_eq!(
            shared,
            LocalCapacity {
                cpu_millis: None,
                memory_bytes: 12 * GIB,
                slots: None
            }
        );
        for value in ["", " ", "0"] {
            assert_eq!(
                resolve_with(&[(CPU_MILLIS_ENV, value), (SLOTS_ENV, value)], host).unwrap(),
                shared
            );
        }
        let explicit = resolve_with(&[(CPU_MILLIS_ENV, "6000")], host).unwrap();
        assert_eq!(
            explicit,
            LocalCapacity {
                cpu_millis: Some(6_000),
                ..shared
            }
        );
        let slots = resolve_with(&[(SLOTS_ENV, "6")], host).unwrap();
        assert_eq!(
            slots,
            LocalCapacity {
                slots: Some(6),
                ..shared
            }
        );
    }

    #[test]
    fn legacy_cpu_reserve_does_not_reintroduce_a_cpu_or_slot_cap() {
        assert_eq!(
            resolve_with(&[(RESERVE_CPU_MILLIS_ENV, "6000")], LARGE_HOST).unwrap(),
            resolve_with(&[], LARGE_HOST).unwrap()
        );
    }
}
