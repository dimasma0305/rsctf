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
