//! edit: image build seam (see edit/mod.rs for the router + shared DTOs/helpers).
use super::*;

mod archive;
use archive::zip_bytes_to_tar;
mod identity;
#[cfg(test)]
use identity::immutable_image_reference;
use identity::{build_lock_key, inspect_immutable_image, ImageOperation};
pub(crate) use identity::{
    canonical_image_reference, canonical_managed_image_tag, image_build_lock_key,
};
mod publication;
use publication::*;
#[cfg(test)]
mod archive_tests;
#[cfg(test)]
mod backfill_tests;
#[cfg(test)]
mod local_image_tests;

const MAX_BUILD_ARCHIVE_BLOB_BYTES: usize = crate::utils::upload::SOURCE_ARCHIVE_BLOB_BYTES;
const RUNTIME_REPAIR_STATE_SQL: &str = r#"SELECT build_status,
              build_image_digest,
              last_build_log
         FROM "GameChallenges"
        WHERE id = $1
          AND deletion_pending = FALSE"#;
const BACKFILL_TERMINAL_BUILD_RECORDS_SQL: &str = r#"
WITH inserted AS (
    INSERT INTO "BuildRecords" (
        challenge_id,
        game_id,
        challenge_title,
        enqueued_at_utc,
        started_at_utc,
        finished_at_utc,
        trigger,
        kind,
        attempt,
        status,
        digest,
        image_ref,
        log_tail
    )
    SELECT challenge.id,
           challenge.game_id,
           challenge.title,
           clock_timestamp(),
           clock_timestamp(),
           clock_timestamp(),
           'Backfill',
           'Challenge',
           1,
           challenge.build_status,
           challenge.build_image_digest,
           CASE WHEN challenge.build_status = 1 THEN challenge.container_image END,
           right(challenge.last_build_log, 4096)
      FROM "GameChallenges" challenge
     WHERE challenge.build_status IN (1, 2, 6)
       AND NOT EXISTS (
            SELECT 1
              FROM "BuildRecords" record
             WHERE record.challenge_id = challenge.id
       )
    RETURNING 1
)
SELECT COUNT(*)::BIGINT FROM inserted
"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BuildIntent {
    Requested,
    RepairMissingRuntime,
    EnsureRuntime,
}

pub(super) fn invalidated_build_status(
    container_image: Option<&str>,
    _original_archive_blob_path: Option<&str>,
    _build_context_subdir: Option<&str>,
) -> ChallengeBuildStatus {
    if container_image.is_none_or(|image| image.trim().is_empty()) {
        ChallengeBuildStatus::NotApplicable
    } else {
        ChallengeBuildStatus::Queued
    }
}

fn validate_local_image_adoption(
    image: &str,
    role: crate::models::internal::configs::RuntimeRole,
    backend: crate::services::container::ContainerBackendKind,
    shared_docker_daemon: bool,
) -> AppResult<String> {
    crate::services::challenge_images::validate_runtime_reference(
        image,
        backend,
        role,
        shared_docker_daemon,
    )
}

/// Public build entry point used by the interactive rebuild route and the admin
/// re-enqueue action. Runs the image build/pull seam (`build_challenge_image`)
/// and then records the attempt as a `BuildRecords` audit row so the admin
/// Builds history + in-progress views have real data. The audit write is
/// best-effort: a DB failure yields `None` and never fails the build itself.
pub(crate) async fn run_challenge_build(
    st: &SharedState,
    challenge: &game_challenge::Model,
    trigger: &str,
    attempt: i32,
) -> (BuildOutcome, Option<build_record::Model>) {
    run_challenge_build_with_intent(st, challenge, trigger, attempt, BuildIntent::Requested).await
}

/// Rebuild a vanished daemon-local runtime image from its authoritative source
/// archive. The build lock rechecks the current published digest first, so a
/// burst of player starts across replicas performs at most one repair.
pub(crate) async fn repair_missing_challenge_image(
    st: &SharedState,
    challenge: &game_challenge::Model,
) -> BuildOutcome {
    run_challenge_build_with_intent(
        st,
        challenge,
        "RuntimeRepair",
        1,
        BuildIntent::RepairMissingRuntime,
    )
    .await
    .0
}

/// Materialize a recoverable queued image for a player's first start. The
/// build lock makes this an idempotent cross-replica ensure operation rather
/// than one build per waiting request.
pub(crate) async fn ensure_challenge_image(
    st: &SharedState,
    challenge: &game_challenge::Model,
) -> BuildOutcome {
    run_challenge_build_with_intent(st, challenge, "RuntimeStart", 1, BuildIntent::EnsureRuntime)
        .await
        .0
}

async fn run_challenge_build_with_intent(
    st: &SharedState,
    challenge: &game_challenge::Model,
    trigger: &str,
    attempt: i32,
    intent: BuildIntent,
) -> (BuildOutcome, Option<build_record::Model>) {
    // `started` doubles as the enqueue instant — this port runs the build inline,
    // so it is enqueued and started in the same breath.
    let started = Utc::now();
    if super::challenges::reject_pending_mutation(st.pg(), challenge.game_id, challenge.id)
        .await
        .is_err()
    {
        return (
            superseded_build_outcome(
                "Build cancelled because the challenge or its game is being deleted.",
            ),
            None,
        );
    }
    // Image tags are mutable daemon state. Serialize every writer of the same
    // canonical tag, including different challenges and different replicas.
    // The gate is distinct from provisioning because self-heal can be reached
    // from inside an already-held provisioning critical section.
    let lock_key = build_lock_key(challenge);
    let mut build_lock = match crate::utils::single_flight::PgAdvisoryLock::acquire_build(
        st.pg(),
        &lock_key,
    )
    .await
    {
        Ok(lock) => lock,
        Err(error) => {
            let outcome = BuildOutcome {
                status: ChallengeBuildStatus::Failed,
                log: Some(format!(
                    "Build coordination unavailable; no background work was queued. Retry manually: {error}"
                )),
                image_digest: None,
            };
            let record = record_build(st, challenge, trigger, attempt, started, &outcome).await;
            return (outcome, record);
        }
    };

    let requested_fingerprint = BuildFingerprint::from_challenge(challenge);
    let current_fingerprint =
        sqlx::query_as::<_, (Option<String>, Option<String>, Option<String>)>(
            BUILD_FINGERPRINT_SQL,
        )
        .bind(challenge.id)
        .fetch_optional(build_lock.connection_mut())
        .await;
    let current_fingerprint = match current_fingerprint {
        Ok(Some((container_image, original_archive_blob_path, build_context_subdir))) => {
            BuildFingerprint {
                container_image,
                original_archive_blob_path,
                build_context_subdir,
            }
        }
        Ok(None) => {
            let outcome = superseded_build_outcome(
                "Build cancelled because the challenge or its game was deleted or fenced before it acquired the image lock.",
            );
            let _ = build_lock.release().await;
            return (outcome, None);
        }
        Err(error) => {
            tracing::warn!(
                challenge = challenge.id,
                %error,
                "challenge build fingerprint read failed"
            );
            // The connection is close-on-drop, so a broken fingerprint read
            // cannot return session-level advisory state to the pool.
            drop(build_lock);
            let outcome = BuildOutcome {
                status: ChallengeBuildStatus::Failed,
                log: Some(
                    "Build coordination unavailable; no background work was queued. Retry the current definition manually."
                        .to_string(),
                ),
                image_digest: None,
            };
            let record = record_build(st, challenge, trigger, attempt, started, &outcome).await;
            return (outcome, record);
        }
    };
    if current_fingerprint != requested_fingerprint {
        let outcome = superseded_build_outcome(
            "Build cancelled because the image, source archive, or context selector changed before it acquired the image lock. Rebuild the current definition.",
        );
        let _ = build_lock.release().await;
        let record = record_build(st, challenge, trigger, attempt, started, &outcome).await;
        return (outcome, record);
    }

    if intent != BuildIntent::Requested {
        let repair_state =
            sqlx::query_as::<_, (i16, Option<String>, Option<String>)>(RUNTIME_REPAIR_STATE_SQL)
                .bind(challenge.id)
                .fetch_optional(build_lock.connection_mut())
                .await;
        match repair_state {
            Ok(Some((status, Some(current_image), _)))
                if status == ChallengeBuildStatus::Success as i16
                    && st.containers.image_exists(current_image.trim()).await =>
            {
                let outcome = BuildOutcome {
                    status: ChallengeBuildStatus::Success,
                    log: Some("The immutable runtime image is already available.".to_string()),
                    image_digest: Some(current_image),
                };
                if let Err(error) = build_lock.release().await {
                    tracing::warn!(
                        challenge = challenge.id,
                        %error,
                        "runtime image repair lock release failed after a completed peer repair"
                    );
                }
                return (outcome, None);
            }
            Ok(Some((status, _, _))) if status == ChallengeBuildStatus::Success as i16 => {}
            Ok(Some((status, _, _)))
                if intent == BuildIntent::EnsureRuntime
                    && status == ChallengeBuildStatus::Queued as i16 => {}
            Ok(Some((_, _, current_log))) => {
                let outcome = BuildOutcome {
                    status: ChallengeBuildStatus::Failed,
                    log: current_log.or_else(|| {
                        Some(
                            "Automatic runtime preparation stopped because the current build is not queued or successful."
                                .to_string(),
                        )
                    }),
                    image_digest: None,
                };
                if let Err(error) = build_lock.release().await {
                    tracing::warn!(
                        challenge = challenge.id,
                        %error,
                        "runtime image repair lock release failed after a terminal peer repair"
                    );
                }
                return (outcome, None);
            }
            Ok(None) => {
                let outcome = superseded_build_outcome(
                    "Automatic runtime preparation stopped because the challenge was deleted.",
                );
                let _ = build_lock.release().await;
                return (outcome, None);
            }
            Err(error) => {
                tracing::warn!(
                    challenge = challenge.id,
                    %error,
                    "runtime image preparation state read failed"
                );
                let outcome = BuildOutcome {
                    status: ChallengeBuildStatus::Failed,
                    log: Some(
                        "Automatic runtime preparation could not verify the current build state."
                            .to_string(),
                    ),
                    image_digest: None,
                };
                let _ = build_lock.release().await;
                return (outcome, None);
            }
        }
    }

    let mut outcome = build_challenge_image(st, challenge).await;
    let ownership = if outcome.status == ChallengeBuildStatus::Success {
        match resolve_build_image_ownership(challenge, &outcome).await {
            Ok(ownership) => ownership,
            Err(error) => {
                outcome = BuildOutcome {
                    status: ChallengeBuildStatus::Failed,
                    log: Some(format!(
                        "The image operation completed, but rsctf could not prove durable ownership of its mutable tag: {error}"
                    )),
                    image_digest: None,
                };
                None
            }
        }
    } else {
        None
    };
    let persisted = publish_build_outcome(
        st,
        challenge,
        &requested_fingerprint,
        &outcome,
        ownership.as_ref(),
    )
    .await;
    let unlocked = build_lock.release().await;
    match (persisted, unlocked) {
        (Ok(1), Ok(())) => {}
        (Ok(1), Err(error)) => {
            // Publication committed under the definition fence. The connection
            // is close-on-drop, so PostgreSQL will still release the session
            // lock even when the explicit unlock round trip fails.
            tracing::warn!(
                challenge = challenge.id,
                %error,
                "challenge build image lock release failed after status publication"
            );
        }
        (Ok(_), unlock_result) => {
            if let Err(error) = unlock_result {
                tracing::warn!(challenge = challenge.id, %error, "superseded build unlock failed");
            }
            outcome = superseded_build_outcome(
                "Build result discarded because the image, source archive, or context selector changed while it was running. Rebuild the current definition.",
            );
        }
        (Err(error), unlock_result) => {
            tracing::warn!(
                challenge = challenge.id,
                %error,
                "challenge build completed but its coordinated status update failed"
            );
            if let Err(unlock_error) = unlock_result {
                tracing::warn!(challenge = challenge.id, %unlock_error, "failed build publication unlock failed");
            }
            outcome = superseded_build_outcome(
                "The image operation completed, but its status could not be recorded. Rebuild before provisioning this definition.",
            );
        }
    }
    let record = record_build(st, challenge, trigger, attempt, started, &outcome).await;
    (outcome, record)
}

async fn resolve_build_image_ownership(
    challenge: &game_challenge::Model,
    outcome: &BuildOutcome,
) -> Result<Option<BuildImageOwnership>, String> {
    let Some(configured) = challenge.container_image.as_deref() else {
        return Ok(None);
    };
    let Some(canonical_ref) = canonical_managed_image_tag(configured) else {
        return Ok(None);
    };
    let immutable_reference = outcome
        .image_digest
        .as_deref()
        .ok_or_else(|| "successful build did not publish an immutable reference".to_string())?;
    let docker = Docker::connect_with_local_defaults()
        .map_err(|error| format!("Docker connection failed after the build: {error}"))?;
    let inspected = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        docker.inspect_image(configured),
    )
    .await
    .map_err(|_| "Docker image identity inspection timed out".to_string())?
    .map_err(|error| format!("Docker image identity inspection failed: {error}"))?;
    if !crate::services::challenge_images::inspect_matches_immutable_reference(
        &inspected,
        immutable_reference,
    ) {
        return Err(
            "Docker's current tag identity does not match the immutable build result".to_string(),
        );
    }
    let installation_scope = crate::services::container::docker_installation_scope();
    crate::services::challenge_images::validate_image_ownership_labels(
        &inspected,
        &installation_scope,
        &canonical_ref,
        challenge.build_context_subdir.is_some(),
    )?;
    let image_id = crate::services::challenge_images::inspected_local_image_id(&inspected)
        .ok_or_else(|| "Docker did not report a valid content-addressable image id".to_string())?
        .to_string();
    Ok(Some(BuildImageOwnership {
        installation_scope,
        canonical_ref,
        image_id,
    }))
}

fn terminal_audit_status(status: ChallengeBuildStatus) -> ChallengeBuildStatus {
    match status {
        ChallengeBuildStatus::Queued | ChallengeBuildStatus::Building => {
            ChallengeBuildStatus::Failed
        }
        status => status,
    }
}

/// Persist one completed synchronous build attempt as a `BuildRecords` audit
/// row. This binary has no background build consumer, so a retryable internal
/// outcome is defensively recorded as `Failed`, never as live queued work.
/// Only a `Success` carries an image reference. The captured log is trimmed to
/// a compact ~4 KiB tail for the row.
async fn record_build(
    st: &SharedState,
    challenge: &game_challenge::Model,
    trigger: &str,
    attempt: i32,
    started: DateTime<Utc>,
    outcome: &BuildOutcome,
) -> Option<build_record::Model> {
    let status = terminal_audit_status(outcome.status);
    let image_ref = (status == ChallengeBuildStatus::Success)
        .then(|| challenge.container_image.clone())
        .flatten();

    let record = build_record::ActiveModel {
        challenge_id: Set(challenge.id),
        game_id: Set(challenge.game_id),
        challenge_title: Set(challenge.title.clone()),
        enqueued_at_utc: Set(started),
        started_at_utc: Set(Some(started)),
        finished_at_utc: Set(Some(Utc::now())),
        trigger: Set(trigger.to_string()),
        kind: Set("Challenge".to_string()),
        attempt: Set(attempt.max(1)),
        status: Set(status),
        digest: Set(outcome.image_digest.clone()),
        image_ref: Set(image_ref),
        log_tail: Set(outcome.log.as_deref().map(build_log_tail)),
        ..Default::default()
    };

    // Best-effort: a failed audit write must not sink the build.
    record.insert(&st.db).await.ok()
}

/// Re-enqueue a build for `challenge` from the admin Builds page: runs the same
/// seam as an interactive rebuild but tags the attempt `AutoRetry`, and returns
/// the freshly-recorded audit row so the caller can echo it back.
pub(crate) async fn admin_reenqueue_build(
    st: &SharedState,
    challenge: &game_challenge::Model,
    attempt: i32,
) -> AppResult<Option<build_record::Model>> {
    super::challenges::reject_pending_mutation(st.pg(), challenge.game_id, challenge.id).await?;
    let (_outcome, record) = run_challenge_build(st, challenge, "AutoRetry", attempt).await;
    // A deletion fence may have committed while slow image work ran. Do not let
    // the admin endpoint turn the resulting absent audit row into a fresh queued
    // record on a challenge that is already draining.
    super::challenges::reject_pending_mutation(st.pg(), challenge.game_id, challenge.id).await?;
    Ok(record)
}

/// Compact the (already 16 KiB-capped) build log to a ~4 KiB tail for the audit
/// row — the failing step and final error live at the end.
fn build_log_tail(log: &str) -> String {
    const MAX: usize = 4 * 1024;
    if log.len() <= MAX {
        return log.to_string();
    }
    // Advance to a char boundary so multi-byte UTF-8 isn't split mid-codepoint.
    let mut start = log.len() - MAX;
    while start < log.len() && !log.is_char_boundary(start) {
        start += 1;
    }
    format!("…(truncated)…\n{}", &log[start..])
}

/// Docker daemon liveness probe — a short-timeout `ping` (mirrors the
/// reachability gate in `services::ad_engine`). `false` means "treat Docker as
/// absent" so the build seam degrades gracefully instead of erroring.
async fn docker_reachable(docker: &Docker) -> bool {
    matches!(
        tokio::time::timeout(std::time::Duration::from_secs(2), docker.ping()).await,
        Ok(Ok(_))
    )
}

fn archive_build_rejection(
    role: crate::models::internal::configs::RuntimeRole,
    backend: crate::services::container::ContainerBackendKind,
    shared_daemon_acknowledged: bool,
    worker_runtime: bool,
) -> Option<&'static str> {
    use crate::models::internal::configs::RuntimeRole;
    use crate::services::container::ContainerBackendKind;

    if worker_runtime {
        return Some(
            "Archive builds cannot publish a portable image to trusted workers. Build and push the image to a registry, then configure its repository digest.",
        );
    }
    if backend != ContainerBackendKind::Docker {
        return Some(
            "Persisted build archives require the Docker backend. Kubernetes and daemon-independent replicas must use a prebuilt registry image.",
        );
    }
    if role != RuntimeRole::All && !shared_daemon_acknowledged {
        return Some(
            "Archive builds are disabled for split roles unless every API replica and container owner uses one shared Docker daemon. Use a prebuilt registry image, or set RSCTF_SHARED_DOCKER_DAEMON=true only after verifying that invariant.",
        );
    }
    None
}

/// Cap a captured build/pull log so a chatty build doesn't bloat the row.
fn cap_build_log(mut log: String) -> Option<String> {
    const MAX: usize = 16 * 1024;
    if log.len() > MAX {
        // Keep the tail — the failing step and final error live at the end.
        let start = log.len() - MAX;
        log = format!("…(truncated)…\n{}", &log[start..]);
    }
    if log.is_empty() {
        None
    } else {
        Some(log)
    }
}

/// The image-build seam invoked by the rebuild route (RSCTF ships this as an
/// async `IChallengeBuildQueue`; this port runs it inline). When a persisted
/// build-context selector is present it builds the selected subtree of the
/// immutable source archive; otherwise it pulls the referenced
/// `container_image` with `Docker::create_image`. Never panics and never
/// surfaces a 5xx — a missing/unreachable daemon degrades to `Queued`.
pub(crate) async fn build_challenge_image(
    st: &SharedState,
    challenge: &game_challenge::Model,
) -> BuildOutcome {
    // Only container-backed challenges have an image to build/pull.
    let image = match challenge
        .container_image
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        Some(img) => img.trim().to_string(),
        None => {
            return BuildOutcome {
                status: ChallengeBuildStatus::NotApplicable,
                log: Some("Challenge has no container image; nothing to build.".to_string()),
                image_digest: None,
            };
        }
    };

    let context_selector = challenge.build_context_subdir.as_deref();
    let archive_path = context_selector.and_then(|_| {
        challenge
            .original_archive_blob_path
            .as_deref()
            .filter(|path| !path.trim().is_empty())
    });
    if context_selector.is_some() && archive_path.is_none() {
        return BuildOutcome {
            status: ChallengeBuildStatus::Failed,
            log: Some(
                "The challenge declares a build context but its immutable source archive is unavailable."
                    .to_string(),
            ),
            image_digest: None,
        };
    }
    let worker_runtime = crate::services::challenge_workloads::uses_worker_runtime(st, challenge);
    if context_selector.is_some() {
        if let Some(reason) = archive_build_rejection(
            st.config.runtime_role,
            st.containers.backend_kind(),
            crate::services::challenge_images::shared_docker_daemon_acknowledged(),
            worker_runtime,
        ) {
            return BuildOutcome {
                status: ChallengeBuildStatus::Failed,
                log: Some(reason.to_string()),
                image_digest: None,
            };
        }
    }

    // A pre-pinned registry reference needs no daemon-side resolution. This is
    // the Kubernetes/independent-node path: the runtime later pulls the exact
    // manifest, while the coordinated publish below records the same immutable
    // definition atomically.
    if context_selector.is_none() && crate::services::challenge_images::is_repository_digest(&image)
    {
        return BuildOutcome {
            status: ChallengeBuildStatus::Success,
            log: Some("Configured image is already pinned to a repository digest.".to_string()),
            image_digest: Some(image),
        };
    }

    // A worker-scoped daemon image is already an exact immutable reference.
    // The control plane cannot inspect that remote daemon; validate the worker
    // scope now and let placement enforce the embedded worker identity.
    if context_selector.is_none()
        && crate::services::challenge_images::worker_local_image(&image).is_some()
    {
        let backend = if worker_runtime {
            crate::services::container::ContainerBackendKind::Worker
        } else {
            st.containers.backend_kind()
        };
        return match crate::services::challenge_images::validate_runtime_reference(
            &image,
            backend,
            st.config.runtime_role,
            false,
        ) {
            Ok(reference) => BuildOutcome {
                status: ChallengeBuildStatus::Success,
                log: Some("Configured image is pinned to one enrolled worker.".to_string()),
                image_digest: Some(reference),
            },
            Err(error) => BuildOutcome {
                status: ChallengeBuildStatus::Failed,
                log: Some(error.to_string()),
                image_digest: None,
            },
        };
    }

    let local_image_id = if context_selector.is_none() {
        crate::services::challenge_images::is_local_image_id(&image)
    } else {
        false
    };
    if local_image_id {
        let backend = if worker_runtime {
            crate::services::container::ContainerBackendKind::Worker
        } else {
            st.containers.backend_kind()
        };
        if let Err(error) = validate_local_image_adoption(
            &image,
            st.config.runtime_role,
            backend,
            !worker_runtime
                && crate::services::challenge_images::shared_docker_daemon_acknowledged(),
        ) {
            return BuildOutcome {
                status: ChallengeBuildStatus::Failed,
                log: Some(error.to_string()),
                image_digest: None,
            };
        }
    }

    // This port executes builds synchronously and has no background build
    // consumer. A daemon failure is therefore terminal for this attempt; the
    // operator can retry explicitly after restoring the daemon.
    let docker = match Docker::connect_with_local_defaults() {
        Ok(d) => d,
        Err(_) => {
            return BuildOutcome {
                status: ChallengeBuildStatus::Failed,
                log: Some(
                    "Docker daemon unreachable; no background work was queued. Retry manually."
                        .to_string(),
                ),
                image_digest: None,
            };
        }
    };
    if !docker_reachable(&docker).await {
        return BuildOutcome {
            status: ChallengeBuildStatus::Failed,
            log: Some(
                "Docker daemon unreachable; no background work was queued. Retry manually."
                    .to_string(),
            ),
            image_digest: None,
        };
    }

    // An explicitly pinned daemon-local image is already immutable. Never send
    // it through Docker's registry-pull endpoint: inspect the exact ID on the
    // acknowledged shared daemon and persist only the canonical ID Docker
    // returns. Unsupported backends/topologies were rejected above.
    if local_image_id {
        return match inspect_immutable_image(&docker, &image, ImageOperation::RegistryPull, false)
            .await
        {
            Ok(reference) if reference.eq_ignore_ascii_case(&image) => BuildOutcome {
                status: ChallengeBuildStatus::Success,
                log: Some(
                    "Adopted the existing immutable image ID from the shared Docker daemon."
                        .to_string(),
                ),
                image_digest: Some(reference),
            },
            Ok(_) => BuildOutcome {
                status: ChallengeBuildStatus::Failed,
                log: Some("Docker inspect returned a different image identity.".to_string()),
                image_digest: None,
            },
            Err(error) => BuildOutcome {
                status: ChallengeBuildStatus::Failed,
                log: Some(format!("immutable local image inspect failed: {error}")),
                image_digest: None,
            },
        };
    }

    // A declared archive is authoritative. Never silently fall back to pulling
    // the tag when its object is absent: doing so can mark an unrelated mutable
    // registry tag as the successful result of this source definition.
    let context: Option<Vec<u8>> = match archive_path {
        Some(path) => match st
            .storage
            .load_bounded(path, MAX_BUILD_ARCHIVE_BLOB_BYTES)
            .await
        {
            Ok(bytes) => Some(bytes),
            Err(error) => {
                tracing::warn!(
                    challenge = challenge.id,
                    archive = path,
                    %error,
                    "challenge build archive load failed"
                );
                return BuildOutcome {
                    status: ChallengeBuildStatus::Failed,
                    log: Some(
                        "The persisted build archive is unavailable; refusing to pull the mutable image tag as a fallback."
                            .to_string(),
                    ),
                    image_digest: None,
                };
            }
        },
        None => None,
    };

    match context {
        Some(bytes) if bytes.len() > MAX_BUILD_ARCHIVE_BLOB_BYTES => BuildOutcome {
            status: ChallengeBuildStatus::Failed,
            log: Some("Rejected unsafe build archive: compressed archive is too large".to_string()),
            image_digest: None,
        },
        Some(bytes) => build_from_context(&docker, &image, bytes, context_selector).await,
        None => {
            let portable_required = worker_runtime
                || st.containers.backend_kind()
                    != crate::services::container::ContainerBackendKind::Docker
                || (st.config.runtime_role != crate::models::internal::configs::RuntimeRole::All
                    && !crate::services::challenge_images::shared_docker_daemon_acknowledged());
            pull_image(&docker, &image, portable_required).await
        }
    }
}

/// Build the challenge image from a persisted archive. Docker's build endpoint
/// consumes a *tar* context; this port's packages are zip archives (see
/// import/export), so a validated ZIP is repacked into a tar. Opaque legacy
/// blobs are rejected instead of being passed to a privileged Docker daemon.
async fn build_from_context(
    docker: &Docker,
    image: &str,
    archive: Vec<u8>,
    context_subdir: Option<&str>,
) -> BuildOutcome {
    let tar = match zip_bytes_to_tar(&archive, context_subdir) {
        Ok(tar) => tar,
        Err(error) => {
            return BuildOutcome {
                status: ChallengeBuildStatus::Failed,
                log: Some(format!("Rejected unsafe build archive: {error}")),
                image_digest: None,
            };
        }
    };

    let labels = canonical_managed_image_tag(image)
        .map(|reference| {
            std::collections::HashMap::from([
                (
                    crate::services::container::IMAGE_SCOPE_LABEL.to_string(),
                    crate::services::container::docker_installation_scope(),
                ),
                (
                    crate::services::container::IMAGE_REFERENCE_LABEL.to_string(),
                    reference,
                ),
            ])
        })
        .unwrap_or_default();
    let options = BuildImageOptions::<String> {
        t: image.to_string(),
        dockerfile: "Dockerfile".to_string(),
        rm: true,
        labels,
        ..Default::default()
    };

    // `Some(_)` pins the tar param to bollard's concrete `bytes::Bytes`, so
    // `.into()` resolves `From<Vec<u8>>` without naming the transitive crate.
    let mut stream = docker.build_image(options, None, Some(tar.into()));

    let mut log = String::new();
    let mut failed: Option<String> = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(info) => {
                if let Some(s) = info.stream {
                    log.push_str(&s);
                }
                if let Some(err) = info.error {
                    failed = Some(err);
                }
            }
            Err(e) => {
                failed = Some(format!("build transport error: {e}"));
                break;
            }
        }
    }

    match failed {
        Some(err) => {
            log.push_str(&err);
            BuildOutcome {
                status: ChallengeBuildStatus::Failed,
                log: cap_build_log(log),
                image_digest: None,
            }
        }
        None => match inspect_immutable_image(docker, image, ImageOperation::ArchiveBuild, false)
            .await
        {
            Ok(reference) => BuildOutcome {
                status: ChallengeBuildStatus::Success,
                log: cap_build_log(log),
                image_digest: Some(reference),
            },
            Err(error) => {
                log.push_str(&format!("\nimmutable image resolution failed: {error}"));
                BuildOutcome {
                    status: ChallengeBuildStatus::Failed,
                    log: cap_build_log(log),
                    image_digest: None,
                }
            }
        },
    }
}

/// Pull the referenced image when there is no local build context, mirroring the
/// best-effort `create_image` pull in `DockerContainerManager::create`. A
/// successful pull is a successful "build" of a registry-shipped challenge.
async fn pull_image(docker: &Docker, image: &str, portable_required: bool) -> BuildOutcome {
    let options = CreateImageOptions {
        from_image: image.to_string(),
        ..Default::default()
    };
    let mut stream = docker.create_image(Some(options), None, None);

    let mut log = String::new();
    let mut failed: Option<String> = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(info) => {
                if let Some(s) = info.status {
                    log.push_str(&s);
                    log.push('\n');
                }
                if let Some(err) = info.error {
                    failed = Some(err);
                }
            }
            Err(e) => {
                failed = Some(format!("pull transport error: {e}"));
                break;
            }
        }
    }

    match failed {
        Some(err) => {
            log.push_str(&err);
            BuildOutcome {
                status: ChallengeBuildStatus::Failed,
                log: cap_build_log(log),
                image_digest: None,
            }
        }
        None => match inspect_immutable_image(
            docker,
            image,
            ImageOperation::RegistryPull,
            portable_required,
        )
        .await
        {
            Ok(reference) => BuildOutcome {
                status: ChallengeBuildStatus::Success,
                log: cap_build_log(log),
                image_digest: Some(reference),
            },
            Err(error) => {
                log.push_str(&format!("\nimmutable image resolution failed: {error}"));
                BuildOutcome {
                    status: ChallengeBuildStatus::Failed,
                    log: cap_build_log(log),
                    image_digest: None,
                }
            }
        },
    }
}

/// One-time-per-boot, set-based backfill of `BuildRecords` for terminal builds
/// that have no audit history. `Queued` and `Building` are deliberately excluded:
/// copying those states would invent live work that no background worker owns.
/// Idempotent: a challenge that already has any record is skipped.
pub async fn backfill_build_records(db: &sea_orm::DatabaseConnection) -> u64 {
    backfill_terminal_build_records(db.get_postgres_connection_pool()).await
}

async fn backfill_terminal_build_records(pool: &sqlx::PgPool) -> u64 {
    let mut transaction = match pool.begin().await {
        Ok(transaction) => transaction,
        Err(_) => return 0,
    };
    if sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('rsctf:build-record-backfill', 0))",
    )
    .execute(&mut *transaction)
    .await
    .is_err()
    {
        return 0;
    }
    let inserted = match sqlx::query_scalar::<_, i64>(BACKFILL_TERMINAL_BUILD_RECORDS_SQL)
        .fetch_one(&mut *transaction)
        .await
    {
        Ok(inserted) => inserted.max(0) as u64,
        Err(_) => return 0,
    };
    if transaction.commit().await.is_err() {
        return 0;
    }
    inserted
}
