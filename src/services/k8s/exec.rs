use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use k8s_openapi::api::core::v1::Pod;
use kube::api::{Api, AttachParams, AttachedProcess};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::services::container::{
    ContainerExecAdmission, ContainerExecError, MAX_EXEC_OUTPUT_BYTES,
};
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
    fn into_exec_error(self) -> ContainerExecError {
        match self {
            Self::Read(error) => ContainerExecError::Platform(AppError::internal(format!(
                "failed to read Kubernetes exec output: {error}"
            ))),
            Self::OutputTooLarge => ContainerExecError::Participant(AppError::internal(
                "container exec output exceeded 1 MiB",
            )),
            Self::MissingStatus => ContainerExecError::Platform(AppError::internal(
                "Kubernetes exec ended without a process status",
            )),
        }
    }
}

fn classify_attach_error(id: &str, error: kube::Error) -> ContainerExecError {
    let participant = matches!(
        &error,
        kube::Error::Api(response) if matches!(response.code, 400 | 404 | 409 | 422)
    );
    let app_error = if matches!(&error, kube::Error::Api(response) if response.code == 404) {
        AppError::not_found(format!("pod not found: {id}"))
    } else {
        AppError::internal(format!("failed to start Kubernetes exec: {error}"))
    };
    if participant {
        ContainerExecError::Participant(app_error)
    } else {
        ContainerExecError::Platform(app_error)
    }
}

fn attach_timeout_error() -> ContainerExecError {
    ContainerExecError::Platform(AppError::internal("Kubernetes exec admission timed out"))
}

fn admit_attached<T>(attached: T, admission: &ContainerExecAdmission) -> T {
    admission.mark_admitted();
    attached
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
pub(super) async fn run_classified(
    pods: Api<Pod>,
    id: &str,
    cmd: Vec<String>,
    admission: ContainerExecAdmission,
) -> Result<String, ContainerExecError> {
    let deadline = tokio::time::Instant::now() + EXEC_TIMEOUT;
    let params = AttachParams {
        // Admission controllers may inject sidecars. The challenge container is
        // deliberately named after its pod during `create`.
        container: Some(id.to_string()),
        ..AttachParams::default()
    };
    let attached = tokio::time::timeout_at(deadline, pods.exec(id, cmd, &params))
        .await
        .map_err(|_| attach_timeout_error())?
        .map_err(|error| classify_attach_error(id, error))?;
    let mut process = ProcessGuard(admit_attached(attached, &admission));
    let stdout = process.0.stdout().ok_or_else(|| {
        ContainerExecError::Platform(AppError::internal("Kubernetes exec did not attach stdout"))
    })?;
    let stderr = process.0.stderr().ok_or_else(|| {
        ContainerExecError::Platform(AppError::internal("Kubernetes exec did not attach stderr"))
    })?;
    let status = process.0.take_status().ok_or_else(|| {
        ContainerExecError::Platform(AppError::internal(
            "Kubernetes exec did not attach process status",
        ))
    })?;
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
        .map_err(|_| {
            ContainerExecError::Participant(AppError::internal("Kubernetes exec timed out"))
        })?
        .map_err(DrainError::into_exec_error)?;
    Ok(String::from_utf8_lossy(&output).into_owned())
}

pub(super) async fn run(pods: Api<Pod>, id: &str, cmd: Vec<String>) -> AppResult<String> {
    run_classified(pods, id, cmd, ContainerExecAdmission::default())
        .await
        .map_err(ContainerExecError::into_app_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kubernetes_attach_timeout_is_platform_and_admission_waits_for_attach() {
        assert!(matches!(
            attach_timeout_error(),
            ContainerExecError::Platform(_)
        ));
        let admission = ContainerExecAdmission::default();
        assert!(!admission.is_admitted());
        admit_attached((), &admission);
        assert!(admission.is_admitted());
    }

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
