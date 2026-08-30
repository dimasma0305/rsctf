//! Traffic-capture serving: pcap listing/download/flows.
use super::*;
use std::io::Read;

// ---------------------------------------------------------------------------
// Traffic capture metadata and pcap serving for the singleton capture worker.
// ---------------------------------------------------------------------------

const MAX_CAPTURE_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
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
