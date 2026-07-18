use bollard::models::ContainerStateStatusEnum;

use super::TEAM_ENV;

/// A readable collision-safe name. A stable operation gets a deterministic
/// suffix so recovery can adopt the same backend workload.
pub(super) fn container_name(
    image: &str,
    env: &[(String, String)],
    operation_id: Option<&str>,
) -> String {
    let base = image.split_once('@').map_or(image, |(image, _)| image);
    let base = base.rsplit_once(':').map_or(base, |(image, _)| image);
    let mut name: String = base
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | '-') {
                character
            } else {
                '-'
            }
        })
        .collect();
    if let Some((_, team)) = env.iter().find(|(key, _)| key == TEAM_ENV) {
        name.push_str("-t");
        name.push_str(team);
    }
    name.push('-');
    if let Some(operation_id) = operation_id {
        let digest = crate::utils::codec::sha256_str(operation_id);
        name.push_str(&digest[..12]);
    } else {
        name.push_str(&uuid::Uuid::new_v4().simple().to_string()[..12]);
    }
    let name = name.trim_matches('-').to_string();
    (!name.is_empty())
        .then_some(name)
        .unwrap_or_else(|| "rsctf-container".to_string())
}

pub(super) fn map_status(state: Option<ContainerStateStatusEnum>) -> &'static str {
    match state {
        Some(ContainerStateStatusEnum::RUNNING) => "running",
        Some(ContainerStateStatusEnum::CREATED) => "pending",
        Some(ContainerStateStatusEnum::PAUSED) => "paused",
        Some(ContainerStateStatusEnum::RESTARTING) => "restarting",
        Some(ContainerStateStatusEnum::REMOVING) => "removing",
        Some(ContainerStateStatusEnum::EXITED) => "exited",
        Some(ContainerStateStatusEnum::DEAD) => "destroyed",
        _ => "pending",
    }
}
