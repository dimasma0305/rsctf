//! Final hosted A&D service snapshot capture and retention policy.

use std::io::Write;
use std::sync::OnceLock;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use flate2::{write::GzEncoder, Compression};

use crate::app_state::SharedState;
use crate::services::container::ContainerBackendKind;
use crate::utils::error::{AppError, AppResult};

pub(crate) const MAX_STORED_SNAPSHOT_BYTES: usize = 128 * 1024 * 1024;
pub(crate) const SNAPSHOT_CONTENT_TYPE: &str = "application/gzip";
const SNAPSHOT_PIPELINE_ADMISSION_TIMEOUT: StdDuration = StdDuration::from_secs(5);

fn snapshot_pipeline_slots() -> &'static tokio::sync::Semaphore {
    static SLOTS: OnceLock<tokio::sync::Semaphore> = OnceLock::new();
    SLOTS.get_or_init(|| tokio::sync::Semaphore::new(1))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FinalSnapshotStatus {
    Available,
    NotRequired,
}

#[derive(Debug, sqlx::FromRow)]
struct SnapshotPolicy {
    participation_id: i32,
    challenge_id: i32,
    container_id: Option<String>,
    ad_self_hosted: bool,
    ad_allow_snapshot_download: bool,
    end_time_utc: DateTime<Utc>,
    ad_snapshot_retention_days: Option<i32>,
}

fn expiry(policy: &SnapshotPolicy) -> Option<DateTime<Utc>> {
    policy
        .ad_snapshot_retention_days
        .map(|days| policy.end_time_utc + Duration::days(i64::from(days)))
}

fn capture_supported(backend: ContainerBackendKind) -> bool {
    backend == ContainerBackendKind::Docker
}

pub(crate) fn archive_name(participation_id: i32, challenge_id: i32) -> String {
    format!("ad-snapshot-team{participation_id}-challenge{challenge_id}.tar.gz")
}

#[derive(Debug, thiserror::Error)]
#[error("compressed snapshot exceeds its storage limit")]
struct ArchiveLimitExceeded;

struct BoundedArchiveWriter {
    bytes: Vec<u8>,
    limit: usize,
}

impl BoundedArchiveWriter {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }
}

impl Write for BoundedArchiveWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let next = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other(ArchiveLimitExceeded))?;
        if next > self.limit {
            return Err(std::io::Error::other(ArchiveLimitExceeded));
        }
        self.bytes
            .try_reserve(bytes.len())
            .map_err(std::io::Error::other)?;
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn is_archive_limit(error: &std::io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|source| source.is::<ArchiveLimitExceeded>())
}

fn compress_tar_with_limit(tar: &[u8], limit: usize) -> AppResult<Vec<u8>> {
    let mut encoder = GzEncoder::new(BoundedArchiveWriter::new(limit), Compression::fast());
    if let Err(error) = encoder.write_all(tar) {
        return if is_archive_limit(&error) {
            Err(AppError::payload_too_large(format!(
                "compressed snapshot exceeds the {} MiB safety limit",
                limit / (1024 * 1024)
            )))
        } else {
            Err(AppError::internal(format!(
                "failed to compress snapshot: {error}"
            )))
        };
    }
    match encoder.finish() {
        Ok(writer) => Ok(writer.bytes),
        Err(error) if is_archive_limit(&error) => Err(AppError::payload_too_large(format!(
            "compressed snapshot exceeds the {} MiB safety limit",
            limit / (1024 * 1024)
        ))),
        Err(error) => Err(AppError::internal(format!(
            "failed to finish snapshot compression: {error}"
        ))),
    }
}

/// Compress a raw Docker filesystem TAR away from Tokio's worker threads.
async fn compress_tar(tar: Vec<u8>) -> AppResult<Vec<u8>> {
    tokio::task::spawn_blocking(move || compress_tar_with_limit(&tar, MAX_STORED_SNAPSHOT_BYTES))
        .await
        .map_err(|error| AppError::internal(format!("snapshot compression task failed: {error}")))?
}

/// Bound the complete raw-export + compression pipeline, not just Docker's
/// stream. Holding this permit through compression prevents queued requests
/// from accumulating several 512 MiB raw TAR buffers in one replica.
pub(crate) async fn export_archive(
    containers: &dyn crate::services::container::ContainerManager,
    container_id: &str,
) -> AppResult<Vec<u8>> {
    let _permit = tokio::time::timeout(
        SNAPSHOT_PIPELINE_ADMISSION_TIMEOUT,
        snapshot_pipeline_slots().acquire(),
    )
    .await
    .map_err(|_| AppError::unavailable("snapshot export capacity is busy; retry shortly"))?
    .map_err(|_| AppError::unavailable("snapshot export service is shutting down"))?;
    let tar = containers.export(container_id).await?;
    compress_tar(tar).await
}

fn permanent_size_failure(error: &AppError) -> bool {
    matches!(error, AppError::PayloadTooLarge(_))
}

/// Persist the exact final filesystem before its runtime identity is released.
///
/// Snapshot capture is intentionally restricted to Docker-backed hosted A&D
/// services: Docker exposes an engine-level filesystem export that does not
/// depend on utilities inside an untrusted challenge image. Kubernetes has no
/// equivalent portable API, so retaining a pod there would never make capture
/// succeed and would turn event teardown into a permanent resource leak.
pub(crate) async fn capture_final_service(
    state: &SharedState,
    team_service_id: i32,
    expected_container_id: &str,
) -> AppResult<FinalSnapshotStatus> {
    if crate::services::blob_refs::load_service_snapshot(state.pg(), team_service_id)
        .await?
        .is_some()
    {
        return Ok(FinalSnapshotStatus::Available);
    }

    let policy = sqlx::query_as::<_, SnapshotPolicy>(
        r#"SELECT service.participation_id, service.challenge_id,
                  service.container_id, challenge.ad_self_hosted,
                  game.ad_allow_snapshot_download, game.end_time_utc,
                  game.ad_snapshot_retention_days
             FROM "AdTeamServices" service
             JOIN "GameChallenges" challenge
               ON challenge.id = service.challenge_id
              AND challenge.game_id = service.game_id
             JOIN "Games" game ON game.id = service.game_id
            WHERE service.id = $1"#,
    )
    .bind(team_service_id)
    .fetch_optional(state.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("A&D team service not found"))?;

    if policy.ad_self_hosted
        || !policy.ad_allow_snapshot_download
        || !capture_supported(state.containers.backend_kind())
    {
        return Ok(FinalSnapshotStatus::NotRequired);
    }
    let now = Utc::now();
    if now < policy.end_time_utc {
        return Err(AppError::conflict(
            "A&D service snapshot cannot be captured before event end",
        ));
    }
    let expires_at_utc = expiry(&policy);
    if expires_at_utc.is_some_and(|expires| expires <= now) {
        return Ok(FinalSnapshotStatus::NotRequired);
    }
    if policy.container_id.as_deref() != Some(expected_container_id) {
        return Err(AppError::conflict(
            "A&D service backend changed before final snapshot capture",
        ));
    }

    let archive = match export_archive(state.containers.as_ref(), expected_container_id).await {
        Ok(archive) => archive,
        Err(error) if permanent_size_failure(&error) => {
            tracing::warn!(
                team_service_id,
                %expected_container_id,
                %error,
                "A&D snapshot exceeded its safety limit; teardown will continue"
            );
            return Ok(FinalSnapshotStatus::NotRequired);
        }
        Err(error) => return Err(error),
    };
    let name = archive_name(policy.participation_id, policy.challenge_id);
    let stored = crate::services::blob_refs::store_service_snapshot(
        state.pg(),
        state.storage.as_ref(),
        team_service_id,
        expected_container_id,
        expires_at_utc,
        &name,
        &archive,
    )
    .await?;
    Ok(if stored.is_some() {
        FinalSnapshotStatus::Available
    } else {
        FinalSnapshotStatus::NotRequired
    })
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    fn policy(retention: Option<i32>) -> SnapshotPolicy {
        SnapshotPolicy {
            participation_id: 1,
            challenge_id: 2,
            container_id: Some("runtime".to_string()),
            ad_self_hosted: false,
            ad_allow_snapshot_download: true,
            end_time_utc: DateTime::from_timestamp(1_800_000_000, 0).unwrap(),
            ad_snapshot_retention_days: retention,
        }
    }

    #[test]
    fn retention_is_anchored_to_event_end_and_null_is_forever() {
        assert_eq!(
            expiry(&policy(Some(7))),
            Some(policy(Some(7)).end_time_utc + Duration::days(7))
        );
        assert_eq!(expiry(&policy(None)), None);
    }

    #[test]
    fn only_engine_level_docker_export_is_claimed_as_supported() {
        assert!(capture_supported(ContainerBackendKind::Docker));
        assert!(!capture_supported(ContainerBackendKind::Kubernetes));
        assert!(!capture_supported(ContainerBackendKind::Worker));
        assert!(!capture_supported(ContainerBackendKind::None));
    }

    #[test]
    fn retained_archive_is_deterministic_bounded_and_round_trips() {
        let tar = vec![b'R'; 2 * 1024 * 1024];
        let first = compress_tar_with_limit(&tar, 1024 * 1024).unwrap();
        let second = compress_tar_with_limit(&tar, 1024 * 1024).unwrap();
        assert_eq!(first, second);
        assert!(first.len() < tar.len());

        let mut restored = Vec::new();
        flate2::read::GzDecoder::new(first.as_slice())
            .read_to_end(&mut restored)
            .unwrap();
        assert_eq!(restored, tar);

        let error = compress_tar_with_limit(&tar, 16).unwrap_err();
        assert!(matches!(error, AppError::PayloadTooLarge(_)));
    }

    #[test]
    fn oversized_snapshot_is_permanent_and_uses_a_compressed_filename() {
        assert!(permanent_size_failure(&AppError::payload_too_large(
            "too large"
        )));
        assert!(!permanent_size_failure(&AppError::unavailable("retry")));
        assert_eq!(archive_name(7, 11), "ad-snapshot-team7-challenge11.tar.gz");
    }
}
