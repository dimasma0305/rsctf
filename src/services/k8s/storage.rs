use k8s_openapi::api::core::v1::Pod;

use crate::utils::error::{AppError, AppResult};

/// Admission can add storage after the platform submits a Pod. Inspect the
/// persisted spec, including for terminal Pods recovered by a retried launch.
pub(super) fn check_replacement(pod: &Pod) -> AppResult<()> {
    let spec = pod
        .spec
        .as_ref()
        .ok_or_else(|| AppError::conflict("Missing A&D Pod specification"))?;
    for volume in spec.volumes.iter().flatten() {
        if volume.config_map.is_some()
            || volume.secret.is_some()
            || volume.projected.is_some()
            || volume.downward_api.is_some()
        {
            continue;
        }
        let storage = match &volume.persistent_volume_claim {
            Some(claim) => format!("persistent volume claim '{}'", claim.claim_name),
            None => format!("volume '{}'", volume.name),
        };
        return Err(AppError::conflict(format!(
            "The A&D Pod has {storage} which this runtime cannot transfer. Preserve the team's data before replacing the Pod."
        )));
    }
    Ok(())
}
