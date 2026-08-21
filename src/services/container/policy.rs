use rsctf_worker_protocol::GameKind;

use super::{
    ContainerSpec, DEFAULT_CONTAINER_STORAGE_MB, DEFAULT_MAX_CPU_COUNT, DEFAULT_MAX_MEMORY_MB,
    DEFAULT_MAX_STORAGE_MB,
};
use crate::utils::enums::{ChallengeType, NetworkMode};
use crate::utils::error::{AppError, AppResult};

pub fn storage_limit_or_default(value: Option<i32>) -> i32 {
    let maximum =
        configured_positive_limit("RSCTF_CONTAINER_MAX_STORAGE_MB", DEFAULT_MAX_STORAGE_MB);
    storage_limit_or_default_with_maximum(value, maximum)
}

fn storage_limit_or_default_with_maximum(value: Option<i32>, maximum: i32) -> i32 {
    value.unwrap_or(DEFAULT_CONTAINER_STORAGE_MB.min(maximum))
}

fn configured_positive_limit(name: &str, default: i32) -> i32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<i32>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

pub fn validate_storage_limit_value(storage_limit: i32) -> AppResult<()> {
    let maximum =
        configured_positive_limit("RSCTF_CONTAINER_MAX_STORAGE_MB", DEFAULT_MAX_STORAGE_MB);
    if !(1..=maximum).contains(&storage_limit) {
        return Err(AppError::bad_request(format!(
            "container storage must be between 1 and {maximum} MB"
        )));
    }
    Ok(())
}

pub fn validate_network_mode_value(
    challenge_type: ChallengeType,
    network_mode: NetworkMode,
) -> AppResult<()> {
    if network_mode == NetworkMode::Custom {
        return Err(AppError::bad_request(
            "Custom container networking is not supported",
        ));
    }
    if network_mode == NetworkMode::Isolated && challenge_type.uses_ad_engine() {
        return Err(AppError::bad_request(
            "A&D and KotH services require their managed private network",
        ));
    }
    Ok(())
}

pub(crate) fn validate_container_spec(spec: &ContainerSpec) -> AppResult<()> {
    let max_memory =
        configured_positive_limit("RSCTF_CONTAINER_MAX_MEMORY_MB", DEFAULT_MAX_MEMORY_MB);
    let max_cpu = configured_positive_limit("RSCTF_CONTAINER_MAX_CPU_COUNT", DEFAULT_MAX_CPU_COUNT);
    if spec.image.trim().is_empty() {
        return Err(AppError::bad_request("container image is required"));
    }
    if !crate::services::challenge_images::is_repository_digest(&spec.image)
        && !crate::services::challenge_images::is_local_image_id(&spec.image)
        && crate::services::challenge_images::worker_local_image(&spec.image).is_none()
    {
        return Err(AppError::bad_request(
            "container image must be an immutable repository digest, Docker image id, or worker-scoped image id",
        ));
    }
    if !(1..=max_memory).contains(&spec.memory_limit) {
        return Err(AppError::bad_request(format!(
            "container memory must be between 1 and {max_memory} MB"
        )));
    }
    if !(1..=max_cpu).contains(&spec.cpu_count) {
        return Err(AppError::bad_request(format!(
            "container CPU count must be between 1 and {max_cpu}"
        )));
    }
    validate_storage_limit_value(spec.storage_limit)?;
    if !(1..=65_535).contains(&spec.expose_port) {
        return Err(AppError::bad_request(
            "container expose port must be between 1 and 65535",
        ));
    }
    if spec.proxy_only
        && (!spec.publish_port || spec.ad_network.is_some() || spec.game_kind != GameKind::Jeopardy)
    {
        return Err(AppError::bad_request(
            "proxy-only publication is valid only for published Jeopardy containers",
        ));
    }
    if spec.network_mode == NetworkMode::Custom {
        return Err(AppError::bad_request(
            "custom container networking is not supported",
        ));
    }
    if spec.network_mode == NetworkMode::Isolated
        && (spec.ad_network.is_some() || spec.allow_egress)
    {
        return Err(AppError::bad_request(
            "isolated containers cannot join the A&D network or allow egress",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_implicit_storage_limit_never_exceeds_the_runtime_ceiling() {
        assert_eq!(storage_limit_or_default_with_maximum(None, 128), 128);
        assert_eq!(storage_limit_or_default_with_maximum(None, 1024), 512);
        assert_eq!(storage_limit_or_default_with_maximum(Some(768), 128), 768);
    }
}
