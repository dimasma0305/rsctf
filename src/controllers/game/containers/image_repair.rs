use super::*;
use crate::utils::enums::ChallengeBuildStatus;

static RUNTIME_IMAGE_REPAIRS: std::sync::LazyLock<crate::utils::single_flight::SingleFlight<bool>> =
    std::sync::LazyLock::new(crate::utils::single_flight::SingleFlight::new);
const RUNTIME_IMAGE_REPAIR_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RuntimeImageRepairPlan {
    BackendManaged,
    Ready,
    RebuildFromArchive,
    Unavailable,
}

fn runtime_image_repair_plan(
    immutable_image: &str,
    image_present: bool,
    archive_available: bool,
) -> RuntimeImageRepairPlan {
    if !crate::services::challenge_images::is_local_image_id(immutable_image) {
        return RuntimeImageRepairPlan::BackendManaged;
    }
    if image_present {
        return RuntimeImageRepairPlan::Ready;
    }
    if archive_available {
        RuntimeImageRepairPlan::RebuildFromArchive
    } else {
        RuntimeImageRepairPlan::Unavailable
    }
}

/// Recover a daemon-local image that disappeared after a successful build.
/// Repository digests remain the backend's responsibility because Docker can
/// pull them without changing identity. A local ID is repaired only from the
/// persisted trusted archive; the mutable configured tag is never a fallback.
pub(super) async fn repair_missing_legacy_image(
    st: &SharedState,
    challenge: &game_challenge::Model,
    immutable_image: &str,
) -> AppResult<bool> {
    if !crate::services::challenge_images::is_local_image_id(immutable_image) {
        return Ok(false);
    }
    let image_present = st.containers.image_exists(immutable_image).await;
    let archive_available = challenge
        .original_archive_blob_path
        .as_deref()
        .is_some_and(|path| !path.trim().is_empty());
    match runtime_image_repair_plan(immutable_image, image_present, archive_available) {
        RuntimeImageRepairPlan::BackendManaged | RuntimeImageRepairPlan::Ready => Ok(false),
        RuntimeImageRepairPlan::Unavailable => {
            tracing::error!(
                game = challenge.game_id,
                challenge = challenge.id,
                image = immutable_image,
                "daemon-local challenge image is missing and has no trusted repair archive"
            );
            Err(AppError::unavailable(
                "The challenge image is unavailable on this container host. Ask an administrator to rebuild the challenge.",
            ))
        }
        RuntimeImageRepairPlan::RebuildFromArchive => {
            // Collapse a same-replica start burst before it reaches the
            // cross-replica build lock. The leader is detached so a browser or
            // reverse-proxy timeout cannot cancel an in-progress image build.
            // The build seam performs the decisive post-lock existence recheck.
            let st = st.clone();
            let challenge = challenge.clone();
            let previous_image = immutable_image.to_string();
            let flight_key = format!("runtime-image-repair:{}", challenge.id);
            let repaired = RUNTIME_IMAGE_REPAIRS
                .run_with_timeout(
                    &flight_key,
                    RUNTIME_IMAGE_REPAIR_TIMEOUT,
                    move || async move {
                        let outcome = crate::controllers::edit::repair_missing_challenge_image(
                            &st, &challenge,
                        )
                        .await;
                        let repaired_image = outcome.image_digest.as_deref().filter(|value| {
                            crate::services::challenge_images::is_local_image_id(value)
                        });
                        let repaired = if outcome.status == ChallengeBuildStatus::Success {
                            match repaired_image {
                                Some(value) => st.containers.image_exists(value).await,
                                None => false,
                            }
                        } else {
                            false
                        };
                        if repaired {
                            tracing::info!(
                                game = challenge.game_id,
                                challenge = challenge.id,
                                previous_image,
                                repaired_image = repaired_image.unwrap_or_default(),
                                "repaired missing daemon-local challenge image"
                            );
                        } else {
                            tracing::error!(
                                game = challenge.game_id,
                                challenge = challenge.id,
                                image = previous_image,
                                build_log = outcome.log.as_deref().unwrap_or("<none>"),
                                "automatic challenge image repair failed"
                            );
                        }
                        repaired
                    },
                )
                .await;
            if repaired {
                return Ok(true);
            }
            Err(AppError::unavailable(
                "The challenge image is temporarily unavailable and automatic repair failed. An administrator must rebuild it.",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCAL: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const PORTABLE: &str =
        "registry.example/ctf/app@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn present_local_image_needs_no_repair() {
        assert_eq!(
            runtime_image_repair_plan(LOCAL, true, true),
            RuntimeImageRepairPlan::Ready
        );
    }

    #[test]
    fn missing_local_image_repairs_only_from_trusted_archive() {
        assert_eq!(
            runtime_image_repair_plan(LOCAL, false, true),
            RuntimeImageRepairPlan::RebuildFromArchive
        );
        assert_eq!(
            runtime_image_repair_plan(LOCAL, false, false),
            RuntimeImageRepairPlan::Unavailable
        );
    }

    #[test]
    fn repository_digest_remains_backend_pull_owned() {
        assert_eq!(
            runtime_image_repair_plan(PORTABLE, false, false),
            RuntimeImageRepairPlan::BackendManaged
        );
    }
}
