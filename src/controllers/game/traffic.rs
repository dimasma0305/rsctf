//! Traffic-capture serving: pcap listing/download/flows.
use super::*;
use base64::Engine as _;
use std::io::{Read, Write};

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub enum TrafficFlowDirection {
    ContainerToTeam,
    TeamToContainer,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrafficFlowSummary {
    pub connection_port: i32,
    pub first_seen_utc: i64,
    pub last_seen_utc: i64,
    pub peer_ip: String,
    pub packets_in: i64,
    pub packets_out: i64,
    pub bytes_in: i64,
    pub bytes_out: i64,
    pub flag_hits: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrafficFlowChunk {
    pub direction: TrafficFlowDirection,
    pub timestamp_utc: i64,
    pub payload_base64: String,
    pub flag_offsets: Vec<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrafficFlowDetail {
    #[serde(flatten)]
    pub summary: TrafficFlowSummary,
    pub chunks: Vec<TrafficFlowChunk>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TrafficFlowFilter {
    pub regex_pattern: Option<String>,
    pub peer_ip_contains: Option<String>,
    pub start_utc: Option<i64>,
    pub end_utc: Option<i64>,
    pub direction: Option<TrafficFlowDirection>,
    pub flags_only: Option<bool>,
    pub page: Option<u32>,
    pub page_size: Option<u32>,
}

// ---------------------------------------------------------------------------
// Traffic capture metadata and pcap serving for the singleton capture worker.
// ---------------------------------------------------------------------------

const MAX_CAPTURE_ARCHIVE_FILES: usize = 256;
const MAX_CAPTURE_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_CAPTURE_ARCHIVE_DEPLOYMENT_BYTES: i64 = 2 * MAX_CAPTURE_ARCHIVE_BYTES as i64;
const MAX_CAPTURE_ARCHIVE_DEPLOYMENT_JOBS: i64 = 2;
const CAPTURE_ARCHIVE_CHUNK_BYTES: usize = 64 * 1024;
const CAPTURE_ARCHIVE_LEASE_SECONDS: i64 = 30;
const CAPTURE_ARCHIVE_STREAM_SECONDS: u64 = 300;
const CAPTURE_ARCHIVE_ADVISORY_KEY: i64 = 1_195_722_091;
const MAX_INSPECT_CAPTURE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_CAPTURE_FLOWS: usize = 20_000;
const MAX_CAPTURE_CHALLENGES: u64 = 500;
const MAX_CAPTURE_PAGE: usize = 100;
const MAX_CAPTURE_SCAN_ENTRIES: usize = 4_096;
static CAPTURE_LISTING_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturePageQuery {
    #[serde(default)]
    skip: usize,
    #[serde(default = "default_capture_page")]
    count: usize,
}

const fn default_capture_page() -> usize {
    MAX_CAPTURE_PAGE
}

impl CapturePageQuery {
    fn normalized(self) -> AppResult<(usize, usize)> {
        if self.count == 0 || self.count > MAX_CAPTURE_PAGE || self.skip > MAX_CAPTURE_SCAN_ENTRIES
        {
            return Err(AppError::bad_request(
                "Capture pages require count 1-100 and skip at most 4096",
            ));
        }
        Ok((self.skip, self.count))
    }
}

#[derive(Debug, sqlx::FromRow)]
struct CaptureTeamRow {
    participation_id: i32,
    team_id: i32,
    name: String,
    avatar_hash: Option<String>,
}
const MAX_FLOW_FILTER_BYTES: usize = 128;
const MAX_PEER_FILTER_BYTES: usize = 64;
const DEFAULT_FLOW_PAGE_SIZE: usize = 100;
const MAX_FLOW_PAGE_SIZE: usize = 200;
static CAPTURE_ARCHIVE_SLOTS: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(2)));

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

fn scan_capture_archive(dir: &std::path::Path) -> AppResult<Vec<CaptureArchiveSource>> {
    let files = list_pcaps(dir)?;
    if files.is_empty() {
        return Err(AppError::not_found("No captures for this participation"));
    }
    if files.len() > MAX_CAPTURE_ARCHIVE_FILES {
        return Err(AppError::bad_request(
            "Too many captures to archive; download them individually",
        ));
    }
    let declared_total = files.iter().try_fold(0u64, |total, entry| {
        entry
            .metadata()
            .ok()
            .and_then(|metadata| total.checked_add(metadata.len()))
    });
    if declared_total.is_none_or(|total| total > MAX_CAPTURE_ARCHIVE_BYTES) {
        return Err(AppError::bad_request(
            "Captures are too large to archive; download them individually",
        ));
    }
    Ok(files
        .into_iter()
        .map(|entry| CaptureArchiveSource {
            path: entry.path(),
            entry: entry.file_name().to_string_lossy().to_string(),
        })
        .collect())
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
        return Err(AppError::unavailable(
            "Capture archive admission is busy; retry shortly",
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
        return Err(AppError::unavailable(
            "Capture archive capacity is busy; retry shortly",
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

fn is_regular_pcap(entry: &std::fs::DirEntry) -> bool {
    entry.file_type().is_ok_and(|kind| kind.is_file())
        && entry
            .path()
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pcap"))
}

/// Read a strictly bounded directory page and sort only that bounded set.
fn list_pcaps(dir: &std::path::Path) -> AppResult<Vec<std::fs::DirEntry>> {
    let mut v = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if !is_regular_pcap(&entry) {
            continue;
        }
        if v.len() >= MAX_CAPTURE_SCAN_ENTRIES {
            return Err(AppError::unavailable(
                "Capture inventory is too large; wait for retention cleanup",
            ));
        }
        v.push(entry);
    }
    v.sort_by_key(|e| std::cmp::Reverse(e.metadata().ok().and_then(|m| m.modified().ok())));
    Ok(v)
}

/// Count without materializing metadata or sorting the directory. The shared
/// budget fences nested challenge/participation scans to a fixed amount of I/O.
fn count_pcaps(dir: &std::path::Path, budget: &mut usize) -> AppResult<usize> {
    let mut count = 0usize;
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        *budget = budget.checked_sub(1).ok_or_else(|| {
            AppError::unavailable("Capture inventory is too large; retry after retention cleanup")
        })?;
        if is_regular_pcap(&entry) {
            count += 1;
        }
    }
    Ok(count)
}

/// `GET /api/game/games/{id}/captures` — each challenge + its total pcap count.
pub async fn game_captures(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<Vec<Json>>> {
    let _permit = CAPTURE_LISTING_SLOTS
        .try_acquire()
        .map_err(|_| AppError::unavailable("Capture listing capacity is busy; retry shortly"))?;
    let challenges = game_challenge::Entity::find()
        .filter(game_challenge::Column::GameId.eq(id))
        .limit(MAX_CAPTURE_CHALLENGES)
        .all(&st.db)
        .await?;
    let root = capture_root(&st);
    let out = tokio::task::spawn_blocking(move || -> AppResult<Vec<Json>> {
        let mut budget = MAX_CAPTURE_SCAN_ENTRIES;
        challenges
            .into_iter()
            .map(|c| {
                let cdir = root.join(c.id.to_string());
                let mut count = 0usize;
                for entry in std::fs::read_dir(&cdir).into_iter().flatten().flatten() {
                    budget = budget.checked_sub(1).ok_or_else(|| {
                        AppError::unavailable(
                            "Capture inventory is too large; retry after retention cleanup",
                        )
                    })?;
                    if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                        count = count.saturating_add(count_pcaps(&entry.path(), &mut budget)?);
                    }
                }
                Ok(serde_json::json!({
                    "id": c.id, "title": c.title, "category": c.category,
                    "type": c.challenge_type, "isEnabled": c.is_enabled, "count": count,
                }))
            })
            .collect()
    })
    .await
    .map_err(|error| AppError::internal(format!("capture listing task failed: {error}")))??;
    Ok(RequestResponse::ok(out))
}

/// `GET /api/game/captures/{challengeId}` — one row per participation with pcaps.
pub async fn team_traffic(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path(cid): Path<i32>,
    Query(page): Query<CapturePageQuery>,
) -> AppResult<RequestResponse<Vec<Json>>> {
    let (skip, count) = page.normalized()?;
    let _permit = CAPTURE_LISTING_SLOTS
        .try_acquire()
        .map_err(|_| AppError::unavailable("Capture listing capacity is busy; retry shortly"))?;
    let cdir = capture_root(&st).join(cid.to_string());
    let captures = tokio::task::spawn_blocking(move || -> AppResult<Vec<(i32, usize)>> {
        let mut budget = MAX_CAPTURE_SCAN_ENTRIES;
        let mut captures = Vec::new();
        for entry in std::fs::read_dir(&cdir).into_iter().flatten().flatten() {
            budget = budget.checked_sub(1).ok_or_else(|| {
                AppError::unavailable(
                    "Capture inventory is too large; retry after retention cleanup",
                )
            })?;
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|value| value.parse::<i32>().ok())
            else {
                continue;
            };
            let file_count = count_pcaps(&entry.path(), &mut budget)?;
            if file_count > 0 {
                captures.push((pid, file_count));
            }
        }
        captures.sort_unstable_by_key(|(pid, _)| *pid);
        Ok(captures.into_iter().skip(skip).take(count).collect())
    })
    .await
    .map_err(|error| AppError::internal(format!("capture listing task failed: {error}")))??;
    let participation_ids: Vec<i32> = captures.iter().map(|(pid, _)| *pid).collect();
    let teams = sqlx::query_as::<_, CaptureTeamRow>(
        r#"SELECT p.id AS participation_id,
                  p.team_id,
                  t.name,
                  t.avatar_hash
             FROM "Participations" p
             JOIN "Teams" t ON t.id = p.team_id
            WHERE p.id = ANY($1)"#,
    )
    .bind(&participation_ids)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let teams: std::collections::HashMap<_, _> = teams
        .into_iter()
        .map(|row| (row.participation_id, row))
        .collect();
    let mut out = Vec::with_capacity(captures.len());
    for (pid, capture_count) in captures {
        let Some(team) = teams.get(&pid) else {
            continue;
        };
        let avatar = team
            .avatar_hash
            .as_ref()
            .map(|hash| format!("/assets/{hash}/avatar"));
        out.push(serde_json::json!({
            "id": pid, "teamId": team.team_id, "name": team.name,
            "division": Json::Null, "avatar": avatar, "count": capture_count,
        }));
    }
    Ok(RequestResponse::ok(out))
}

/// `GET /api/game/captures/{challengeId}/{partId}` — the pcap files (FileRecord).
pub async fn traffic_files(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path((cid, pid)): Path<(i32, i32)>,
    Query(page): Query<CapturePageQuery>,
) -> AppResult<RequestResponse<Vec<Json>>> {
    let (skip, count) = page.normalized()?;
    let _permit = CAPTURE_LISTING_SLOTS
        .try_acquire()
        .map_err(|_| AppError::unavailable("Capture listing capacity is busy; retry shortly"))?;
    let dir = capture_root(&st)
        .join(cid.to_string())
        .join(pid.to_string());
    let out = tokio::task::spawn_blocking(move || -> AppResult<Vec<Json>> {
        let rows = list_pcaps(&dir)?
            .into_iter()
            .skip(skip)
            .take(count)
            .map(|e| {
                let meta = e.metadata().ok();
                let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                let update = meta
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                serde_json::json!({
                    "fileName": e.file_name().to_string_lossy(),
                    "size": size,
                    "updateTime": update,
                })
            })
            .collect::<Vec<_>>();
        Ok(rows)
    })
    .await
    .map_err(|error| AppError::internal(format!("capture listing task failed: {error}")))??;
    Ok(RequestResponse::ok(out))
}

/// `GET /api/game/captures/{challengeId}/{partId}/all` — zip of the pcaps.
pub async fn get_all_traffic(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path((cid, pid)): Path<(i32, i32)>,
) -> AppResult<Response> {
    let dir = capture_root(&st)
        .join(cid.to_string())
        .join(pid.to_string());
    let permit = CAPTURE_ARCHIVE_SLOTS
        .clone()
        .try_acquire_owned()
        .map_err(|_| AppError::unavailable("Capture archive capacity is busy; retry shortly"))?;
    let operation_id = acquire_archive_lease(st.pg(), cid, pid).await?;
    let sources = match tokio::task::spawn_blocking(move || scan_capture_archive(&dir)).await {
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
    if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(AppError::internal(format!(
                "could not delete captures: {error}"
            )));
        }
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
    if let Err(error) = tokio::fs::remove_file(&path).await {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(AppError::internal(format!(
                "could not delete capture: {error}"
            )));
        }
    }
    Ok(StatusCode::OK)
}

/// `GET /api/game/captures/{challengeId}/{partId}/{filename}/flows` — the TCP/UDP
/// flows parsed out of the pcap (`services::traffic::list_flows`).
pub async fn traffic_flows(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path((cid, pid, filename)): Path<(i32, i32, String)>,
    Query(filter): Query<TrafficFlowFilter>,
) -> AppResult<RequestResponse<Vec<TrafficFlowSummary>>> {
    let name = safe_capture_name(&filename)?;
    let path = capture_root(&st)
        .join(cid.to_string())
        .join(pid.to_string())
        .join(name);
    let regex = compile_flow_regex(filter.regex_pattern.as_deref())?;
    let peer = bounded_peer_filter(filter.peer_ip_contains.as_deref())?;
    if filter
        .start_utc
        .zip(filter.end_utc)
        .is_some_and(|(start, end)| start > end)
    {
        return Err(AppError::bad_request("Flow time range is reversed"));
    }
    let page = filter.page.unwrap_or(1).max(1) as usize;
    let page_size = filter
        .page_size
        .map_or(DEFAULT_FLOW_PAGE_SIZE, |size| size as usize)
        .clamp(1, MAX_FLOW_PAGE_SIZE);
    let skip = page.saturating_sub(1).saturating_mul(page_size);
    let flows = crate::services::traffic::inspect_flows_cached(
        path,
        MAX_INSPECT_CAPTURE_BYTES,
        MAX_CAPTURE_FLOWS,
    )
    .await?;
    let out = flows
        .iter()
        .filter(|flow| {
            peer.as_ref()
                .is_none_or(|peer| flow.peer_ip.to_string().to_ascii_lowercase().contains(peer))
                && regex
                    .as_ref()
                    .is_none_or(|regex| flow.retained_payload_matches(regex))
                && filter
                    .start_utc
                    .is_none_or(|start| flow.last_seen_millis >= start)
                && filter
                    .end_utc
                    .is_none_or(|end| flow.first_seen_millis <= end)
                && filter.direction.is_none_or(|direction| match direction {
                    TrafficFlowDirection::ContainerToTeam => flow.packets_in > 0,
                    TrafficFlowDirection::TeamToContainer => flow.packets_out > 0,
                })
                && (!filter.flags_only.unwrap_or(false) || flow.flag_hits > 0)
        })
        .skip(skip)
        .take(page_size)
        .map(flow_summary)
        .collect();
    Ok(RequestResponse::ok(out))
}

fn compile_flow_regex(pattern: Option<&str>) -> AppResult<Option<regex::bytes::Regex>> {
    let Some(pattern) = pattern.map(str::trim).filter(|pattern| !pattern.is_empty()) else {
        return Ok(None);
    };
    if pattern.len() > MAX_FLOW_FILTER_BYTES {
        return Err(AppError::bad_request("Payload regex is too long"));
    }
    regex::bytes::RegexBuilder::new(pattern)
        .size_limit(1024 * 1024)
        .dfa_size_limit(1024 * 1024)
        .build()
        .map(Some)
        .map_err(|_| AppError::bad_request("Payload regex is invalid or too complex"))
}

fn bounded_peer_filter(peer: Option<&str>) -> AppResult<Option<String>> {
    let peer = peer.map(str::trim).filter(|peer| !peer.is_empty());
    if peer.is_some_and(|peer| peer.len() > MAX_PEER_FILTER_BYTES) {
        return Err(AppError::bad_request("Peer IP filter is too long"));
    }
    Ok(peer.map(str::to_ascii_lowercase))
}

fn bounded_i64(value: u64) -> i64 {
    value.try_into().unwrap_or(i64::MAX)
}

fn flow_summary(flow: &crate::services::traffic::InspectedFlow) -> TrafficFlowSummary {
    TrafficFlowSummary {
        connection_port: i32::from(flow.connection_port),
        first_seen_utc: flow.first_seen_millis,
        last_seen_utc: flow.last_seen_millis,
        peer_ip: flow.peer_ip.to_string(),
        packets_in: bounded_i64(flow.packets_in),
        packets_out: bounded_i64(flow.packets_out),
        bytes_in: bounded_i64(flow.bytes_in),
        bytes_out: bounded_i64(flow.bytes_out),
        flag_hits: bounded_i64(flow.flag_hits),
    }
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
    if !(0..=i32::from(u16::MAX)).contains(&connection_port) {
        return Err(AppError::bad_request("Invalid flow connection port"));
    }
    let flows = crate::services::traffic::inspect_flows_cached(
        path,
        MAX_INSPECT_CAPTURE_BYTES,
        MAX_CAPTURE_FLOWS,
    )
    .await?;
    let flow = flows
        .iter()
        .find(|flow| i32::from(flow.connection_port) == connection_port)
        .ok_or_else(|| AppError::not_found("Traffic flow not found"))?;
    Ok(RequestResponse::ok(TrafficFlowDetail {
        summary: flow_summary(flow),
        chunks: flow
            .chunks
            .iter()
            .map(|chunk| TrafficFlowChunk {
                direction: match chunk.direction {
                    crate::services::traffic::InspectedDirection::ContainerToTeam => {
                        TrafficFlowDirection::ContainerToTeam
                    }
                    crate::services::traffic::InspectedDirection::TeamToContainer => {
                        TrafficFlowDirection::TeamToContainer
                    }
                },
                timestamp_utc: chunk.timestamp_millis,
                payload_base64: base64::engine::general_purpose::STANDARD.encode(&chunk.payload),
                flag_offsets: chunk
                    .flag_offsets
                    .iter()
                    .map(|offset| (*offset).try_into().unwrap_or(i64::MAX))
                    .collect(),
            })
            .collect(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn capture_pages_reject_unbounded_windows() {
        assert_eq!(
            CapturePageQuery {
                skip: 4_096,
                count: 100,
            }
            .normalized()
            .unwrap(),
            (4_096, 100)
        );
        assert!(CapturePageQuery { skip: 0, count: 0 }.normalized().is_err());
        assert!(CapturePageQuery {
            skip: 0,
            count: 101
        }
        .normalized()
        .is_err());
        assert!(CapturePageQuery {
            skip: 4_097,
            count: 1
        }
        .normalized()
        .is_err());
    }

    #[test]
    fn filesystem_inventory_uses_one_shared_strict_scan_budget() {
        let dir = std::env::temp_dir().join(format!("rsctf-capture-list-{}", Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        for name in ["one.pcap", "two.PCAP", "three.pcap"] {
            std::fs::File::create(dir.join(name)).unwrap();
        }
        std::fs::File::create(dir.join("ignore.txt")).unwrap();

        let mut enough = 4;
        assert_eq!(count_pcaps(&dir, &mut enough).unwrap(), 3);
        assert_eq!(list_pcaps(&dir).unwrap().len(), 3);
        let mut exhausted = 2;
        assert!(count_pcaps(&dir, &mut exhausted).is_err());

        std::fs::remove_dir_all(&dir).unwrap();
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
}
