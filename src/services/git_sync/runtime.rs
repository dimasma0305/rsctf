//! Conservative equivalence checks for repository updates to enabled runtimes.

use std::path::Path;

use crate::app_state::SharedState;
use crate::models::data::game_challenge;
use crate::utils::enums::{ChallengeType, NetworkMode};
use crate::utils::error::{AppError, AppResult};

use super::package::{archived_context_fingerprint, context_fingerprint};

pub(super) struct LiveRuntimeIntent<'a> {
    pub container_image: Option<&'a str>,
    pub declared_container_image: Option<&'a str>,
    pub memory_limit: Option<i32>,
    pub storage_limit: Option<i32>,
    pub cpu_count: Option<i32>,
    pub expose_port: Option<i32>,
    pub flag_template: Option<&'a str>,
    pub build_context_subdir: Option<&'a str>,
    pub local_build_context: Option<&'a Path>,
    pub checker_source: Option<&'a Path>,
    pub static_flags: &'a [String],
    pub enable_traffic_capture: bool,
    pub enable_shared_container: bool,
    pub network_mode: Option<NetworkMode>,
    pub ad_allow_egress: bool,
    pub ad_allow_self_reset: bool,
    pub ad_ssh_requires_flag: bool,
    pub ad_self_hosted: bool,
}

fn mutable_registry_reference_is_unverifiable(reference: Option<&str>) -> bool {
    reference.is_some_and(|reference| {
        !reference.contains("{{")
            && !crate::services::challenge_images::is_repository_digest(reference)
    })
}

async fn local_build_source_matches(
    st: &SharedState,
    challenge: &game_challenge::Model,
    intent: &LiveRuntimeIntent<'_>,
) -> bool {
    match intent.local_build_context {
        Some(current)
            if challenge.build_context_subdir.as_deref() == intent.build_context_subdir =>
        {
            let Some(hash) = challenge.original_archive_blob_path.as_deref() else {
                return false;
            };
            let archive = match st
                .storage
                .load_bounded(hash, crate::utils::upload::SOURCE_ARCHIVE_BLOB_BYTES)
                .await
            {
                Ok(archive) => archive,
                Err(error) => {
                    tracing::warn!(%error, %hash, "git_sync: live build source could not be fingerprinted");
                    return false;
                }
            };
            let current = context_fingerprint(current).await;
            let retained =
                archived_context_fingerprint(archive, intent.build_context_subdir.unwrap_or("."))
                    .await;
            matches!((current, retained), (Ok(left), Ok(right)) if left == right)
        }
        None => challenge.build_context_subdir.is_none(),
        Some(_) => false,
    }
}

async fn checker_source_matches(challenge: &game_challenge::Model, current: Option<&Path>) -> bool {
    match (current, challenge.ad_checker_image.as_deref()) {
        (Some(current), Some(retained)) => {
            let current = context_fingerprint(current).await;
            let retained = context_fingerprint(&Path::new(retained).join("src")).await;
            matches!((current, retained), (Ok(left), Ok(right)) if left == right)
        }
        (None, None) => true,
        _ => false,
    }
}

async fn static_flag_policy_matches(
    st: &SharedState,
    challenge: &game_challenge::Model,
    requested: &[String],
) -> AppResult<bool> {
    if challenge.challenge_type != ChallengeType::StaticContainer {
        return Ok(true);
    }
    let mut stored = sqlx::query_scalar::<_, String>(
        r#"SELECT flag FROM "FlagContexts" WHERE challenge_id = $1 ORDER BY flag"#,
    )
    .bind(challenge.id)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    stored.sort();
    stored.dedup();
    Ok(stored == requested)
}

/// Return true unless every launch-affecting input can be proven byte-for-byte
/// equivalent to the currently published runtime.
pub(super) async fn live_runtime_update_deferred(
    st: &SharedState,
    challenge: &game_challenge::Model,
    intent: &LiveRuntimeIntent<'_>,
) -> AppResult<bool> {
    let local_source_matches = local_build_source_matches(st, challenge, intent).await;
    let checker_matches = checker_source_matches(challenge, intent.checker_source).await;
    let flag_policy_matches =
        static_flag_policy_matches(st, challenge, intent.static_flags).await?;
    Ok(
        challenge.container_image.as_deref() != intent.container_image
            || challenge.memory_limit != intent.memory_limit
            || challenge.storage_limit != intent.storage_limit
            || challenge.cpu_count != intent.cpu_count
            || challenge.expose_port != intent.expose_port
            || challenge.flag_template.as_deref() != intent.flag_template
            || challenge.build_context_subdir.as_deref() != intent.build_context_subdir
            || challenge.enable_traffic_capture != intent.enable_traffic_capture
            || challenge.enable_shared_container != intent.enable_shared_container
            || challenge.network_mode != intent.network_mode
            || challenge.ad_allow_egress != intent.ad_allow_egress
            || challenge.ad_allow_self_reset != intent.ad_allow_self_reset
            || challenge.ad_ssh_requires_flag != intent.ad_ssh_requires_flag
            || challenge.ad_self_hosted != intent.ad_self_hosted
            || mutable_registry_reference_is_unverifiable(intent.declared_container_image)
            || !local_source_matches
            || !checker_matches
            || !flag_policy_matches,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutable_registry_tags_are_never_assumed_equivalent() {
        assert!(mutable_registry_reference_is_unverifiable(Some(
            "registry.example/team/service:stable"
        )));
        assert!(!mutable_registry_reference_is_unverifiable(Some(&format!(
            "registry.example/team/service@sha256:{}",
            "a".repeat(64)
        ))));
        assert!(!mutable_registry_reference_is_unverifiable(Some(
            "{{.slug}}:latest"
        )));
        assert!(!mutable_registry_reference_is_unverifiable(None));
    }
}
