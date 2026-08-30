//! Traffic-capture serving: pcap listing/download/flows.
use super::*;
use std::io::{Read, Write};

// ---------------------------------------------------------------------------
// Traffic capture metadata and pcap serving for the singleton capture worker.
// ---------------------------------------------------------------------------

const MAX_CAPTURE_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_CAPTURE_ARCHIVE_FILES: usize = 256;
const MAX_CAPTURE_ARCHIVE_DEPLOYMENT_BYTES: i64 = 2 * MAX_CAPTURE_ARCHIVE_BYTES as i64;
const MAX_CAPTURE_ARCHIVE_DEPLOYMENT_JOBS: i64 = 2;
const CAPTURE_ARCHIVE_CHUNK_BYTES: usize = 64 * 1024;
const CAPTURE_ARCHIVE_LEASE_SECONDS: i64 = 30;
const CAPTURE_ARCHIVE_STREAM_SECONDS: u64 = 300;
const CAPTURE_ARCHIVE_RETRY_SECONDS: u64 = 2;
const CAPTURE_ARCHIVE_ADVISORY_KEY: i64 = 1_195_722_091;
const MAX_INSPECT_CAPTURE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_CAPTURE_FLOWS: usize = 20_000;
static CAPTURE_ARCHIVE_SLOTS: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(2)));
static CAPTURE_FLOW_SLOTS: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(2)));

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
}

impl CaptureZipStreamWriter {
    fn new(output: tokio::sync::mpsc::Sender<CaptureZipChunk>) -> Self {
        Self {
            output,
            buffered: Vec::with_capacity(CAPTURE_ARCHIVE_CHUNK_BYTES),
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
        self.output
            .blocking_send(Ok(bytes::Bytes::from(chunk)))
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "client disconnected"))
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
    if declared_total.is_none_or(|total| total > MAX_CAPTURE_ARCHIVE_BYTES) {
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
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
        .bind(CAPTURE_ARCHIVE_ADVISORY_KEY)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if !locked {
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
    let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(10));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = &mut completed => break,
            _ = heartbeat.tick() => {
                let renewed = sqlx::query(
                    r#"UPDATE "TrafficArchiveLeases"
                          SET expires_at_utc = CURRENT_TIMESTAMP + make_interval(secs => $2)
                        WHERE operation_id = $1"#,
                )
                .bind(operation_id)
                .bind(CAPTURE_ARCHIVE_LEASE_SECONDS as f64)
                .execute(&pool)
                .await;
                match renewed {
                    Ok(result) if result.rows_affected() == 1 => {}
                    Ok(_) => {
                        tracing::warn!(%operation_id, "capture archive lease disappeared");
                        if let Some(sender) = lease_failed.take() {
                            let _ = sender.send(());
                        }
                        break;
                    }
                    Err(error) => {
                        tracing::warn!(%operation_id, %error, "capture archive lease heartbeat failed");
                        if let Some(sender) = lease_failed.take() {
                            let _ = sender.send(());
                        }
                        break;
                    }
                }
            }
        }
    }
    if let Err(error) = sqlx::query(r#"DELETE FROM "TrafficArchiveLeases" WHERE operation_id = $1"#)
        .bind(operation_id)
        .execute(&pool)
        .await
    {
        tracing::warn!(%operation_id, %error, "capture archive lease release failed");
    }
}

/// `GET /api/game/games/{id}/captures`
/// Root dir for per-(challenge, participation) pcaps:
/// `{storage_root}/capture/{challengeId}/{participationId}/{name}.pcap`. This is
/// where a live NIC capture (`services::traffic::capture_live`) writes; the
/// endpoints below serve whatever is present, independent of how it got there.
fn capture_root(st: &SharedState) -> std::path::PathBuf {
    std::path::PathBuf::from(&st.config.storage_root).join("capture")
}

/// Reject path-traversal in a URL-supplied file name.
fn safe_capture_name(name: &str) -> AppResult<&str> {
    if name.is_empty()
        || name.len() > 255
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || name.chars().any(|character| {
            character.is_control() || matches!(character, '"' | '\'' | '\r' | '\n')
        })
        || !name
            .rsplit_once('.')
            .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("pcap"))
    {
        return Err(AppError::bad_request("Invalid capture file name"));
    }
    Ok(name)
}

/// `GET /api/game/games/{id}/captures` — each challenge + its total pcap count.
pub async fn game_captures(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<Vec<crate::services::traffic::inventory::ChallengeCaptureItem>>> {
    let root = capture_root(&st);
    let page = crate::services::traffic::inventory::challenge_page(
        st.pg(),
        &root,
        id,
        &crate::services::traffic::inventory::CapturePageQuery::capped(100),
    )
    .await?;
    Ok(RequestResponse::ok(page.items))
}

/// Cursor-paged challenge capture inventory for large monitor views.
pub async fn game_captures_page(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path(id): Path<i32>,
    Query(query): Query<crate::services::traffic::inventory::CapturePageQuery>,
) -> AppResult<
    RequestResponse<
        crate::services::traffic::inventory::CapturePage<
            crate::services::traffic::inventory::ChallengeCaptureItem,
        >,
    >,
> {
    let root = capture_root(&st);
    let page =
        crate::services::traffic::inventory::challenge_page(st.pg(), &root, id, &query).await?;
    Ok(RequestResponse::ok(page))
}

/// `GET /api/game/captures/{challengeId}` — one row per participation with pcaps.
pub async fn team_traffic(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path(cid): Path<i32>,
) -> AppResult<RequestResponse<Vec<crate::services::traffic::inventory::TeamCaptureItem>>> {
    let root = capture_root(&st);
    let page = crate::services::traffic::inventory::team_page(
        st.pg(),
        &root,
        cid,
        &crate::services::traffic::inventory::CapturePageQuery::capped(100),
    )
    .await?;
    Ok(RequestResponse::ok(page.items))
}

/// Cursor-paged team capture inventory using one bounded SQL join.
pub async fn team_traffic_page(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path(cid): Path<i32>,
    Query(query): Query<crate::services::traffic::inventory::CapturePageQuery>,
) -> AppResult<
    RequestResponse<
        crate::services::traffic::inventory::CapturePage<
            crate::services::traffic::inventory::TeamCaptureItem,
        >,
    >,
> {
    let root = capture_root(&st);
    let page = crate::services::traffic::inventory::team_page(st.pg(), &root, cid, &query).await?;
    Ok(RequestResponse::ok(page))
}

/// `GET /api/game/captures/{challengeId}/{partId}` — the pcap files (FileRecord).
pub async fn traffic_files(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path((cid, pid)): Path<(i32, i32)>,
) -> AppResult<RequestResponse<Vec<crate::services::traffic::inventory::CaptureFileItem>>> {
    let root = capture_root(&st);
    let page = crate::services::traffic::inventory::file_page(
        st.pg(),
        &root,
        cid,
        pid,
        &crate::services::traffic::inventory::CapturePageQuery::capped(100),
    )
    .await?;
    Ok(RequestResponse::ok(page.items))
}

/// Cursor-paged file inventory, newest first with a stable filename tie-break.
pub async fn traffic_files_page(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path((cid, pid)): Path<(i32, i32)>,
    Query(query): Query<crate::services::traffic::inventory::CapturePageQuery>,
) -> AppResult<
    RequestResponse<
        crate::services::traffic::inventory::CapturePage<
            crate::services::traffic::inventory::CaptureFileItem,
        >,
    >,
> {
    let root = capture_root(&st);
    let page =
        crate::services::traffic::inventory::file_page(st.pg(), &root, cid, pid, &query).await?;
    Ok(RequestResponse::ok(page))
}

/// `GET /api/game/captures/{challengeId}/{partId}/all` — zip of the pcaps.
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
    let names =
        match crate::services::traffic::inventory::archive_file_names(st.pg(), &root, cid, pid)
            .await
        {
            Ok(names) => names,
            Err(error) => {
                let _ =
                    sqlx::query(r#"DELETE FROM "TrafficArchiveLeases" WHERE operation_id = $1"#)
                        .bind(operation_id)
                        .execute(st.pg())
                        .await;
                return Err(error);
            }
        };
    let sources = match tokio::task::spawn_blocking(move || scan_capture_archive(&dir, names)).await
    {
        Ok(Ok(sources)) => sources,
        Ok(Err(error)) => {
            let _ = sqlx::query(r#"DELETE FROM "TrafficArchiveLeases" WHERE operation_id = $1"#)
                .bind(operation_id)
                .execute(st.pg())
                .await;
            return Err(error);
        }
        Err(error) => {
            let _ = sqlx::query(r#"DELETE FROM "TrafficArchiveLeases" WHERE operation_id = $1"#)
                .bind(operation_id)
                .execute(st.pg())
                .await;
            return Err(AppError::internal(format!(
                "capture archive scan task failed: {error}"
            )));
        }
    };
    let (output_sender, output_receiver) = tokio::sync::mpsc::channel::<CaptureZipChunk>(8);
    let error_sender = output_sender.clone();
    let (completed_sender, completed_receiver) = tokio::sync::oneshot::channel();
    let (lease_failed_sender, lease_failed_receiver) = tokio::sync::oneshot::channel();
    tokio::spawn(maintain_archive_lease(
        st.pg().clone(),
        operation_id,
        completed_receiver,
        lease_failed_sender,
    ));
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
                let remaining = MAX_CAPTURE_ARCHIVE_BYTES.saturating_sub(written);
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

/// `DELETE /api/game/captures/{challengeId}/{partId}/all`
pub async fn delete_all_traffic(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path((cid, pid)): Path<(i32, i32)>,
) -> AppResult<StatusCode> {
    let dir = capture_root(&st)
        .join(cid.to_string())
        .join(pid.to_string());
    crate::services::traffic::inventory::mark_reconcile_required(st.pg()).await?;
    if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
        if error.kind() != std::io::ErrorKind::NotFound {
            crate::services::traffic::inventory::mark_reconcile_required_after_failure(st.pg())
                .await;
            return Err(AppError::internal(format!(
                "could not delete captures: {error}"
            )));
        }
    }
    if let Err(error) = crate::services::traffic::inventory::delete_bucket(st.pg(), cid, pid).await
    {
        crate::services::traffic::inventory::mark_reconcile_required_after_failure(st.pg()).await;
        return Err(error);
    }
    Ok(StatusCode::OK)
}

/// `GET /api/game/captures/{challengeId}/{partId}/{filename}` — download one pcap.
pub async fn get_traffic_file(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path((cid, pid, filename)): Path<(i32, i32, String)>,
) -> AppResult<Response> {
    let name = safe_capture_name(&filename)?;
    let path = capture_root(&st)
        .join(cid.to_string())
        .join(pid.to_string())
        .join(name);
    let path_metadata = tokio::fs::symlink_metadata(&path)
        .await
        .map_err(|_| AppError::not_found("Capture not found"))?;
    if path_metadata.file_type().is_symlink() || !path_metadata.is_file() {
        return Err(AppError::not_found("Capture not found"));
    }
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| AppError::not_found("Capture not found"))?;
    let size = file
        .metadata()
        .await
        .map_err(|_| AppError::not_found("Capture not found"))?
        .len();
    // Snapshot the size observed above. An active capture may keep growing;
    // without `take`, one download could chase the writer indefinitely and no
    // longer match its Content-Length.
    let body = Body::from_stream(tokio_util::io::ReaderStream::new(
        tokio::io::AsyncReadExt::take(file, size),
    ));
    Ok((
        [
            (
                header::CONTENT_TYPE,
                "application/vnd.tcpdump.pcap".to_string(),
            ),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
            (header::CONTENT_LENGTH, size.to_string()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
        ],
        body,
    )
        .into_response())
}

/// `DELETE /api/game/captures/{challengeId}/{partId}/{filename}`
pub async fn delete_traffic_file(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path((cid, pid, filename)): Path<(i32, i32, String)>,
) -> AppResult<StatusCode> {
    let name = safe_capture_name(&filename)?;
    let path = capture_root(&st)
        .join(cid.to_string())
        .join(pid.to_string())
        .join(name);
    crate::services::traffic::inventory::mark_reconcile_required(st.pg()).await?;
    if let Err(error) = tokio::fs::remove_file(&path).await {
        if error.kind() != std::io::ErrorKind::NotFound {
            crate::services::traffic::inventory::mark_reconcile_required_after_failure(st.pg())
                .await;
            return Err(AppError::internal(format!(
                "could not delete capture: {error}"
            )));
        }
    }
    if let Err(error) =
        crate::services::traffic::inventory::delete_file(st.pg(), cid, pid, name).await
    {
        crate::services::traffic::inventory::mark_reconcile_required_after_failure(st.pg()).await;
        return Err(error);
    }
    Ok(StatusCode::OK)
}

/// `GET /api/game/captures/{challengeId}/{partId}/{filename}/flows` — the TCP/UDP
/// flows parsed out of the pcap (`services::traffic::list_flows`).
pub async fn traffic_flows(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path((cid, pid, filename)): Path<(i32, i32, String)>,
) -> AppResult<RequestResponse<Vec<Json>>> {
    let name = safe_capture_name(&filename)?;
    let path = capture_root(&st)
        .join(cid.to_string())
        .join(pid.to_string())
        .join(name);
    let permit = CAPTURE_FLOW_SLOTS
        .clone()
        .try_acquire_owned()
        .map_err(|_| AppError::unavailable("Capture inspection capacity is busy; retry shortly"))?;
    let flows = spawn_blocking_with_permit(permit, move || {
        crate::services::traffic::list_flows_bounded(
            &path,
            MAX_INSPECT_CAPTURE_BYTES,
            MAX_CAPTURE_FLOWS,
        )
    })
    .await
    .map_err(|error| AppError::internal(format!("capture inspection task failed: {error}")))??;
    let out = flows
        .into_iter()
        .map(|f| {
            serde_json::json!({
                "src": f.src, "dst": f.dst,
                "packetCount": f.packet_count, "bytes": f.bytes,
            })
        })
        .collect();
    Ok(RequestResponse::ok(out))
}

/// `GET /api/game/captures/{challengeId}/{partId}/{filename}/flow/{connectionPort}`
/// — the flow whose src or dst uses `connectionPort`.
pub async fn traffic_flow_detail(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path((cid, pid, filename, connection_port)): Path<(i32, i32, String, i32)>,
) -> AppResult<RequestResponse<TrafficFlowDetail>> {
    let name = safe_capture_name(&filename)?;
    let path = capture_root(&st)
        .join(cid.to_string())
        .join(pid.to_string())
        .join(name);
    let port = connection_port.to_string();
    let permit = CAPTURE_FLOW_SLOTS
        .clone()
        .try_acquire_owned()
        .map_err(|_| AppError::unavailable("Capture inspection capacity is busy; retry shortly"))?;
    let flows = spawn_blocking_with_permit(permit, move || {
        crate::services::traffic::list_flows_bounded(
            &path,
            MAX_INSPECT_CAPTURE_BYTES,
            MAX_CAPTURE_FLOWS,
        )
    })
    .await
    .map_err(|error| AppError::internal(format!("capture inspection task failed: {error}")))??;
    let flow = flows
        .into_iter()
        .find(|f| f.src.ends_with(&format!(":{port}")) || f.dst.ends_with(&format!(":{port}")));
    Ok(RequestResponse::ok(TrafficFlowDetail {
        connection_port,
        peer_ip: flow
            .as_ref()
            .map(|f| {
                f.dst
                    .rsplit_once(':')
                    .map(|(ip, _)| ip.to_string())
                    .unwrap_or_else(|| f.dst.clone())
            })
            .unwrap_or_default(),
        packets_in: flow.as_ref().map(|f| f.packet_count as i64).unwrap_or(0),
        bytes_in: flow.as_ref().map(|f| f.bytes as i64).unwrap_or(0),
        ..Default::default()
    }))
}

#[cfg(test)]
mod traffic_admission_tests {
    use super::spawn_blocking_with_permit;
    use super::*;
    use std::io::Cursor;
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn blocking_work_retains_admission_after_waiter_cancellation() {
        let gate = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = gate.clone().acquire_owned().await.unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = std::sync::mpsc::channel();
        let waiter = tokio::spawn(spawn_blocking_with_permit(permit, move || {
            let _ = started_tx.send(());
            let _ = finish_rx.recv();
        }));

        started_rx.await.unwrap();
        waiter.abort();
        let _ = waiter.await;
        assert!(gate.clone().try_acquire_owned().is_err());

        finish_tx.send(()).unwrap();
        let released = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if let Ok(permit) = gate.clone().try_acquire_owned() {
                    break permit;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("blocking work retained the permit after it completed");
        drop(released);
    }

    #[test]
    fn traffic_zip_writer_streams_a_valid_archive() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel::<CaptureZipChunk>(1);
        let worker = std::thread::spawn(move || {
            let writer = CaptureZipStreamWriter::new(sender);
            let mut zip = zip::ZipWriter::new_stream(writer);
            zip.start_file("capture.pcap", zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"pcap-data").unwrap();
            zip.finish().unwrap().into_inner().finish().unwrap();
        });

        let mut bytes = Vec::new();
        while let Some(chunk) = receiver.blocking_recv() {
            bytes.extend_from_slice(&chunk.unwrap());
        }
        worker.join().unwrap();

        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut contents = Vec::new();
        archive
            .by_name("capture.pcap")
            .unwrap()
            .read_to_end(&mut contents)
            .unwrap();
        assert_eq!(contents, b"pcap-data");
    }

    #[test]
    fn deployment_budget_reserves_the_full_per_export_ceiling() {
        assert_eq!(
            MAX_CAPTURE_ARCHIVE_DEPLOYMENT_BYTES,
            MAX_CAPTURE_ARCHIVE_DEPLOYMENT_JOBS * MAX_CAPTURE_ARCHIVE_BYTES as i64
        );
        assert!(CAPTURE_ARCHIVE_LEASE_SECONDS > 2 * 10);
    }

    #[test]
    fn archive_overload_is_retryable() {
        use axum::response::IntoResponse as _;

        let response =
            AppError::retryable_unavailable("capture archive busy", CAPTURE_ARCHIVE_RETRY_SECONDS)
                .into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response.headers()[header::RETRY_AFTER].to_str().unwrap(),
            CAPTURE_ARCHIVE_RETRY_SECONDS.to_string().as_str()
        );
    }

    #[tokio::test]
    async fn response_stream_retains_admission_until_disconnect() {
        let admission = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        let permit = admission.clone().try_acquire_owned().unwrap();
        let (_sender, receiver) = tokio::sync::mpsc::channel(1);
        let (completed, released) = tokio::sync::oneshot::channel();
        let (_lease_failed, lease_failure) = tokio::sync::oneshot::channel();
        let stream = CaptureArchiveStream {
            inner: tokio_stream::wrappers::ReceiverStream::new(receiver),
            _permit: permit,
            completed: Some(completed),
            lease_failed: Box::pin(lease_failure),
            deadline: Box::pin(tokio::time::sleep(std::time::Duration::from_secs(30))),
            terminal: false,
        };

        assert!(admission.clone().try_acquire_owned().is_err());
        drop(stream);
        released.await.unwrap();
        assert!(admission.try_acquire_owned().is_ok());
    }

    #[tokio::test]
    async fn final_buffered_archive_chunk_retains_admission_until_eof() {
        use futures::StreamExt as _;

        let admission = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        let permit = admission.clone().try_acquire_owned().unwrap();
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        sender
            .send(Ok(bytes::Bytes::from_static(b"last-archive-byte")))
            .await
            .unwrap();
        drop(sender);
        let (completed, released) = tokio::sync::oneshot::channel();
        let (_lease_failed, lease_failure) = tokio::sync::oneshot::channel();
        let mut stream = CaptureArchiveStream {
            inner: tokio_stream::wrappers::ReceiverStream::new(receiver),
            _permit: permit,
            completed: Some(completed),
            lease_failed: Box::pin(lease_failure),
            deadline: Box::pin(tokio::time::sleep(std::time::Duration::from_secs(30))),
            terminal: false,
        };

        assert_eq!(
            stream.next().await.unwrap().unwrap(),
            bytes::Bytes::from_static(b"last-archive-byte")
        );
        assert!(admission.clone().try_acquire_owned().is_err());
        assert!(stream.next().await.is_none());
        released.await.unwrap();
        // The stream object still owns admission after the sender reaches EOF;
        // it is released only after Axum drops the completed response body.
        assert!(admission.clone().try_acquire_owned().is_err());
        drop(stream);
        assert!(admission.try_acquire_owned().is_ok());
    }
}
