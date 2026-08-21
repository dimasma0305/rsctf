use std::collections::HashMap;

use bollard::models::SystemInfo;

pub(in crate::runtime::docker) fn storage_quota_supported(info: &SystemInfo) -> bool {
    match info.driver.as_deref() {
        Some("btrfs" | "zfs" | "windowsfilter") => true,
        Some("overlay2") => info
            .driver_status
            .as_ref()
            .into_iter()
            .flatten()
            .any(|entry| {
                entry.first().map(String::as_str) == Some("Backing Filesystem")
                    && entry
                        .get(1)
                        .is_some_and(|value| value.eq_ignore_ascii_case("xfs"))
            }),
        _ => false,
    }
}

pub(in crate::runtime::docker) fn writable_layer_storage_opt(
    bytes: u64,
) -> HashMap<String, String> {
    const MIB: u64 = 1024 * 1024;
    HashMap::from([("size".to_string(), format!("{}M", bytes.div_ceil(MIB)))])
}

/// The worker-wide setting is an operator safety ceiling; a workload may ask
/// for a smaller layer but can never expand past that ceiling. `None` is kept
/// only for the explicit development-only unbounded-storage escape hatch.
pub(in crate::runtime::docker) fn effective_writable_layer_limit(
    worker_maximum: Option<u64>,
    requested: u64,
) -> Option<u64> {
    worker_maximum.map(|maximum| maximum.min(requested))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windowsfilter_supports_per_container_writable_layer_limits() {
        let info = SystemInfo {
            driver: Some("windowsfilter".to_string()),
            ..Default::default()
        };
        assert!(storage_quota_supported(&info));
    }

    #[test]
    fn workload_limit_cannot_exceed_the_worker_ceiling() {
        assert_eq!(effective_writable_layer_limit(Some(512), 128), Some(128));
        assert_eq!(effective_writable_layer_limit(Some(512), 1_024), Some(512));
        assert_eq!(effective_writable_layer_limit(None, 128), None);
    }

    #[test]
    fn quota_detection_is_fail_closed() {
        let xfs = SystemInfo {
            driver: Some("overlay2".to_string()),
            driver_status: Some(vec![vec![
                "Backing Filesystem".to_string(),
                "xfs".to_string(),
            ]]),
            ..Default::default()
        };
        assert!(storage_quota_supported(&xfs));

        let ext = SystemInfo {
            driver: Some("overlay2".to_string()),
            driver_status: Some(vec![vec![
                "Backing Filesystem".to_string(),
                "extfs".to_string(),
            ]]),
            ..Default::default()
        };
        assert!(!storage_quota_supported(&ext));
        assert_eq!(
            writable_layer_storage_opt(512 * 1024 * 1024),
            HashMap::from([("size".to_string(), "512M".to_string())])
        );
    }
}
