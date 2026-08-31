//! Bounded capture archive construction and streamed delivery.

use super::*;
use std::io::{Read, Write};

// ---------------------------------------------------------------------------
// Traffic capture metadata and pcap serving for the singleton capture worker.
// ---------------------------------------------------------------------------

const MAX_CAPTURE_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
// Stored ZIP records still need local/central headers and filenames. Reserve a
// conservative fixed envelope so the streamed response itself never exceeds
// the advertised/deployment-reserved 128 MiB ceiling.
const CAPTURE_ARCHIVE_METADATA_BYTES: u64 = 512 * 1024;
const MAX_CAPTURE_ARCHIVE_SOURCE_BYTES: u64 =
    MAX_CAPTURE_ARCHIVE_BYTES - CAPTURE_ARCHIVE_METADATA_BYTES;
const MAX_CAPTURE_ARCHIVE_FILES: usize = 256;
const MAX_CAPTURE_ARCHIVE_DEPLOYMENT_BYTES: i64 = 2 * MAX_CAPTURE_ARCHIVE_BYTES as i64;
const MAX_CAPTURE_ARCHIVE_DEPLOYMENT_JOBS: i64 = 2;
const CAPTURE_ARCHIVE_CHUNK_BYTES: usize = 64 * 1024;
const CAPTURE_ARCHIVE_LEASE_SECONDS: i64 = 30;
const CAPTURE_ARCHIVE_HEARTBEAT_SECONDS: u64 = 10;
const CAPTURE_ARCHIVE_DATABASE_SECONDS: u64 = 2;
const CAPTURE_ARCHIVE_STREAM_SECONDS: u64 = 300;
const CAPTURE_ARCHIVE_RETRY_SECONDS: u64 = 2;
const CAPTURE_ARCHIVE_ADVISORY_KEY: i64 = 1_195_722_091;
const FLOW_FILTER_SLOTS: usize = 2;
static CAPTURE_ARCHIVE_SLOTS: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(2)));
static FLOW_FILTER_CAPACITY: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| {
        std::sync::Arc::new(tokio::sync::Semaphore::new(FLOW_FILTER_SLOTS))
    });

async fn spawn_blocking_with_permit<T, F>(
    permit: tokio::sync::OwnedSemaphorePermit,
    work: F,
) -> Result<T, tokio::task::JoinError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
}

type CaptureZipChunk = Result<bytes::Bytes, std::io::Error>;

struct CaptureArchiveStream {
    inner: tokio_stream::wrappers::ReceiverStream<CaptureZipChunk>,
    _permit: tokio::sync::OwnedSemaphorePermit,
    completed: Option<tokio::sync::oneshot::Sender<()>>,
    lease_failed: std::pin::Pin<Box<tokio::sync::oneshot::Receiver<()>>>,
    deadline: std::pin::Pin<Box<tokio::time::Sleep>>,
    terminal: bool,
}

/// Owns the durable deployment-wide archive reservation while the request is
/// still preparing its inventory and filesystem snapshot. Dropping the owner
/// cancels the heartbeat and releases (or, after a database outage, lets expire)
/// the reservation, so every pre-response error has the same cleanup path.
struct CaptureArchiveLeaseOwner {
    completed: Option<tokio::sync::oneshot::Sender<()>>,
    lease_failed: Option<tokio::sync::oneshot::Receiver<()>>,
}

impl CaptureArchiveLeaseOwner {
    fn start(pool: sqlx::PgPool, operation_id: uuid::Uuid) -> Self {
        let (completed_sender, completed_receiver) = tokio::sync::oneshot::channel();
        let (lease_failed_sender, lease_failed_receiver) = tokio::sync::oneshot::channel();
        tokio::spawn(maintain_archive_lease(
            pool,
            operation_id,
            completed_receiver,
            lease_failed_sender,
        ));
        Self {
            completed: Some(completed_sender),
            lease_failed: Some(lease_failed_receiver),
        }
    }

    fn ensure_alive(&mut self) -> AppResult<()> {
        let lease_failed = self
            .lease_failed
            .as_mut()
            .expect("archive lease failure receiver is present");
        match lease_failed.try_recv() {
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => Ok(()),
            Ok(()) | Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                Err(AppError::retryable_unavailable(
                    "Capture archive admission was lost; retry shortly",
                    CAPTURE_ARCHIVE_RETRY_SECONDS,
                ))
            }
        }
    }

    fn into_stream_parts(
        mut self,
    ) -> (
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::oneshot::Receiver<()>,
    ) {
        (
            self.completed
                .take()
                .expect("archive lease completion owner is present"),
            self.lease_failed
                .take()
                .expect("archive lease failure receiver is present"),
        )
    }
}

impl Drop for CaptureArchiveLeaseOwner {
    fn drop(&mut self) {
        if let Some(completed) = self.completed.take() {
            let _ = completed.send(());
        }
    }
}

impl CaptureArchiveStream {
    fn finish(&mut self) {
        if let Some(completed) = self.completed.take() {
            let _ = completed.send(());
        }
    }
}

impl futures::Stream for CaptureArchiveStream {
    type Item = CaptureZipChunk;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        if self.terminal {
            return std::task::Poll::Ready(None);
        }
        if std::future::Future::poll(self.deadline.as_mut(), context).is_ready() {
            self.terminal = true;
            self.finish();
            return std::task::Poll::Ready(Some(Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "capture archive stream exceeded its delivery deadline",
            ))));
        }
        if std::future::Future::poll(self.lease_failed.as_mut(), context).is_ready() {
            self.terminal = true;
            self.finish();
            return std::task::Poll::Ready(Some(Err(std::io::Error::other(
                "capture archive admission lease was lost",
            ))));
        }
        match futures::Stream::poll_next(std::pin::Pin::new(&mut self.inner), context) {
            std::task::Poll::Ready(None) => {
                self.terminal = true;
                self.finish();
                std::task::Poll::Ready(None)
            }
            result => result,
        }
    }
}

impl Drop for CaptureArchiveStream {
    fn drop(&mut self) {
        self.finish();
    }
}

struct CaptureArchiveSource {
    path: std::path::PathBuf,
    entry: String,
}

struct CaptureZipStreamWriter {
    output: tokio::sync::mpsc::Sender<CaptureZipChunk>,
    buffered: Vec<u8>,
    sent: u64,
}

impl CaptureZipStreamWriter {
    fn new(output: tokio::sync::mpsc::Sender<CaptureZipChunk>) -> Self {
        Self {
            output,
            buffered: Vec::with_capacity(CAPTURE_ARCHIVE_CHUNK_BYTES),
            sent: 0,
        }
    }

    fn send_buffer(&mut self) -> std::io::Result<()> {
        if self.buffered.is_empty() {
            return Ok(());
        }
        let chunk = std::mem::replace(
            &mut self.buffered,
            Vec::with_capacity(CAPTURE_ARCHIVE_CHUNK_BYTES),
        );
        let next = self
            .sent
            .checked_add(chunk.len() as u64)
            .filter(|next| *next <= MAX_CAPTURE_ARCHIVE_BYTES)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::FileTooLarge,
                    "capture archive exceeded its response size limit",
                )
            })?;
        self.output
            .blocking_send(Ok(bytes::Bytes::from(chunk)))
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::BrokenPipe, "client disconnected")
            })?;
        self.sent = next;
        Ok(())
    }

    fn finish(mut self) -> std::io::Result<()> {
        self.send_buffer()
    }
}

impl Write for CaptureZipStreamWriter {
    fn write(&mut self, mut input: &[u8]) -> std::io::Result<usize> {
        let input_len = input.len();
        while !input.is_empty() {
            let available = CAPTURE_ARCHIVE_CHUNK_BYTES - self.buffered.len();
            let take = available.min(input.len());
            self.buffered.extend_from_slice(&input[..take]);
            input = &input[take..];
            if self.buffered.len() == CAPTURE_ARCHIVE_CHUNK_BYTES {
                self.send_buffer()?;
            }
        }
        Ok(input_len)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.send_buffer()
    }
}

fn scan_capture_archive(
    dir: &std::path::Path,
    names: Vec<String>,
) -> AppResult<Vec<CaptureArchiveSource>> {
    if names.is_empty() {
        return Err(AppError::not_found("No captures for this participation"));
    }
    if names.len() > MAX_CAPTURE_ARCHIVE_FILES {
        return Err(AppError::bad_request(
            "Too many captures to archive; download them individually",
        ));
    }
    let files = names
        .into_iter()
        .map(|entry| {
            safe_capture_name(&entry)?;
            let path = dir.join(&entry);
            let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
                AppError::internal(format!("capture metadata {}: {error}", path.display()))
            })?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(AppError::not_found("Capture not found"));
            }
            Ok((CaptureArchiveSource { path, entry }, metadata.len()))
        })
        .collect::<AppResult<Vec<_>>>()?;
    let declared_total = files
        .iter()
        .try_fold(0u64, |total, (_, size)| total.checked_add(*size));
    if declared_total.is_none_or(|total| total > MAX_CAPTURE_ARCHIVE_SOURCE_BYTES) {
        return Err(AppError::bad_request(
            "Captures are too large to archive; download them individually",
        ));
    }
    Ok(files.into_iter().map(|(source, _)| source).collect())
}

async fn acquire_archive_lease(
    pool: &sqlx::PgPool,
    challenge_id: i32,
    participation_id: i32,
) -> AppResult<uuid::Uuid> {
    let mut transaction = match tokio::time::timeout(
        std::time::Duration::from_millis(250),
        crate::utils::database::begin_sqlx_transaction(pool),
    )
    .await
    {
        Ok(result) => result.map_err(|error| AppError::internal(error.to_string()))?,
        Err(_) => {
            return Err(AppError::retryable_unavailable(
                "Capture archive admission is busy; retry shortly",
                CAPTURE_ARCHIVE_RETRY_SECONDS,
            ));
        }
    };
    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
        .bind(CAPTURE_ARCHIVE_ADVISORY_KEY)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if !locked {
        transaction
            .rollback()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Err(AppError::retryable_unavailable(
            "Capture archive admission is busy; retry shortly",
            CAPTURE_ARCHIVE_RETRY_SECONDS,
        ));
    }
    sqlx::query(r#"DELETE FROM "TrafficArchiveLeases" WHERE expires_at_utc <= CURRENT_TIMESTAMP"#)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let (active, reserved): (i64, i64) = sqlx::query_as(
        r#"SELECT COUNT(*)::BIGINT, COALESCE(SUM(reserved_bytes), 0)::BIGINT
             FROM "TrafficArchiveLeases"
            WHERE expires_at_utc > CURRENT_TIMESTAMP"#,
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let duplicate: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
               SELECT 1 FROM "TrafficArchiveLeases"
                WHERE challenge_id = $1 AND participation_id = $2
                  AND expires_at_utc > CURRENT_TIMESTAMP
           )"#,
    )
    .bind(challenge_id)
    .bind(participation_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if duplicate
        || active >= MAX_CAPTURE_ARCHIVE_DEPLOYMENT_JOBS
        || reserved.saturating_add(MAX_CAPTURE_ARCHIVE_BYTES as i64)
            > MAX_CAPTURE_ARCHIVE_DEPLOYMENT_BYTES
    {
        transaction
            .rollback()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Err(AppError::retryable_unavailable(
            "Capture archive capacity is busy; retry shortly",
            CAPTURE_ARCHIVE_RETRY_SECONDS,
        ));
    }

    let operation_id = uuid::Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "TrafficArchiveLeases"
               (operation_id, challenge_id, participation_id, reserved_bytes, expires_at_utc)
           VALUES ($1, $2, $3, $4,
                   CURRENT_TIMESTAMP + make_interval(secs => $5))"#,
    )
    .bind(operation_id)
    .bind(challenge_id)
    .bind(participation_id)
    .bind(MAX_CAPTURE_ARCHIVE_BYTES as i64)
    .bind(CAPTURE_ARCHIVE_LEASE_SECONDS as f64)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(operation_id)
}

async fn maintain_archive_lease(
    pool: sqlx::PgPool,
    operation_id: uuid::Uuid,
    mut completed: tokio::sync::oneshot::Receiver<()>,
    lease_failed: tokio::sync::oneshot::Sender<()>,
) {
    let mut lease_failed = Some(lease_failed);
    let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(
        CAPTURE_ARCHIVE_HEARTBEAT_SECONDS,
    ));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    'heartbeat: loop {
        tokio::select! {
            biased;
            _ = &mut completed => break,
            _ = heartbeat.tick() => {
                let renewal = tokio::time::timeout(
                    std::time::Duration::from_secs(CAPTURE_ARCHIVE_DATABASE_SECONDS),
                    sqlx::query(
                        r#"UPDATE "TrafficArchiveLeases"
                              SET expires_at_utc = CURRENT_TIMESTAMP + make_interval(secs => $2)
                            WHERE operation_id = $1"#,
                    )
                    .bind(operation_id)
                    .bind(CAPTURE_ARCHIVE_LEASE_SECONDS as f64)
                    .execute(&pool),
                );
                let renewed = tokio::select! {
                    biased;
                    _ = &mut completed => break 'heartbeat,
                    renewed = renewal => renewed,
                };
                match renewed {
                    Ok(Ok(result)) if result.rows_affected() == 1 => {}
                    Ok(Ok(_)) => {
                        tracing::warn!(%operation_id, "capture archive lease disappeared");
                        if let Some(sender) = lease_failed.take() {
                            let _ = sender.send(());
                        }
                        break;
                    }
                    Ok(Err(error)) => {
                        tracing::warn!(%operation_id, %error, "capture archive lease heartbeat failed");
                        if let Some(sender) = lease_failed.take() {
                            let _ = sender.send(());
                        }
                        break;
                    }
                    Err(_) => {
                        tracing::warn!(%operation_id, "capture archive lease heartbeat timed out");
                        if let Some(sender) = lease_failed.take() {
                            let _ = sender.send(());
                        }
                        break;
                    }
                }
            }
        }
    }
    match tokio::time::timeout(
        std::time::Duration::from_secs(CAPTURE_ARCHIVE_DATABASE_SECONDS),
        sqlx::query(r#"DELETE FROM "TrafficArchiveLeases" WHERE operation_id = $1"#)
            .bind(operation_id)
            .execute(&pool),
    )
    .await
    {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            tracing::warn!(%operation_id, %error, "capture archive lease release failed");
        }
        Err(_) => {
            tracing::warn!(%operation_id, "capture archive lease release timed out");
        }
    }
}

/// `GET /api/game/games/{id}/captures`
/// Root dir for per-(challenge, participation) pcaps:
/// `{storage_root}/capture/{challengeId}/{participationId}/{name}.pcap`. This is
/// where a live NIC capture (`services::traffic::capture_live`) writes; the
/// endpoints below serve whatever is present, independent of how it got there.

pub async fn get_all_traffic(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path((cid, pid)): Path<(i32, i32)>,
) -> AppResult<Response> {
    let root = capture_root(&st);
    let dir = root.join(cid.to_string()).join(pid.to_string());
    let permit = CAPTURE_ARCHIVE_SLOTS
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            AppError::retryable_unavailable(
                "Capture archive capacity is busy; retry shortly",
                CAPTURE_ARCHIVE_RETRY_SECONDS,
            )
        })?;
    let operation_id = acquire_archive_lease(st.pg(), cid, pid).await?;
    // Start the durable heartbeat before either the inventory query or the
    // filesystem snapshot. A slow disk scan must not let another replica
    // reclaim this export's deployment-wide byte reservation.
    let mut lease_owner = CaptureArchiveLeaseOwner::start(st.pg().clone(), operation_id);
    let names =
        crate::services::traffic::inventory::archive_file_names(st.pg(), &root, cid, pid).await?;
    lease_owner.ensure_alive()?;
    let sources = tokio::task::spawn_blocking(move || scan_capture_archive(&dir, names))
        .await
        .map_err(|error| {
            AppError::internal(format!("capture archive scan task failed: {error}"))
        })??;
    lease_owner.ensure_alive()?;
    let (output_sender, output_receiver) = tokio::sync::mpsc::channel::<CaptureZipChunk>(8);
    let error_sender = output_sender.clone();
    let (completed_sender, lease_failed_receiver) = lease_owner.into_stream_parts();
    tokio::task::spawn_blocking(move || {
        let outcome = (|| -> AppResult<()> {
            let writer = CaptureZipStreamWriter::new(output_sender);
            let mut zip = zip::ZipWriter::new_stream(writer);
            let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            let mut written = 0u64;
            for source in sources {
                zip.start_file(source.entry, opts)
                    .map_err(|err| AppError::internal(format!("zip: {err}")))?;
                let file = std::fs::File::open(source.path)
                    .map_err(|error| AppError::internal(format!("capture open: {error}")))?;
                let remaining = MAX_CAPTURE_ARCHIVE_SOURCE_BYTES.saturating_sub(written);
                let copied = std::io::copy(&mut file.take(remaining + 1), &mut zip)
                    .map_err(|error| AppError::internal(format!("zip: {error}")))?;
                if copied > remaining {
                    return Err(AppError::bad_request(
                        "Captures grew beyond the archive size limit",
                    ));
                }
                written += copied;
            }
            let writer = zip
                .finish()
                .map_err(|err| AppError::internal(format!("zip: {err}")))?;
            writer
                .into_inner()
                .finish()
                .map_err(|error| AppError::internal(format!("zip stream: {error}")))?;
            Ok(())
        })();
        if let Err(error) = outcome {
            let _ = error_sender.blocking_send(Err(std::io::Error::other(error.to_string())));
        }
    });
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"captures_{cid}_{pid}.zip\""),
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
        ],
        Body::from_stream(CaptureArchiveStream {
            inner: tokio_stream::wrappers::ReceiverStream::new(output_receiver),
            _permit: permit,
            completed: Some(completed_sender),
            lease_failed: Box::pin(lease_failed_receiver),
            deadline: Box::pin(tokio::time::sleep(std::time::Duration::from_secs(
                CAPTURE_ARCHIVE_STREAM_SECONDS,
            ))),
            terminal: false,
        }),
    )
        .into_response())
}

#[cfg(test)]
#[path = "traffic_tests.rs"]
mod tests;
