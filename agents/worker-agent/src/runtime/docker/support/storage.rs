use std::collections::HashMap;

use bollard::container::{Config, CreateContainerOptions, RemoveContainerOptions};
use bollard::image::{BuildImageOptions, RemoveImageOptions};
use bollard::models::{HostConfig, SystemInfo};
use bollard::Docker;
use futures_util::StreamExt;
use rsctf_worker_protocol::CommandErrorCode;
use uuid::Uuid;

use crate::runtime::RuntimeError;

use super::make_single_file_archive;

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

/// Moby does not expose overlay2's `projectQuotaSupported` flag in `/info`.
/// Build a layerless scratch image and create (but never start) a disposable
/// container with `storage-opt=size`; overlay2 rejects that create unless XFS
/// project quotas are genuinely active.
pub(in crate::runtime::docker) async fn verify_storage_quota_support(
    docker: &Docker,
    info: &SystemInfo,
) -> Result<(), RuntimeError> {
    if !storage_quota_supported(info) {
        return Err(RuntimeError::unsupported(
            "Docker storage driver cannot enforce per-container writable-layer quotas; configure overlay2 on XFS with project quotas or native Windows windowsfilter",
        ));
    }
    if info.driver.as_deref() != Some("overlay2") {
        return Ok(());
    }

    let suffix = Uuid::new_v4().simple().to_string();
    let image = format!("rsctf-worker-quota-probe:{suffix}");
    let container = format!("rsctf-worker-quota-probe-{suffix}");
    let context = make_single_file_archive(
        "Dockerfile".to_string(),
        b"FROM scratch\nCMD [\"/rsctf-quota-probe\"]\n".to_vec(),
    )
    .await?;
    let options = BuildImageOptions::<String> {
        dockerfile: "Dockerfile".to_string(),
        t: image.clone(),
        networkmode: "none".to_string(),
        nocache: true,
        pull: false,
        rm: true,
        forcerm: true,
        ..Default::default()
    };
    let mut created_id = None;
    let probe_result: Result<(), RuntimeError> = async {
        let mut build = docker.build_image(options, None, Some(context));
        while let Some(item) = build.next().await {
            let item = item.map_err(|error| {
                RuntimeError::new(
                    CommandErrorCode::RuntimeUnavailable,
                    format!("build Docker quota probe image: {error}"),
                )
            })?;
            if let Some(error) = item.error {
                return Err(RuntimeError::new(
                    CommandErrorCode::RuntimeUnavailable,
                    format!("build Docker quota probe image: {error}"),
                ));
            }
        }

        let config = Config {
            image: Some(image.clone()),
            host_config: Some(HostConfig {
                network_mode: Some("none".to_string()),
                storage_opt: Some(writable_layer_storage_opt(1024 * 1024)),
                ..Default::default()
            }),
            ..Default::default()
        };
        let created = docker
            .create_container(
                Some(CreateContainerOptions {
                    name: container,
                    platform: None,
                }),
                config,
            )
            .await
            .map_err(|error| {
                RuntimeError::unsupported(format!(
                    "Docker rejected a writable-layer quota probe; verify XFS is mounted with pquota/prjquota: {error}"
                ))
            })?;
        created_id = Some(created.id);
        Ok(())
    }
    .await;

    let mut cleanup_errors = Vec::new();
    if let Some(id) = created_id {
        if let Err(error) = docker
            .remove_container(
                &id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
        {
            cleanup_errors.push(format!("remove Docker quota probe container: {error}"));
        }
    }
    if let Err(error) = docker
        .remove_image(
            &image,
            Some(RemoveImageOptions {
                force: true,
                ..Default::default()
            }),
            None,
        )
        .await
    {
        if !matches!(
            &error,
            bollard::errors::Error::DockerResponseServerError {
                status_code: 404,
                ..
            }
        ) {
            cleanup_errors.push(format!("remove Docker quota probe image: {error}"));
        }
    }
    if !cleanup_errors.is_empty() {
        return Err(RuntimeError::new(
            CommandErrorCode::RuntimeUnavailable,
            cleanup_errors.join("; "),
        ));
    }
    probe_result
}

pub(in crate::runtime::docker) fn writable_layer_storage_opt(
    bytes: u64,
) -> HashMap<String, String> {
    // Moby parses a unitless `size` as bytes. Preserve the exact ceiling: an
    // MiB round-up would grant more writable storage than the scheduler owns.
    HashMap::from([("size".to_string(), bytes.to_string())])
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
            HashMap::from([("size".to_string(), "536870912".to_string())])
        );
        assert_eq!(
            writable_layer_storage_opt(64 * 1024 * 1024 + 1),
            HashMap::from([("size".to_string(), "67108865".to_string())])
        );
    }
}
