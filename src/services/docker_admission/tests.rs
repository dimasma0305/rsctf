use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;

use super::*;

fn limits(
    read_concurrency: usize,
    read_deadline_ms: u64,
    queue_wait_ms: u64,
) -> DockerAdmissionLimits {
    DockerAdmissionLimits {
        read_concurrency,
        read_deadline: Duration::from_millis(read_deadline_ms),
        read_queue_wait: Duration::from_millis(queue_wait_ms),
        lifecycle_concurrency: 1,
        lifecycle_deadline: Duration::from_millis(read_deadline_ms),
        lifecycle_queue_wait: Duration::from_millis(queue_wait_ms),
        pull_deadline: Duration::from_millis(read_deadline_ms * 4),
    }
}

/// Sets a flag when dropped so a test can prove the wrapped future (and the
/// response stream it owns) was released rather than left pending.
struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn concurrency_cap_rejects_the_extra_caller_with_a_retryable_overload() {
    let admission = Arc::new(DockerAdmission::new(limits(1, 5_000, 50)));
    let (release_tx, release_rx) = oneshot::channel::<()>();
    let holder = {
        let admission = Arc::clone(&admission);
        tokio::spawn(async move {
            admission
                .read("inspect_container", async move {
                    let _ = release_rx.await;
                    7_u8
                })
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(admission.metrics().read.in_flight, 1);

    let rejected = admission.read("list_containers", async { 1_u8 }).await;
    assert!(matches!(
        rejected,
        Err(DockerAdmissionError::Overloaded {
            class: DockerOperationClass::Read,
            operation: "list_containers",
            ..
        })
    ));
    let metrics = admission.metrics().read;
    assert_eq!(metrics.overloaded, 1);
    assert_eq!(metrics.admitted, 1);
    assert_eq!(metrics.queued, 0);

    let app_error: AppError = rejected.unwrap_err().into();
    assert!(matches!(
        app_error,
        AppError::RetryableUnavailable { retry_after, .. } if retry_after >= 1
    ));

    release_tx.send(()).unwrap();
    assert_eq!(holder.await.unwrap(), Ok(7));
    assert_eq!(admission.read("stats", async { 2_u8 }).await, Ok(2));
    let metrics = admission.metrics().read;
    assert_eq!(metrics.in_flight, 0);
    assert_eq!(metrics.completed, 2);
}

#[tokio::test]
async fn queued_caller_is_admitted_when_a_slot_frees_and_records_its_wait() {
    let admission = Arc::new(DockerAdmission::new(limits(1, 5_000, 2_000)));
    let (release_tx, release_rx) = oneshot::channel::<()>();
    let holder = {
        let admission = Arc::clone(&admission);
        tokio::spawn(async move {
            admission
                .lifecycle("create_container", async move {
                    let _ = release_rx.await;
                })
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    let releaser = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(60)).await;
        release_tx.send(()).unwrap();
    });
    let waited_at = Instant::now();
    admission
        .lifecycle("remove_container", async {})
        .await
        .unwrap();
    assert!(waited_at.elapsed() >= Duration::from_millis(50));
    holder.await.unwrap().unwrap();
    releaser.await.unwrap();

    let metrics = admission.metrics().lifecycle;
    assert_eq!(metrics.overloaded, 0);
    assert_eq!(metrics.completed, 2);
    assert!(metrics.queue_wait_max_ms >= 50);
    assert!(metrics.queue_wait_total_ms >= metrics.queue_wait_max_ms);
}

#[tokio::test]
async fn deadline_drops_the_wrapped_future_and_counts_a_timeout() {
    let admission = DockerAdmission::new(limits(4, 40, 100));
    let dropped = Arc::new(AtomicBool::new(false));
    let flag = DropFlag(Arc::clone(&dropped));
    let result = admission
        .read("logs", async move {
            let _flag = flag;
            std::future::pending::<()>().await;
        })
        .await;
    assert!(matches!(
        result,
        Err(DockerAdmissionError::Timeout {
            class: DockerOperationClass::Read,
            operation: "logs",
            ..
        })
    ));
    assert!(
        dropped.load(Ordering::SeqCst),
        "timed-out future must be dropped"
    );
    let metrics = admission.metrics().read;
    assert_eq!(metrics.timeouts, 1);
    assert_eq!(metrics.cancellations, 0);
    assert_eq!(metrics.in_flight, 0);
    assert_eq!(metrics.completed, 0);

    let app_error: AppError = result.unwrap_err().into();
    assert!(matches!(app_error, AppError::RetryableUnavailable { .. }));
}

#[tokio::test]
async fn caller_cancellation_releases_the_slot_and_counts_a_cancellation() {
    let admission = Arc::new(DockerAdmission::new(limits(1, 5_000, 50)));
    let task = {
        let admission = Arc::clone(&admission);
        tokio::spawn(async move { admission.read("stats", std::future::pending::<()>()).await })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(admission.metrics().read.in_flight, 1);
    task.abort();
    let _ = task.await;

    let metrics = admission.metrics().read;
    assert_eq!(metrics.in_flight, 0);
    assert_eq!(metrics.cancellations, 1);
    assert_eq!(metrics.timeouts, 0);
    assert_eq!(admission.read("inspect", async { 1 }).await, Ok(1));
}

#[tokio::test]
async fn pull_uses_the_lifecycle_slot_with_the_longer_deadline() {
    let admission = DockerAdmission::new(limits(1, 30, 50));
    let started = Instant::now();
    let result = admission
        .pull("create_image", async {
            tokio::time::sleep(Duration::from_millis(60)).await;
            "pulled"
        })
        .await;
    assert_eq!(result, Ok("pulled"));
    assert!(started.elapsed() >= Duration::from_millis(60));
    assert_eq!(admission.metrics().lifecycle.completed, 1);
    assert_eq!(admission.metrics().pull_deadline_ms, 120);
}

#[tokio::test]
async fn caller_budget_never_extends_the_class_deadline() {
    let admission = DockerAdmission::new(limits(1, 30, 50));
    let started = Instant::now();
    let result = admission
        .read_within(
            "inspect_image",
            Duration::from_secs(10),
            std::future::pending::<()>(),
        )
        .await;
    assert!(matches!(result, Err(DockerAdmissionError::Timeout { .. })));
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn environment_overrides_are_validated_and_fall_back_to_defaults() {
    let defaults = DockerAdmissionLimits::default();
    let parsed = DockerAdmissionLimits::from_lookup(|name| match name {
        READ_CONCURRENCY_ENV => Some("32".to_string()),
        READ_DEADLINE_ENV => Some("0".to_string()),
        READ_QUEUE_WAIT_ENV => Some(" 3 ".to_string()),
        LIFECYCLE_CONCURRENCY_ENV => Some("not-a-number".to_string()),
        LIFECYCLE_DEADLINE_ENV => Some("99999".to_string()),
        LIFECYCLE_QUEUE_WAIT_ENV => Some(String::new()),
        PULL_DEADLINE_ENV => Some("600".to_string()),
        _ => None,
    });
    assert_eq!(parsed.read_concurrency, 32);
    assert_eq!(parsed.read_deadline, defaults.read_deadline);
    assert_eq!(parsed.read_queue_wait, Duration::from_secs(3));
    assert_eq!(parsed.lifecycle_concurrency, defaults.lifecycle_concurrency);
    assert_eq!(parsed.lifecycle_deadline, defaults.lifecycle_deadline);
    assert_eq!(parsed.lifecycle_queue_wait, defaults.lifecycle_queue_wait);
    assert_eq!(parsed.pull_deadline, Duration::from_secs(600));
    assert_eq!(DockerAdmissionLimits::from_lookup(|_| None), defaults);
}

#[test]
fn metrics_snapshot_serializes_camel_case_for_the_admin_surface() {
    let admission = DockerAdmission::new(DockerAdmissionLimits::default());
    let json = serde_json::to_value(admission.metrics()).unwrap();
    assert_eq!(json["read"]["concurrencyLimit"], 16);
    assert_eq!(json["read"]["deadlineMs"], 5_000);
    assert_eq!(json["lifecycle"]["concurrencyLimit"], 4);
    assert_eq!(json["lifecycle"]["deadlineMs"], 30_000);
    assert_eq!(json["pullDeadlineMs"], 120_000);
    assert_eq!(json["read"]["daemonLatencyMaxMs"], 0);
}

/// A fake Engine API endpoint that answers one bounded `logs` request with a
/// chunked body that never ends, then reports how long it took for the client
/// to close the connection. The daemon-side close is the proof that a timed
/// out log read cannot stay active or busy-loop on a stalled stream.
async fn stalled_log_server() -> (std::net::SocketAddr, oneshot::Receiver<Duration>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (closed_tx, closed_rx) = oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut head = Vec::new();
        let mut buf = [0_u8; 1024];
        loop {
            let n = socket.read(&mut buf).await.unwrap();
            if n == 0 {
                return;
            }
            head.extend_from_slice(&buf[..n]);
            if head.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        assert!(
            head.starts_with(b"GET /"),
            "expected a logs request, got {:?}",
            String::from_utf8_lossy(&head)
        );
        // One multiplexed stdout frame, then silence with the body still open.
        let frame = [1, 0, 0, 0, 0, 0, 0, 5, b'h', b'e', b'l', b'l', b'o'];
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/vnd.docker.multiplexed-stream\r\nTransfer-Encoding: chunked\r\n\r\nd\r\n")
            .await
            .unwrap();
        socket.write_all(&frame).await.unwrap();
        socket.write_all(b"\r\n").await.unwrap();
        let stalled_at = Instant::now();
        loop {
            match socket.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        let _ = closed_tx.send(stalled_at.elapsed());
    });
    (addr, closed_rx)
}

#[tokio::test]
async fn timed_out_bounded_log_read_drops_its_daemon_stream() {
    let (addr, closed_rx) = stalled_log_server().await;
    let docker = bollard::Docker::connect_with_http(
        &format!("http://{addr}"),
        60,
        bollard::API_DEFAULT_VERSION,
    )
    .unwrap();
    let admission = DockerAdmission::new(limits(4, 200, 100));

    let started = Instant::now();
    let result = admission
        .read("logs", async {
            let mut stream = docker.logs::<String>(
                "stalled",
                Some(bollard::container::LogsOptions {
                    stdout: true,
                    stderr: false,
                    ..Default::default()
                }),
            );
            let mut collected = Vec::new();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                collected.extend_from_slice(chunk.as_ref());
            }
            Ok::<_, bollard::errors::Error>(collected)
        })
        .await;
    assert!(
        matches!(
            result,
            Err(DockerAdmissionError::Timeout {
                operation: "logs",
                ..
            })
        ),
        "expected a deadline timeout, got {result:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(5));

    let closed_after = tokio::time::timeout(Duration::from_secs(5), closed_rx)
        .await
        .expect("the fake daemon must observe the client closing the stalled stream")
        .unwrap();
    assert!(
        closed_after < Duration::from_secs(3),
        "close took {closed_after:?}"
    );

    let metrics = admission.metrics().read;
    assert_eq!(metrics.timeouts, 1);
    assert_eq!(metrics.in_flight, 0);
    assert_eq!(metrics.cancellations, 0);
}
