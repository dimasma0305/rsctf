use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use k8s_openapi::api::core::v1::Pod;
use kube::api::{Api, AttachParams, AttachedProcess};
use tokio::io::{AsyncRead, AsyncReadExt};

use super::is_not_found;
use crate::services::container::MAX_EXEC_OUTPUT_BYTES;
use crate::utils::error::{AppError, AppResult};

/// A backend-level deadline keeps callers other than the KotH checker from
/// leaving a remote command or websocket attached indefinitely.
const EXEC_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
enum DrainError {
    Read(std::io::Error),
    OutputTooLarge,
    MissingStatus,
}

impl From<std::io::Error> for DrainError {
    fn from(error: std::io::Error) -> Self {
        Self::Read(error)
    }
}

impl DrainError {
    fn into_app_error(self) -> AppError {
        match self {
            Self::Read(error) => {
                AppError::internal(format!("failed to read Kubernetes exec output: {error}"))
            }
            Self::OutputTooLarge => AppError::internal("container exec output exceeded 1 MiB"),
            Self::MissingStatus => {
                AppError::internal("Kubernetes exec ended without a process status")
            }
        }
    }
}

/// Aborts kube's background websocket task when this request future is
/// cancelled by an outer timeout (the KotH checker uses a shorter deadline).
struct ProcessGuard(AttachedProcess);

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn collect_stream<R>(
    mut stream: R,
    total_bytes: &AtomicUsize,
    byte_limit: usize,
) -> Result<Vec<u8>, DrainError>
where
    R: AsyncRead + Unpin,
{
    let mut output = Vec::new();
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(output);
        }
        if total_bytes
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                used.checked_add(read).filter(|next| *next <= byte_limit)
            })
            .is_err()
        {
            return Err(DrainError::OutputTooLarge);
        }
        output.extend_from_slice(&chunk[..read]);
    }
}

/// Run a command in the named challenge container and return deterministic
/// stdout-then-stderr output. Kubernetes exposes the streams independently, so
/// their exact interleaving is not available without allocating a TTY.
pub(super) async fn run(pods: Api<Pod>, id: &str, cmd: Vec<String>) -> AppResult<String> {
    let deadline = tokio::time::Instant::now() + EXEC_TIMEOUT;
    let params = AttachParams {
        // Admission controllers may inject sidecars. The challenge container is
        // deliberately named after its pod during `create`.
        container: Some(id.to_string()),
        ..AttachParams::default()
    };
    let attached = tokio::time::timeout_at(deadline, pods.exec(id, cmd, &params))
        .await
        .map_err(|_| AppError::internal("Kubernetes exec timed out"))?
        .map_err(|error| {
            if is_not_found(&error) {
                AppError::not_found(format!("pod not found: {id}"))
            } else {
                AppError::internal(format!("failed to start Kubernetes exec: {error}"))
            }
        })?;
    let mut process = ProcessGuard(attached);
    let stdout = process
        .0
        .stdout()
        .ok_or_else(|| AppError::internal("Kubernetes exec did not attach stdout"))?;
    let stderr = process
        .0
        .stderr()
        .ok_or_else(|| AppError::internal("Kubernetes exec did not attach stderr"))?;
    let status = process
        .0
        .take_status()
        .ok_or_else(|| AppError::internal("Kubernetes exec did not attach process status"))?;
    let total_bytes = AtomicUsize::new(0);

    let drain = async {
        let output = async {
            tokio::try_join!(
                collect_stream(stdout, &total_bytes, MAX_EXEC_OUTPUT_BYTES),
                collect_stream(stderr, &total_bytes, MAX_EXEC_OUTPUT_BYTES),
            )
        };
        let wait_for_status = async { status.await.ok_or(DrainError::MissingStatus).map(|_| ()) };
        let ((mut stdout, stderr), ()) = tokio::try_join!(output, wait_for_status)?;
        stdout.extend_from_slice(&stderr);
        Ok::<Vec<u8>, DrainError>(stdout)
    };

    let output = tokio::time::timeout_at(deadline, drain)
        .await
        .map_err(|_| AppError::internal("Kubernetes exec timed out"))?
        .map_err(DrainError::into_app_error)?;
    Ok(String::from_utf8_lossy(&output).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn streams_share_one_output_budget() {
        let total = AtomicUsize::new(0);

        assert_eq!(
            collect_stream(&b"12345"[..], &total, 9).await.unwrap(),
            b"12345"
        );
        assert_eq!(
            collect_stream(&b"6789"[..], &total, 9).await.unwrap(),
            b"6789"
        );
        assert_eq!(total.load(Ordering::Relaxed), 9);
    }

    #[tokio::test]
    async fn stream_rejects_output_over_combined_budget() {
        let total = AtomicUsize::new(7);

        assert!(matches!(
            collect_stream(&b"89"[..], &total, 8).await,
            Err(DrainError::OutputTooLarge)
        ));
        assert_eq!(total.load(Ordering::Relaxed), 7);
    }
}
