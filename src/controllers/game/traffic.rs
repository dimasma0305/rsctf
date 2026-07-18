//! Traffic-capture serving: pcap listing/download/flows.
use super::*;

// ---------------------------------------------------------------------------
// Traffic capture metadata and pcap serving for the singleton capture worker.
// ---------------------------------------------------------------------------

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
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
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
    let out = challenges
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
        .collect();
    Ok(RequestResponse::ok(out))
}

/// `GET /api/game/captures/{challengeId}` — one row per participation with pcaps.
pub async fn team_traffic(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path(cid): Path<i32>,
) -> AppResult<RequestResponse<Vec<Json>>> {
    let cdir = capture_root(&st).join(cid.to_string());
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&cdir).into_iter().flatten().flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<i32>().ok())
        else {
            continue;
        };
        let count = list_pcaps(&entry.path()).len();
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
    let out = list_pcaps(&dir)
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
        .collect();
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
    let files = list_pcaps(&dir);
    if files.is_empty() {
        return Err(AppError::not_found("No captures for this participation"));
    }
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions<()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for e in files {
            let name = e.file_name().to_string_lossy().to_string();
            if let Ok(bytes) = std::fs::read(e.path()) {
                use std::io::Write;
                zip.start_file(name, opts)
                    .map_err(|err| AppError::internal(format!("zip: {err}")))?;
                zip.write_all(&bytes)
                    .map_err(|err| AppError::internal(format!("zip: {err}")))?;
            }
        }
        zip.finish()
            .map_err(|err| AppError::internal(format!("zip: {err}")))?;
    }
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
    let _ = std::fs::remove_dir_all(&dir);
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
    let bytes = std::fs::read(&path).map_err(|_| AppError::not_found("Capture not found"))?;
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
        ],
        bytes,
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
    let _ = std::fs::remove_file(&path);
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
    let out = crate::services::traffic::list_flows(&path)
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
    let flow = crate::services::traffic::list_flows(&path)
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
