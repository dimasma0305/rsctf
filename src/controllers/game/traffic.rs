//! Traffic-capture serving: pcap listing/download/flows.
use super::*;
use base64::Engine as _;
use std::io::Read;

// ---------------------------------------------------------------------------
// Traffic capture metadata and pcap serving for the singleton capture worker.
// ---------------------------------------------------------------------------

const MAX_CAPTURE_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
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

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub enum TrafficFlowDirection {
    ContainerToTeam,
    TeamToContainer,
}

impl From<TrafficFlowDirection> for crate::services::traffic::FlowDirection {
    fn from(direction: TrafficFlowDirection) -> Self {
        match direction {
            TrafficFlowDirection::ContainerToTeam => Self::ContainerToTeam,
            TrafficFlowDirection::TeamToContainer => Self::TeamToContainer,
        }
    }
}

impl From<crate::services::traffic::FlowDirection> for TrafficFlowDirection {
    fn from(direction: crate::services::traffic::FlowDirection) -> Self {
        match direction {
            crate::services::traffic::FlowDirection::ContainerToTeam => Self::ContainerToTeam,
            crate::services::traffic::FlowDirection::TeamToContainer => Self::TeamToContainer,
        }
    }
}

fn default_flow_page() -> u32 {
    1
}

fn default_flow_page_size() -> u16 {
    crate::services::traffic::DEFAULT_FLOW_PAGE_SIZE
}

/// Bounded filters and pagination for one immutable capture snapshot.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrafficFlowQuery {
    #[serde(default)]
    regex_pattern: Option<String>,
    #[serde(default)]
    peer_ip_contains: Option<String>,
    #[serde(default)]
    start_utc: Option<i64>,
    #[serde(default)]
    end_utc: Option<i64>,
    #[serde(default)]
    direction: Option<TrafficFlowDirection>,
    #[serde(default)]
    flags_only: bool,
    #[serde(default = "default_flow_page")]
    page: u32,
    #[serde(default = "default_flow_page_size")]
    page_size: u16,
}

impl TrafficFlowQuery {
    fn validated_filter(&self) -> Result<crate::services::traffic::ValidatedFlowFilter, AppError> {
        crate::services::traffic::ValidatedFlowFilter::new(
            self.regex_pattern.as_deref(),
            self.peer_ip_contains.as_deref(),
            self.start_utc,
            self.end_utc,
            self.direction.map(Into::into),
            self.flags_only,
        )
        .map_err(Into::into)
    }
}

async fn configured_capture_port(st: &SharedState, challenge_id: i32) -> AppResult<u16> {
    let port = sqlx::query_scalar::<_, Option<i32>>(
        r#"SELECT expose_port FROM "GameChallenges"
            WHERE id = $1 AND enable_traffic_capture = TRUE"#,
    )
    .bind(challenge_id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .flatten()
    .ok_or_else(|| AppError::not_found("Capture challenge service port not found"))?;
    u16::try_from(port)
        .ok()
        .filter(|port| *port > 0)
        .ok_or_else(|| AppError::bad_request("Capture challenge has an invalid service port"))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrafficFlowDetailQuery {
    #[serde(default)]
    snapshot_version: Option<String>,
    #[serde(default)]
    flow_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrafficFlowSummaryModel {
    pub flow_id: String,
    pub connection_port: u16,
    pub first_seen_utc: i64,
    pub last_seen_utc: i64,
    pub peer_ip: String,
    pub packets_in: u64,
    pub packets_out: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub flag_hits: u32,
    pub payload_truncated: bool,
}

impl From<&crate::services::traffic::IndexedFlow> for TrafficFlowSummaryModel {
    fn from(flow: &crate::services::traffic::IndexedFlow) -> Self {
        Self {
            flow_id: flow.flow_id.clone(),
            connection_port: flow.connection_port,
            first_seen_utc: flow.first_seen_utc,
            last_seen_utc: flow.last_seen_utc,
            peer_ip: flow.peer_ip.clone(),
            packets_in: flow.packets_in,
            packets_out: flow.packets_out,
            bytes_in: flow.bytes_in,
            bytes_out: flow.bytes_out,
            flag_hits: flow.flag_hits,
            payload_truncated: flow.payload_truncated,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrafficFlowPageModel {
    pub items: Vec<TrafficFlowSummaryModel>,
    pub page: u32,
    pub page_size: u16,
    pub total_items: usize,
    pub total_pages: usize,
    pub snapshot_version: String,
    pub indexed_payload_bytes: usize,
    pub payload_truncated: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrafficFlowChunkModel {
    pub direction: TrafficFlowDirection,
    pub timestamp_utc: i64,
    pub payload_base64: String,
    pub flag_offsets: Vec<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrafficFlowDetailModel {
    #[serde(flatten)]
    pub summary: TrafficFlowSummaryModel,
    pub snapshot_version: String,
    pub chunks: Vec<TrafficFlowChunkModel>,
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
    let permit = CAPTURE_ARCHIVE_SLOTS
        .clone()
        .try_acquire_owned()
        .map_err(|_| AppError::unavailable("Capture archive capacity is busy; retry shortly"))?;
    let names =
        crate::services::traffic::inventory::archive_file_names(st.pg(), &root, cid, pid).await?;
    let dir = root.join(cid.to_string()).join(pid.to_string());
    let buf = spawn_blocking_with_permit(permit, move || -> AppResult<Vec<u8>> {
        if names.is_empty() {
            return Err(AppError::not_found("No captures for this participation"));
        }
        let files = names
            .into_iter()
            .map(|name| {
                let path = dir.join(&name);
                let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
                    AppError::internal(format!("capture metadata {}: {error}", path.display()))
                })?;
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(AppError::internal(format!(
                        "capture archive entry is not a regular file: {}",
                        path.display()
                    )));
                }
                Ok((name, path, metadata.len()))
            })
            .collect::<AppResult<Vec<_>>>()?;
        let declared_total = files
            .iter()
            .try_fold(0u64, |total, (_, _, size)| total.checked_add(*size));
        if declared_total.is_none_or(|total| total > MAX_CAPTURE_ARCHIVE_BYTES) {
            return Err(AppError::bad_request(
                "Captures are too large to archive; download them individually",
            ));
        }

        let mut buf = Vec::new();
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions<()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        let mut written = 0u64;
        for (name, path, _) in files {
            zip.start_file(name, opts)
                .map_err(|err| AppError::internal(format!("zip: {err}")))?;
            let file = std::fs::File::open(path)
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
        zip.finish()
            .map_err(|err| AppError::internal(format!("zip: {err}")))?;
        Ok(buf)
    })
    .await
    .map_err(|error| AppError::internal(format!("capture archive task failed: {error}")))??;
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"captures_{cid}_{pid}.zip\""),
            ),
        ],
        buf,
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
    crate::services::traffic::invalidate_inspection_directory(&dir);
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
    crate::services::traffic::invalidate_inspection_path(&path);
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

/// `GET /api/game/captures/{challengeId}/{partId}/{filename}/flows` — a bounded,
/// filterable page from the immutable PCAP flow snapshot.
pub async fn traffic_flows(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path((cid, pid, filename)): Path<(i32, i32, String)>,
    Query(query): Query<TrafficFlowQuery>,
) -> AppResult<RequestResponse<TrafficFlowPageModel>> {
    let name = safe_capture_name(&filename)?;
    let path = capture_root(&st)
        .join(cid.to_string())
        .join(pid.to_string())
        .join(name);
    let filter = query.validated_filter()?;
    crate::services::traffic::validate_flow_page_bounds(query.page, query.page_size)
        .map_err(AppError::from)?;
    let container_port = configured_capture_port(&st, cid).await?;
    let snapshot = crate::services::traffic::load_flow_snapshot(&path, container_port, None)
        .await
        .map_err(AppError::from)?;
    let filter_permit = FLOW_FILTER_CAPACITY
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            AppError::overloaded("Traffic flow filter capacity is busy; retry shortly", 1)
        })?;
    let filter_snapshot = std::sync::Arc::clone(&snapshot);
    let page = spawn_blocking_with_permit(filter_permit, move || {
        crate::services::traffic::filter_flow_page(
            &filter_snapshot,
            &filter,
            query.page,
            query.page_size,
        )
    })
    .await
    .map_err(|error| AppError::internal(format!("traffic flow filter task failed: {error}")))?
    .map_err(AppError::from)?;
    let items = page
        .indices
        .into_iter()
        .map(|index| TrafficFlowSummaryModel::from(&snapshot.flows()[index]))
        .collect();
    let page_size = usize::from(query.page_size);
    Ok(RequestResponse::ok(TrafficFlowPageModel {
        items,
        page: query.page,
        page_size: query.page_size,
        total_items: page.total_items,
        total_pages: page.total_items.div_ceil(page_size),
        snapshot_version: snapshot.version().to_owned(),
        indexed_payload_bytes: snapshot.indexed_payload_bytes(),
        payload_truncated: snapshot.payload_truncated(),
    }))
}

/// `GET /api/game/captures/{challengeId}/{partId}/{filename}/flow/{connectionPort}`
/// — the flow whose src or dst uses `connectionPort`.
pub async fn traffic_flow_detail(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path((cid, pid, filename, connection_port)): Path<(i32, i32, String, i32)>,
    Query(query): Query<TrafficFlowDetailQuery>,
) -> AppResult<RequestResponse<TrafficFlowDetailModel>> {
    let name = safe_capture_name(&filename)?;
    let path = capture_root(&st)
        .join(cid.to_string())
        .join(pid.to_string())
        .join(name);
    let container_port = configured_capture_port(&st, cid).await?;
    let connection_port = u16::try_from(connection_port)
        .ok()
        .filter(|port| *port > 0)
        .ok_or_else(|| AppError::bad_request("connectionPort must be between 1 and 65535"))?;
    if let Some(version) = query.snapshot_version.as_deref() {
        crate::services::traffic::validate_snapshot_version(version).map_err(AppError::from)?;
    }
    if let Some(flow_id) = query.flow_id.as_deref() {
        crate::services::traffic::validate_flow_id(flow_id).map_err(AppError::from)?;
    }
    let snapshot = crate::services::traffic::load_flow_snapshot(
        &path,
        container_port,
        query.snapshot_version.as_deref(),
    )
    .await
    .map_err(AppError::from)?;
    let detail_permit = FLOW_FILTER_CAPACITY
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            AppError::overloaded("Traffic flow detail capacity is busy; retry shortly", 1)
        })?;
    let detail_snapshot = std::sync::Arc::clone(&snapshot);
    let flow_id = query.flow_id;
    let detail = spawn_blocking_with_permit(detail_permit, move || {
        let Some(flow) = detail_snapshot.flow(connection_port, flow_id.as_deref())? else {
            return Ok(None);
        };
        let chunks = flow
            .chunks
            .iter()
            .map(|chunk| TrafficFlowChunkModel {
                direction: chunk.direction.into(),
                timestamp_utc: chunk.timestamp_utc,
                payload_base64: base64::engine::general_purpose::STANDARD.encode(&chunk.payload),
                flag_offsets: chunk.flag_offsets.clone(),
            })
            .collect();
        Ok::<_, crate::services::traffic::InspectionError>(Some(TrafficFlowDetailModel {
            summary: TrafficFlowSummaryModel::from(flow),
            snapshot_version: detail_snapshot.version().to_owned(),
            chunks,
        }))
    })
    .await
    .map_err(|error| AppError::internal(format!("traffic flow detail task failed: {error}")))?
    .map_err(AppError::from)?
    .ok_or_else(|| AppError::not_found("Flow not found in this capture snapshot"))?;
    Ok(RequestResponse::ok(detail))
}

#[cfg(test)]
mod cancellation_tests {
    use super::spawn_blocking_with_permit;
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
}

#[cfg(test)]
mod flow_contract_tests {
    use super::*;

    #[test]
    fn flow_query_is_camel_case_typed_and_rejects_unknown_or_invalid_filters() {
        let query: TrafficFlowQuery = serde_json::from_value(serde_json::json!({
            "regexPattern": "flag\\{",
            "peerIpContains": "10.8:",
            "direction": "TeamToContainer",
            "flagsOnly": true,
            "page": 2,
            "pageSize": 25
        }))
        .unwrap();
        assert_eq!(query.page, 2);
        assert_eq!(query.page_size, 25);
        assert!(query.validated_filter().is_ok());

        assert!(
            serde_json::from_value::<TrafficFlowQuery>(serde_json::json!({
                "payloadRegex": "ignored-contract-field"
            }))
            .is_err()
        );
        let invalid: TrafficFlowQuery = serde_json::from_value(serde_json::json!({
            "regexPattern": "(",
            "page": 1,
            "pageSize": 50
        }))
        .unwrap();
        assert_eq!(
            invalid.validated_filter().unwrap_err().status(),
            StatusCode::BAD_REQUEST
        );

        let detail: TrafficFlowDetailQuery = serde_json::from_value(serde_json::json!({
            "snapshotVersion": "a".repeat(32),
            "flowId": "04ac1400041f90040a080007b043"
        }))
        .unwrap();
        assert!(
            crate::services::traffic::validate_flow_id(detail.flow_id.as_deref().unwrap()).is_ok()
        );
        assert!(
            serde_json::from_value::<TrafficFlowDetailQuery>(serde_json::json!({
                "src": "unsupported"
            }))
            .is_err()
        );
    }

    #[test]
    fn summary_page_and_detail_share_one_numeric_timestamp_contract() {
        let summary = TrafficFlowSummaryModel {
            flow_id: "04ac1400041f90040a080007b043".into(),
            connection_port: 45_123,
            first_seen_utc: 1_001,
            last_seen_utc: 1_030,
            peer_ip: "10.8.0.7".into(),
            packets_in: 1,
            packets_out: 2,
            bytes_in: 17,
            bytes_out: 20,
            flag_hits: 1,
            payload_truncated: false,
        };
        let detail = TrafficFlowDetailModel {
            summary: summary.clone(),
            snapshot_version: "a".repeat(32),
            chunks: vec![TrafficFlowChunkModel {
                direction: TrafficFlowDirection::TeamToContainer,
                timestamp_utc: 1_001,
                payload_base64: "ZmxhZ3thbHBoYX0=".into(),
                flag_offsets: vec![0],
            }],
        };
        let detail = serde_json::to_value(detail).unwrap();
        assert_eq!(detail["flowId"], "04ac1400041f90040a080007b043");
        assert_eq!(detail["connectionPort"], 45_123);
        assert_eq!(detail["firstSeenUtc"], 1_001);
        assert_eq!(detail["chunks"][0]["timestampUtc"], 1_001);
        assert_eq!(detail["chunks"][0]["direction"], "TeamToContainer");
        assert!(detail.get("src").is_none());
        assert!(detail.get("dst").is_none());

        let page = serde_json::to_value(TrafficFlowPageModel {
            items: vec![summary],
            page: 1,
            page_size: 50,
            total_items: 1,
            total_pages: 1,
            snapshot_version: "a".repeat(32),
            indexed_payload_bytes: 17,
            payload_truncated: false,
        })
        .unwrap();
        assert_eq!(page["items"][0]["connectionPort"], 45_123);
        assert_eq!(page["snapshotVersion"], "a".repeat(32));
    }

    #[test]
    fn cached_inspector_work_is_bounded_and_never_runs_on_tokio_workers() {
        assert!(FLOW_FILTER_SLOTS > 0 && FLOW_FILTER_SLOTS <= 4);
        let source = include_str!("traffic.rs");
        assert!(source.contains("FLOW_FILTER_CAPACITY"));
        assert!(source.contains("try_acquire_owned()"));
        assert!(source.contains("spawn_blocking_with_permit(filter_permit"));
        assert!(source.contains("spawn_blocking_with_permit(detail_permit"));
        assert!(source.contains("Traffic flow filter capacity is busy; retry shortly"));
        assert!(source.contains("Traffic flow detail capacity is busy; retry shortly"));
    }
}
