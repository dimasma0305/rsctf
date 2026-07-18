//! Bounded Docker logging for platform-managed challenge containers.

use std::collections::HashMap;

use bollard::models::HostConfigLogConfig;

const DEFAULT_MAX_SIZE_MIB: usize = 5;
const ABSOLUTE_MAX_SIZE_MIB: usize = 16;
const DEFAULT_MAX_FILES: usize = 3;
const ABSOLUTE_MAX_FILES: usize = 5;

fn bounded_value(value: Option<&str>, maximum: usize, default: usize) -> usize {
    value
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| (1..=maximum).contains(value))
        .unwrap_or(default)
}

pub(super) fn bounded_log_config() -> HostConfigLogConfig {
    let max_size_mib = bounded_value(
        std::env::var("RSCTF_CONTAINER_LOG_MAX_SIZE_MIB")
            .ok()
            .as_deref(),
        ABSOLUTE_MAX_SIZE_MIB,
        DEFAULT_MAX_SIZE_MIB,
    );
    let max_files = bounded_value(
        std::env::var("RSCTF_CONTAINER_LOG_MAX_FILES")
            .ok()
            .as_deref(),
        ABSOLUTE_MAX_FILES,
        DEFAULT_MAX_FILES,
    );
    HostConfigLogConfig {
        typ: Some("json-file".into()),
        config: Some(HashMap::from([
            ("max-size".into(), format!("{max_size_mib}m")),
            ("max-file".into(), max_files.to_string()),
        ])),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_limits_reject_zero_and_values_above_the_hard_ceiling() {
        assert_eq!(bounded_value(Some("0"), 16, 5), 5);
        assert_eq!(bounded_value(Some("17"), 16, 5), 5);
        assert_eq!(bounded_value(Some("16"), 16, 5), 16);
        assert_eq!(bounded_value(Some("invalid"), 16, 5), 5);
    }
}
