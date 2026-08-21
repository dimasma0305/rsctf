use std::time::Duration;

use super::DEFAULT_RECONCILE_SECONDS;

pub(super) fn capture_filename(container_id: &str) -> String {
    let prefix: String = container_id
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        .take(20)
        .collect();
    let prefix = if prefix.is_empty() {
        "container"
    } else {
        &prefix
    };
    let digest = crate::utils::codec::sha256_str(container_id);
    format!("{prefix}-{}-{}.pcap", &digest[..16], uuid::Uuid::now_v7())
}

pub(super) fn capture_device() -> String {
    std::env::var("RSCTF_CAPTURE_DEVICE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "any".to_string())
}

pub(super) fn reconcile_interval() -> Duration {
    let seconds = std::env::var("RSCTF_CAPTURE_RECONCILE_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| (1..=60).contains(seconds))
        .unwrap_or(DEFAULT_RECONCILE_SECONDS);
    Duration::from_secs(seconds)
}

pub(super) fn capture_enabled() -> bool {
    std::env::var("RSCTF_TRAFFIC_CAPTURE_ENABLED")
        .map(|value| matches!(value.as_str(), "1" | "true" | "yes"))
        .unwrap_or(true)
}

fn configured_bytes(name: &str, default: u64, minimum: u64, maximum: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| (minimum..=maximum).contains(value))
        .unwrap_or(default)
}

pub(super) fn capture_limits() -> crate::services::traffic::LiveCaptureLimits {
    const MIB: u64 = 1024 * 1024;
    let max_file_bytes = configured_bytes(
        "RSCTF_CAPTURE_MAX_FILE_BYTES",
        128 * MIB,
        MIB,
        4 * 1024 * MIB,
    );
    let max_directory_bytes = configured_bytes(
        "RSCTF_CAPTURE_MAX_PARTICIPATION_BYTES",
        256 * MIB,
        max_file_bytes,
        16 * 1024 * MIB,
    )
    .max(max_file_bytes);
    let free_space_floor_bytes = configured_bytes(
        "RSCTF_CAPTURE_FREE_SPACE_FLOOR_BYTES",
        512 * MIB,
        64 * MIB,
        64 * 1024 * MIB,
    );
    let max_file_seconds = configured_bytes("RSCTF_CAPTURE_MAX_FILE_SECONDS", 3_600, 60, 86_400);
    crate::services::traffic::LiveCaptureLimits {
        max_file_bytes,
        max_directory_bytes,
        free_space_floor_bytes,
        max_file_duration: Duration::from_secs(max_file_seconds),
    }
}

pub(super) fn retention_days() -> i64 {
    std::env::var("RSCTF_CAPTURE_RETENTION_DAYS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| (1..=3_650).contains(value))
        .unwrap_or(14)
}

pub(super) fn unexpected_exit_error(result: Result<Result<u64, String>, String>) -> String {
    match result {
        Ok(Ok(packets)) => format!("capture exited unexpectedly after {packets} packets"),
        Ok(Err(error)) | Err(error) => error,
    }
}

pub(super) async fn join_capture_thread(
    thread: std::thread::JoinHandle<Result<u64, String>>,
) -> Result<Result<u64, String>, String> {
    tokio::task::spawn_blocking(move || thread.join())
        .await
        .map_err(|error| format!("join task failed: {error}"))?
        .map_err(|_| "capture thread panicked".to_string())
}
