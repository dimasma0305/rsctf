//! Traffic-capture serving: pcap listing/download/flows.
use super::*;
use std::io::Read;

// ---------------------------------------------------------------------------
// Traffic capture metadata and pcap serving for the singleton capture worker.
// ---------------------------------------------------------------------------

const MAX_CAPTURE_ARCHIVE_FILES: usize = 256;
const MAX_CAPTURE_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_INSPECT_CAPTURE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_CAPTURE_FLOWS: usize = 20_000;
static CAPTURE_ARCHIVE_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
static CAPTURE_FLOW_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

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

/// The `.pcap` files directly inside `dir` (sorted, newest first by mtime).
fn list_pcaps(dir: &std::path::Path) -> Vec<std::fs::DirEntry> {
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| x.eq_ignore_ascii_case("pcap"))
        })
        .collect();
    v.sort_by_key(|e| std::cmp::Reverse(e.metadata().ok().and_then(|m| m.modified().ok())));
    v
}

/// `GET /api/game/games/{id}/captures` — each challenge + its total pcap count.
pub async fn game_captures(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<Vec<Json>>> {
    let challenges = game_challenge::Entity::find()
        .filter(game_challenge::Column::GameId.eq(id))
        .all(&st.db)
        .await?;
    let root = capture_root(&st);
    let out = tokio::task::spawn_blocking(move || {
        challenges
            .into_iter()
            .map(|c| {
                let cdir = root.join(c.id.to_string());
                let count: usize = std::fs::read_dir(&cdir)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter(|e| e.path().is_dir())
                    .map(|e| list_pcaps(&e.path()).len())
                    .sum();
                serde_json::json!({
                    "id": c.id, "title": c.title, "category": c.category,
                    "type": c.challenge_type, "isEnabled": c.is_enabled, "count": count,
                })
            })
            .collect()
    })
    .await
    .map_err(|error| AppError::internal(format!("capture listing task failed: {error}")))?;
    Ok(RequestResponse::ok(out))
}

/// `GET /api/game/captures/{challengeId}` — one row per participation with pcaps.
pub async fn team_traffic(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path(cid): Path<i32>,
) -> AppResult<RequestResponse<Vec<Json>>> {
    let cdir = capture_root(&st).join(cid.to_string());
    let captures = tokio::task::spawn_blocking(move || {
        std::fs::read_dir(&cdir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| {
                let pid = entry.file_name().to_str()?.parse::<i32>().ok()?;
                Some((pid, list_pcaps(&entry.path()).len()))
            })
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|error| AppError::internal(format!("capture listing task failed: {error}")))?;
    let mut out = Vec::new();
    for (pid, count) in captures {
        // Resolve the team behind the participation for display.
        let (team_id, name, avatar) =
            match participation::Entity::find_by_id(pid).one(&st.db).await? {
                Some(p) => match team::Entity::find_by_id(p.team_id).one(&st.db).await? {
                    Some(t) => (p.team_id, t.name.clone(), t.avatar_url()),
                    None => (p.team_id, String::new(), None),
                },
                None => (0, String::new(), None),
            };
        out.push(serde_json::json!({
            "id": pid, "teamId": team_id, "name": name,
            "division": Json::Null, "avatar": avatar, "count": count,
        }));
    }
    Ok(RequestResponse::ok(out))
}

/// `GET /api/game/captures/{challengeId}/{partId}` — the pcap files (FileRecord).
pub async fn traffic_files(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path((cid, pid)): Path<(i32, i32)>,
) -> AppResult<RequestResponse<Vec<Json>>> {
    let dir = capture_root(&st)
        .join(cid.to_string())
        .join(pid.to_string());
    let out = tokio::task::spawn_blocking(move || {
        list_pcaps(&dir)
            .into_iter()
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
            .collect()
    })
    .await
    .map_err(|error| AppError::internal(format!("capture listing task failed: {error}")))?;
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
    let _permit = CAPTURE_ARCHIVE_SLOTS
        .try_acquire()
        .map_err(|_| AppError::unavailable("Capture archive capacity is busy; retry shortly"))?;
    let buf = tokio::task::spawn_blocking(move || -> AppResult<Vec<u8>> {
        let files = list_pcaps(&dir);
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

        let mut buf = Vec::new();
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions<()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        let mut written = 0u64;
        for e in files {
            let name = e.file_name().to_string_lossy().to_string();
            zip.start_file(name, opts)
                .map_err(|err| AppError::internal(format!("zip: {err}")))?;
            let file = std::fs::File::open(e.path())
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
) -> AppResult<RequestResponse<Vec<Json>>> {
    let name = safe_capture_name(&filename)?;
    let path = capture_root(&st)
        .join(cid.to_string())
        .join(pid.to_string())
        .join(name);
    let _permit = CAPTURE_FLOW_SLOTS
        .try_acquire()
        .map_err(|_| AppError::unavailable("Capture inspection capacity is busy; retry shortly"))?;
    let flows = tokio::task::spawn_blocking(move || {
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
    let _permit = CAPTURE_FLOW_SLOTS
        .try_acquire()
        .map_err(|_| AppError::unavailable("Capture inspection capacity is busy; retry shortly"))?;
    let flows = tokio::task::spawn_blocking(move || {
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
