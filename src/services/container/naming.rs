use bollard::models::ContainerStateStatusEnum;

use super::TEAM_ENV;

fn readable_name_prefix(image: &str, env: &[(String, String)]) -> String {
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
    name
}

#[cfg(test)]
pub(super) fn legacy_operation_container_name(
    image: &str,
    env: &[(String, String)],
    operation_id: &str,
) -> String {
    let mut name = readable_name_prefix(image, env);
    name.push('-');
    name.push_str(&crate::utils::codec::sha256_str(operation_id)[..12]);
    name.trim_matches('-').to_string()
}

/// A readable collision-safe name for one-shot launches. A stable operation
/// gets a scope-bound deterministic name that excludes mutable launch fields.
pub(super) fn container_name(
    image: &str,
    env: &[(String, String)],
    operation_id: Option<&str>,
) -> String {
    if let Some(operation_id) = operation_id {
        let digest = crate::utils::codec::sha256_str(operation_id);
        return format!("rsctf-operation-{}", &digest[..32]);
    }
    let mut name = readable_name_prefix(image, env);
    name.push('-');
    name.push_str(&uuid::Uuid::new_v4().simple().to_string()[..12]);
    let name = name.trim_matches('-').to_string();
    if name.is_empty() {
        "rsctf-container".to_string()
    } else {
        name
    }
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
