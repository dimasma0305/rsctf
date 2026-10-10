use super::eligibility::{
    authorize_on_demand_build, ineligible_container_start_error, player_request_is_eligible_now,
    ContainerRequestMode,
};
use super::*;
use crate::services::live_roster::LiveParticipationIdentity;
use crate::utils::enums::ChallengeBuildStatus;

static RUNTIME_IMAGE_BUILDS: std::sync::LazyLock<crate::utils::single_flight::SingleFlight<bool>> =
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

/// Build an eligible queued image on the first runtime demand. The detached
/// single-flight leader keeps building if the initiating HTTP request times
/// out, while the PostgreSQL image lock collapses leaders across replicas.
pub(crate) async fn prepare_queued_image(
    st: &SharedState,
    challenge: &game_challenge::Model,
) -> AppResult<bool> {
    if challenge.build_status != ChallengeBuildStatus::Queued {
        return Ok(false);
    }
    let policy = crate::services::container_policy::ContainerPolicy::load(st.pg()).await?;
    if !crate::services::image_storage::lazy_build_eligible(&policy, challenge) {
        return Ok(false);
    }
    let st = st.clone();
    let challenge = challenge.clone();
    let flight_key = format!("runtime-image-build:{}", challenge.id);
    let built = RUNTIME_IMAGE_BUILDS
        .run_with_timeout(
            &flight_key,
            RUNTIME_IMAGE_REPAIR_TIMEOUT,
            move || async move {
                let outcome =
                    crate::controllers::edit::ensure_challenge_image(&st, &challenge).await;
                let image = outcome.image_digest.as_deref();
                let ready = outcome.status == ChallengeBuildStatus::Success
                    && match image {
                        Some(image) => st.containers.image_exists(image).await,
                        None => false,
                    };
                if ready {
                    tracing::info!(
                        game = challenge.game_id,
                        challenge = challenge.id,
                        image = image.unwrap_or_default(),
                        "built challenge image on first runtime demand"
                    );
                } else {
                    tracing::error!(
                        game = challenge.game_id,
                        challenge = challenge.id,
                        build_log = outcome.log.as_deref().unwrap_or("<none>"),
                        "on-demand challenge image build failed"
                    );
                }
                ready
            },
        )
        .await;
    if built {
        Ok(true)
    } else {
        Err(AppError::unavailable(
            "The challenge image could not be built on demand. An administrator must inspect its build log.",
        ))
    }
}

/// Recover a daemon-local image that disappeared after a successful build.
/// Repository digests remain the backend's responsibility because Docker can
/// pull them without changing identity. A local ID is repaired only from the
/// persisted trusted archive; the mutable configured tag is never a fallback.
pub(crate) async fn repair_missing_legacy_image(
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
            let repaired = RUNTIME_IMAGE_BUILDS
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AdmissionImagePlan<'a> {
    Ready,
    BuildQueued,
    RepairMissing(&'a str),
}

fn admission_image_plan(
    build_status: ChallengeBuildStatus,
    missing_local_image: Option<&str>,
) -> AdmissionImagePlan<'_> {
    if build_status == ChallengeBuildStatus::Queued {
        return AdmissionImagePlan::BuildQueued;
    }
    match missing_local_image {
        Some(image) => AdmissionImagePlan::RepairMissing(image),
        None => AdmissionImagePlan::Ready,
    }
}

/// Materialize a queued or pruned image before a player start is admitted.
/// `operations::spawn_owner` bounds an admitted launch to its deadline. A
/// multi-minute build inside that window timed out the owner and failed its
/// followers although the detached build kept running, so the player saw an
/// error and then an instant success on the next click. Both builders are
/// detached single flights: a cancelled waiter does not cancel them and a
/// retried request rejoins. The admitted launch still takes its own locked
/// definition snapshot, which remains the decisive recheck.
pub(super) async fn prepare_image_before_admission(
    st: &SharedState,
    caller: LiveParticipationIdentity<'_>,
    challenge: &game_challenge::Model,
    shared: bool,
) -> AppResult<()> {
    let legacy_image = crate::services::challenge_workloads::resolve_runtime(st, challenge)
        .ok()
        .and_then(|runtime| runtime.legacy_image);
    let mut missing_local_image = None;
    if let Some(image) = legacy_image.as_deref() {
        if crate::services::challenge_images::is_local_image_id(image)
            && !st.containers.image_exists(image).await
        {
            missing_local_image = Some(image);
        }
    }
    match admission_image_plan(challenge.build_status, missing_local_image) {
        AdmissionImagePlan::Ready => Ok(()),
        AdmissionImagePlan::BuildQueued => {
            // The first-build transition keeps its exact authorization gate. A
            // queued challenge that is not lazily buildable falls through to
            // the admitted launch's own error.
            if authorize_on_demand_build(st, caller, challenge).await? {
                prepare_queued_image(st, challenge).await?;
            }
            Ok(())
        }
        AdmissionImagePlan::RepairMissing(image) => {
            let mode = if shared {
                ContainerRequestMode::Shared
            } else {
                ContainerRequestMode::PerTeam
            };
            if !player_request_is_eligible_now(st, caller, challenge.id, mode).await? {
                return Err(ineligible_container_start_error(st, challenge));
            }
            repair_missing_legacy_image(st, challenge, image).await?;
            Ok(())
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
    fn admission_plan_builds_queued_images_and_repairs_only_missing_local_ids() {
        assert_eq!(
            admission_image_plan(ChallengeBuildStatus::Queued, None),
            AdmissionImagePlan::BuildQueued
        );
        assert_eq!(
            admission_image_plan(ChallengeBuildStatus::Success, Some(LOCAL)),
            AdmissionImagePlan::RepairMissing(LOCAL)
        );
        assert_eq!(
            admission_image_plan(ChallengeBuildStatus::Success, None),
            AdmissionImagePlan::Ready
        );
    }

    #[test]
    fn player_create_prepares_its_image_before_the_bounded_operation_claim() {
        let source = include_str!("../containers.rs");
        let start = source.find("pub async fn create_container").unwrap();
        let end = source.find("async fn perform_create_container").unwrap();
        let create = &source[start..end];
        let prepare = create
            .find("prepare_image_before_admission")
            .expect("player create must materialize or repair its image before admission");
        let claim = create
            .find("operations::claim_create")
            .expect("player create must claim its durable operation");
        assert!(prepare < claim);
    }

    #[test]
    fn repository_digest_remains_backend_pull_owned() {
        assert_eq!(
            runtime_image_repair_plan(PORTABLE, false, false),
            RuntimeImageRepairPlan::BackendManaged
        );
    }
}
