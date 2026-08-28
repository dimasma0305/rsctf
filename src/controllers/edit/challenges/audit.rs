use super::*;
use axum::body::Body;
use axum::http::{HeaderValue, StatusCode};
use std::collections::VecDeque;
use std::sync::{Arc, LazyLock, Mutex};

const MAX_AUDIT_ARCHIVE_ENTRIES: usize = 2_048;
const MAX_AUDIT_TEXT_BYTES: u64 = 64 * 1024;
const MAX_AUDIT_PREVIEW_BYTES: usize = 256 * 1024;
const MAX_AUDIT_FILE_PATH_BYTES: usize = 512 * 1024;
const MAX_AUDIT_CACHE_ENTRIES: usize = 16;
const AUDIT_DOWNLOADS: usize = 4;
const MAX_BUILD_STATUS_ROWS: i64 = 2_048;

static AUDIT_DOWNLOAD_ADMISSION: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(AUDIT_DOWNLOADS)));
static AUDIT_ARCHIVE_FLIGHTS: LazyLock<
    crate::utils::single_flight::SingleFlight<AuditProjectionFill>,
> = LazyLock::new(crate::utils::single_flight::SingleFlight::new);
static AUDIT_PROJECTION_CACHE: LazyLock<Mutex<VecDeque<(String, Arc<JsonValue>)>>> =
    LazyLock::new(|| Mutex::new(VecDeque::new()));

#[derive(Clone, Default)]
enum AuditProjectionFill {
    Ready(Arc<JsonValue>),
    Missing,
    Busy,
    #[default]
    Failed,
}

#[derive(sqlx::FromRow)]
struct AuditChallengeRow {
    title: String,
    original_archive_blob_path: Option<String>,
    build_status: i16,
    last_build_log: Option<String>,
}

#[derive(sqlx::FromRow)]
struct ChallengeBuildStatusRow {
    id: i32,
    original_archive_blob_path: Option<String>,
    build_status: i16,
    last_build_log: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeBuildStatusModel {
    challenge_id: i32,
    build_status: ChallengeBuildStatus,
    last_build_log: Option<String>,
    archive_available: bool,
    archive_version: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeBuildListStatusModel {
    challenge_id: i32,
    build_status: ChallengeBuildStatus,
}

fn decode_build_status(value: i16) -> AppResult<ChallengeBuildStatus> {
    match value {
        0 => Ok(ChallengeBuildStatus::None),
        1 => Ok(ChallengeBuildStatus::Success),
        2 => Ok(ChallengeBuildStatus::Failed),
        3 => Ok(ChallengeBuildStatus::Building),
        4 => Ok(ChallengeBuildStatus::NotApplicable),
        5 => Ok(ChallengeBuildStatus::Queued),
        6 => Ok(ChallengeBuildStatus::MissingDockerfile),
        _ => Err(AppError::internal("challenge has an invalid build status")),
    }
}

fn archive_version(path: Option<&str>) -> Option<String> {
    path.filter(|value| !value.is_empty()).map(sha256_str)
}

impl TryFrom<ChallengeBuildStatusRow> for ChallengeBuildStatusModel {
    type Error = AppError;

    fn try_from(row: ChallengeBuildStatusRow) -> Result<Self, Self::Error> {
        Ok(Self {
            challenge_id: row.id,
            build_status: decode_build_status(row.build_status)?,
            last_build_log: row.last_build_log,
            archive_available: row
                .original_archive_blob_path
                .as_deref()
                .is_some_and(|path| !path.is_empty()),
            archive_version: archive_version(row.original_archive_blob_path.as_deref()),
        })
    }
}

async fn load_challenge_build_status(
    pool: &sqlx::PgPool,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<ChallengeBuildStatusModel> {
    let row = sqlx::query_as::<_, ChallengeBuildStatusRow>(
        r#"SELECT id, original_archive_blob_path, build_status,
                  LEFT(last_build_log, 16384) AS last_build_log
             FROM "GameChallenges"
            WHERE game_id = $1 AND id = $2"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Challenge not found"))?;
    row.try_into()
}

async fn load_audit_challenge(
    pool: &sqlx::PgPool,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<AuditChallengeRow> {
    sqlx::query_as::<_, AuditChallengeRow>(
        r#"SELECT title, original_archive_blob_path, build_status,
                  LEFT(last_build_log, 16384) AS last_build_log
             FROM "GameChallenges"
            WHERE game_id = $1 AND id = $2"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Challenge not found"))
}

async fn load_challenge_build_statuses(
    pool: &sqlx::PgPool,
    game_id: i32,
) -> AppResult<Vec<ChallengeBuildListStatusModel>> {
    let rows = sqlx::query_as::<_, (i32, i16)>(
        r#"SELECT id, build_status
             FROM "GameChallenges"
            WHERE game_id = $1 AND review_status = 0
            ORDER BY id
            LIMIT $2"#,
    )
    .bind(game_id)
    .bind(MAX_BUILD_STATUS_ROWS)
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    rows.into_iter()
        .map(|(challenge_id, build_status)| {
            Ok(ChallengeBuildListStatusModel {
                challenge_id,
                build_status: decode_build_status(build_status)?,
            })
        })
        .collect()
}

fn cached_projection(hash: &str) -> Option<Arc<JsonValue>> {
    AUDIT_PROJECTION_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .find(|(key, _)| key == hash)
        .map(|(_, value)| value.clone())
}

fn cache_projection(hash: String, value: Arc<JsonValue>) {
    let mut cache = AUDIT_PROJECTION_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.retain(|(key, _)| key != &hash);
    cache.push_back((hash, value));
    while cache.len() > MAX_AUDIT_CACHE_ENTRIES {
        cache.pop_front();
    }
}

/// Compact mutable state used by challenge-detail and audit-modal polling. This
/// query deliberately excludes challenge content, flags and archive bytes.
pub async fn get_challenge_build_status(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, c_id)): Path<(i32, i32)>,
) -> AppResult<RequestResponse<ChallengeBuildStatusModel>> {
    manager_or_admin(&st, &user, id).await?;
    Ok(RequestResponse::ok(
        load_challenge_build_status(st.pg(), id, c_id).await?,
    ))
}

/// One bounded status snapshot for the parent challenge list. A hundred live
/// cards consume this one request and never create per-card timers.
pub async fn list_challenge_build_statuses(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<Vec<ChallengeBuildListStatusModel>>> {
    manager_or_admin(&st, &user, id).await?;
    Ok(RequestResponse::ok(
        load_challenge_build_statuses(st.pg(), id).await?,
    ))
}

fn safe_archive_filename(title: &str) -> String {
    let mut slug = title
        .chars()
        .filter_map(|character| {
            if character.is_ascii_alphanumeric() {
                Some(character.to_ascii_lowercase())
            } else if character == '-' || character == '_' || character.is_whitespace() {
                Some('-')
            } else {
                None
            }
        })
        .take(80)
        .collect::<String>();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    let slug = slug.trim_matches('-');
    format!(
        "{}-source.zip",
        if slug.is_empty() { "challenge" } else { slug }
    )
}

/// Authorized, bounded retained-source download. Storage streams directly into
/// the response after its metadata size passes the same 72-MiB archive cap used
/// by imports and inspection; the server never collects the download in memory.
pub async fn download_challenge_audit_archive(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, c_id)): Path<(i32, i32)>,
) -> AppResult<Response> {
    manager_or_admin(&st, &user, id).await?;
    let challenge = load_audit_challenge(st.pg(), id, c_id).await?;
    let hash = challenge
        .original_archive_blob_path
        .as_deref()
        .filter(|hash| !hash.is_empty())
        .ok_or_else(|| AppError::not_found("Challenge source archive not found"))?;
    let size = st.storage.size(hash).await?;
    if size > crate::utils::upload::SOURCE_ARCHIVE_BLOB_BYTES as u64 {
        return Err(AppError::payload_too_large(
            "Challenge source archive exceeds the download limit",
        ));
    }
    let permit = AUDIT_DOWNLOAD_ADMISSION
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            AppError::retryable_unavailable(
                "Archive download capacity is busy; retry shortly",
                crate::services::bulk_export::RETRY_AFTER_SECONDS,
            )
        })?;
    let stream = st.storage.stream_range(hash, 0..size).await?;
    // The body owns this permit through EOF or disconnect, not merely until
    // the handler returns, so concurrent storage streams stay bounded.
    let stream = stream.map(move |chunk| {
        let _permit = &permit;
        chunk
    });
    let filename = safe_archive_filename(&challenge.title);
    let disposition = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
        .map_err(|_| AppError::internal("invalid archive filename"))?;
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/zip")
        .header(header::CONTENT_LENGTH, size.to_string())
        .header(header::CONTENT_DISPOSITION, disposition)
        .header(header::CACHE_CONTROL, "private, no-store")
        .header(header::PRAGMA, "no-cache")
        .body(Body::from_stream(stream))
        .map_err(|error| AppError::internal(error.to_string()))
}

/// `GET /api/edit/games/{id}/challenges/{cId}/auditmeta` — parsed audit metadata
/// (`ChallengeAuditModel`). Mirrors `EditController.GetChallengeAuditMeta`: opens
/// the challenge's persisted `original_archive_blob_path`, extracts it, and returns
/// the raw yaml, the file tree, and previews of reviewer-targeted files.
///
/// `archiveAvailable` is false only when no archive is on file or the blob is
/// missing; a corrupt/unparseable archive still reports `true` (with empty
/// files/previews), matching RSCTF's catch behavior. `buildStatus`/`lastBuildLog`
/// are always carried through so the modal renders its rebuild button + log panel.
pub async fn get_challenge_audit_meta(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, c_id)): Path<(i32, i32)>,
) -> AppResult<RequestResponse<JsonValue>> {
    manager_or_admin(&st, &user, id).await?;
    let challenge = load_audit_challenge(st.pg(), id, c_id).await?;
    let build_status = decode_build_status(challenge.build_status)?;

    // Empty shape (archive absent/missing): valid `ChallengeAuditModel` with the
    // build fields still populated.
    let empty = |available: bool| {
        json!({
            "archiveAvailable": available,
            "files": [],
            "previews": {},
            "yamlText": JsonValue::Null,
            "buildStatus": build_status,
            "lastBuildLog": challenge.last_build_log,
        })
    };

    let Some(hash) = challenge
        .original_archive_blob_path
        .as_deref()
        .filter(|h| !h.is_empty())
    else {
        return Ok(RequestResponse::ok(empty(false)));
    };
    let hash = hash.to_owned();
    let st_for_fill = st.clone();
    let fill_hash = hash.clone();
    let fill = if let Some(value) = cached_projection(&hash) {
        AuditProjectionFill::Ready(value)
    } else {
        AUDIT_ARCHIVE_FLIGHTS
            .run_with_timeout(
                &format!("audit:{hash}"),
                std::time::Duration::from_secs(60),
                move || async move {
                    if let Some(value) = cached_projection(&fill_hash) {
                        return AuditProjectionFill::Ready(value);
                    }
                    let size = match st_for_fill.storage.size(&fill_hash).await {
                        Ok(size)
                            if size <= crate::utils::upload::SOURCE_ARCHIVE_BLOB_BYTES as u64 =>
                        {
                            size
                        }
                        Ok(_) => return AuditProjectionFill::Failed,
                        Err(_) => return AuditProjectionFill::Missing,
                    };
                    // Reserve the archive's worst buffered footprint before loading
                    // any bytes. Several tabs can join this same hash's flight, but
                    // unrelated archives share the local and deployment-wide budget.
                    let permit = match st_for_fill
                        .bulk_export_admission
                        .try_acquire(
                            Arc::clone(&st_for_fill.cache),
                            usize::try_from(size).unwrap_or(usize::MAX),
                        )
                        .await
                    {
                        Ok(permit) => permit,
                        Err(_) => return AuditProjectionFill::Busy,
                    };
                    let bytes = match st_for_fill
                        .storage
                        .load_bounded(&fill_hash, crate::utils::upload::SOURCE_ARCHIVE_BLOB_BYTES)
                        .await
                    {
                        Ok(bytes) => bytes,
                        Err(_) => return AuditProjectionFill::Missing,
                    };
                    let parsed = match tokio::task::spawn_blocking(move || {
                        let _admission = permit;
                        parse_audit_archive(&bytes)
                    })
                    .await
                    {
                        Ok(parsed) => parsed,
                        Err(error) => {
                            tracing::warn!(%error, "archive inspection task failed");
                            return AuditProjectionFill::Failed;
                        }
                    };
                    let parsed = Arc::new(parsed);
                    cache_projection(fill_hash, parsed.clone());
                    AuditProjectionFill::Ready(parsed)
                },
            )
            .await
    };
    let projection = match fill {
        AuditProjectionFill::Ready(model) => model,
        AuditProjectionFill::Missing => return Ok(RequestResponse::ok(empty(false))),
        AuditProjectionFill::Busy => {
            return Err(AppError::retryable_unavailable(
                "Archive inspection capacity is busy; retry shortly",
                crate::services::bulk_export::RETRY_AFTER_SECONDS,
            ))
        }
        AuditProjectionFill::Failed => {
            return Err(AppError::unavailable(
                "Archive inspection failed; retry shortly",
            ))
        }
    };
    // Cache and broadcast one shared immutable projection. Only the response
    // clone below is mutable because it carries the latest build state.
    let mut model = (*projection).clone();
    if let Some(obj) = model.as_object_mut() {
        obj.insert(
            "buildStatus".into(),
            serde_json::to_value(build_status).unwrap_or(JsonValue::Null),
        );
        obj.insert(
            "lastBuildLog".into(),
            serde_json::to_value(&challenge.last_build_log).unwrap_or(JsonValue::Null),
        );
    }
    Ok(RequestResponse::ok(model))
}

/// Parse a challenge source archive into the `ChallengeAuditModel` core
/// (`archiveAvailable`/`files`/`previews`/`yamlText`). Best-effort and infallible:
/// a corrupt archive yields `archiveAvailable: true` with empty files (the blob
/// was on file, so the archive "exists" — it just couldn't be read), mirroring
/// RSCTF `FillAuditModel`. Previews are keyed by relative path so the modal can
/// blue-highlight previewed entries in the file tree.
fn parse_audit_archive(bytes: &[u8]) -> JsonValue {
    // Eligibility/size caps mirror RSCTF FillAuditModel.
    const PREVIEW_TRUNC: usize = 8 * 1024;
    const PREVIEW_KEYWORDS: [&str; 6] =
        ["readme", "writeup", "solution", "solve", "solver", "notes"];

    let mut files: Vec<(String, u64)> = Vec::new();
    let mut yaml_text: Option<String> = None;
    let mut previews = serde_json::Map::new();
    let mut file_path_bytes = 0usize;
    let mut preview_bytes = 0usize;

    if let Ok(mut archive) = zip::ZipArchive::new(Cursor::new(bytes)) {
        if archive.len() > MAX_AUDIT_ARCHIVE_ENTRIES {
            return json!({
                "archiveAvailable": true,
                "files": [],
                "previews": {},
                "yamlText": JsonValue::Null,
            });
        }
        for i in 0..archive.len() {
            let Ok(mut entry) = archive.by_index(i) else {
                continue;
            };
            if entry.is_dir() {
                continue;
            }
            // Audit display is best-effort, but never present a normalized alias
            // as though it were the raw reviewed archive path.
            let Some(name_path) = crate::utils::archive::canonical_zip_entry_path(&entry) else {
                continue;
            };
            let rel = name_path.to_string_lossy().replace('\\', "/");
            let size = entry.size();
            if rel.len() <= 4 * 1024
                && file_path_bytes.saturating_add(rel.len()) <= MAX_AUDIT_FILE_PATH_BYTES
            {
                file_path_bytes += rel.len();
                files.push((rel.clone(), size));
            }

            let file_name = name_path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            let lower = file_name.to_ascii_lowercase();

            // First challenge.yml/.yaml wins; it isn't also emitted as a preview.
            if yaml_text.is_none() && (lower == "challenge.yaml" || lower == "challenge.yml") {
                let mut buf = String::new();
                if size <= MAX_AUDIT_TEXT_BYTES
                    && std::io::Read::take(&mut entry, MAX_AUDIT_TEXT_BYTES + 1)
                        .read_to_string(&mut buf)
                        .is_ok()
                    && buf.len() <= MAX_AUDIT_TEXT_BYTES as usize
                {
                    yaml_text = Some(buf);
                }
                continue;
            }

            if size <= MAX_AUDIT_TEXT_BYTES && PREVIEW_KEYWORDS.iter().any(|k| lower.contains(k)) {
                let mut data = Vec::new();
                if std::io::Read::take(&mut entry, MAX_AUDIT_TEXT_BYTES + 1)
                    .read_to_end(&mut data)
                    .is_ok()
                    && data.len() <= MAX_AUDIT_TEXT_BYTES as usize
                {
                    if let Ok(mut s) = String::from_utf8(data) {
                        if s.len() > PREVIEW_TRUNC {
                            // Truncate on a char boundary to avoid a panic.
                            let mut end = PREVIEW_TRUNC;
                            while end > 0 && !s.is_char_boundary(end) {
                                end -= 1;
                            }
                            s.truncate(end);
                            s.push_str("\n…(truncated)");
                        }
                        let projected_bytes = rel.len().saturating_add(s.len());
                        if preview_bytes.saturating_add(projected_bytes) <= MAX_AUDIT_PREVIEW_BYTES
                        {
                            preview_bytes += projected_bytes;
                            previews.insert(rel, JsonValue::String(s));
                        }
                    }
                }
            }
        }
    }

    files.sort_by_key(|file| file.0.to_ascii_lowercase());
    let files_json: Vec<JsonValue> = files
        .into_iter()
        .map(|(path, size)| json!({ "path": path, "size": size }))
        .collect();

    json!({
        "archiveAvailable": true,
        "files": files_json,
        "previews": JsonValue::Object(previews),
        "yamlText": yaml_text,
    })
}

/// `POST /api/edit/games/{id}/challenges/{cId}/rebuild` — (re)build the
/// challenge's container image. Mirrors `EditController.RebuildChallengeImage`.
///
/// When the challenge carries a persisted build-context selector
/// (`build_context_subdir`), the selected subtree of its immutable source archive
/// is built with `Docker::build_image`; otherwise the referenced
/// `container_image` is pulled with `Docker::create_image`. `build_status`
/// moves `Building -> Success/Failed` accordingly. Degrades to a valid 200 when
/// the daemon is unreachable (the build stays enqueued/`Queued`), never a 5xx.
///
/// Contract preserved: the response is a `ChallengeAuditModel`-shaped object —
/// the original `files`/`previews`/`archiveAvailable` keys plus the build
/// outcome (`buildStatus`/`lastBuildLog`) the UI polls for.
pub async fn rebuild_challenge(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, c_id)): Path<(i32, i32)>,
    headers: axum::http::HeaderMap,
) -> AppResult<(
    axum::http::StatusCode,
    RequestResponse<crate::services::control_jobs::ControlJobModel>,
)> {
    manager_or_admin(&st, &user, id).await?;
    super::reject_pending_mutation(st.pg(), id, c_id).await?;
    let challenge = load_challenge(&st, id, c_id).await?;
    let operation = crate::controllers::edit::control_jobs::operation_id(&headers)?;
    let attempt: i32 = sqlx::query_scalar(
        r#"SELECT COALESCE(MAX(attempt), 0) + 1
             FROM "BuildRecords" WHERE challenge_id = $1"#,
    )
    .bind(c_id)
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let job = crate::controllers::edit::enqueue_challenge_build_job(
        &st, &challenge, "Manual", attempt, operation,
    )
    .await?;
    Ok((axum::http::StatusCode::ACCEPTED, RequestResponse::ok(job)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, data) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn audit_skips_noncanonical_archive_aliases() {
        let model = parse_audit_archive(&archive(&[
            ("checker/../challenge.yml", b"name: forged\n"),
            ("checker\\notes.txt", b"ambiguous"),
            ("checker/./solution.txt", b"normalized"),
            ("checker/notes.txt", b"reviewed"),
        ]));

        let files = model["files"].as_array().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0]["path"], "checker/notes.txt");
        assert!(model["yamlText"].is_null());
        assert_eq!(model["previews"]["checker/notes.txt"], "reviewed");
    }

    #[test]
    fn audit_projection_caps_many_entry_and_preview_output() {
        let many = (0..=MAX_AUDIT_ARCHIVE_ENTRIES)
            .map(|index| (format!("notes-{index}.txt"), b"review".as_slice()))
            .collect::<Vec<_>>();
        let many_refs = many
            .iter()
            .map(|(name, contents)| (name.as_str(), *contents))
            .collect::<Vec<_>>();
        let rejected = parse_audit_archive(&archive(&many_refs));
        assert!(rejected["files"].as_array().unwrap().is_empty());

        let preview = vec![b'x'; MAX_AUDIT_TEXT_BYTES as usize];
        let preview_entries = (0..64)
            .map(|index| (format!("solution-{index}.txt"), preview.as_slice()))
            .collect::<Vec<_>>();
        let preview_refs = preview_entries
            .iter()
            .map(|(name, contents)| (name.as_str(), *contents))
            .collect::<Vec<_>>();
        let bounded = parse_audit_archive(&archive(&preview_refs));
        let response_preview_bytes = bounded["previews"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(path, contents)| path.len() + contents.as_str().unwrap().len())
            .sum::<usize>();
        assert!(response_preview_bytes <= MAX_AUDIT_PREVIEW_BYTES);
    }

    #[test]
    fn archive_admission_and_download_contract_are_bounded_and_case_exact() {
        assert!((1..=8).contains(&AUDIT_DOWNLOADS));
        assert_eq!(
            safe_archive_filename("../My \"Challenge\"\r\n"),
            "my-challenge-source.zip"
        );

        let routes = include_str!("../mod.rs");
        assert!(routes.contains("\"/api/edit/games/{id}/challenges/{cId}/auditarchive\""));
        assert!(!routes.contains("AuditArchive"));
        let handler = include_str!("audit.rs");
        let authorization = handler
            .find("manager_or_admin(&st, &user, id).await?")
            .unwrap();
        let stream = handler.find("stream_range(hash, 0..size)").unwrap();
        assert!(authorization < stream);
        assert!(handler.contains("let _permit = &permit"));
        let size_check = handler.find("storage.size(&fill_hash)").unwrap();
        let admission = handler.find("bulk_export_admission").unwrap();
        let load = handler.find("load_bounded(&fill_hash").unwrap();
        assert!(size_check < admission && admission < load);
        assert!(handler.contains("private, no-store"));
    }

    #[tokio::test]
    async fn concurrent_archive_fills_share_one_parse() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let flights: &'static crate::utils::single_flight::SingleFlight<AuditProjectionFill> =
            Box::leak(Box::new(crate::utils::single_flight::SingleFlight::new()));
        let parses = Arc::new(AtomicUsize::new(0));
        let mut callers = tokio::task::JoinSet::new();
        for _ in 0..32 {
            let parses = parses.clone();
            callers.spawn(async move {
                flights
                    .run("same-immutable-archive", move || async move {
                        parses.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                        AuditProjectionFill::Ready(Arc::new(json!({ "files": [] })))
                    })
                    .await
            });
        }
        while let Some(result) = callers.join_next().await {
            assert!(matches!(result.unwrap(), AuditProjectionFill::Ready(_)));
        }
        assert_eq!(parses.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn compact_status_reads_are_game_scoped_log_capped_and_row_bounded() {
        use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
        use std::str::FromStr;

        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("challenge_build_status_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"CREATE TABLE "GameChallenges" (
                 id INTEGER PRIMARY KEY,
                 game_id INTEGER NOT NULL,
                 title TEXT NOT NULL,
                 original_archive_blob_path TEXT,
                 build_status SMALLINT NOT NULL,
                 last_build_log TEXT,
                 review_status SMALLINT NOT NULL
               )"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(
            r#"INSERT INTO "GameChallenges"
                 (id, game_id, title, original_archive_blob_path, build_status,
                  last_build_log, review_status)
               SELECT item, 7, 'challenge-' || item, 'archive-' || item, 3,
                      repeat('x', 20000), 0
                 FROM generate_series(1, 2100) item;
               INSERT INTO "GameChallenges"
                 (id, game_id, title, build_status, review_status)
               VALUES (9999, 8, 'other-game', 5, 0)"#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let one = load_audit_challenge(&pool, 7, 1).await.unwrap();
        assert_eq!(one.title, "challenge-1");
        assert_eq!(one.last_build_log.as_deref().unwrap().len(), 16_384);
        assert!(matches!(
            load_audit_challenge(&pool, 8, 1).await,
            Err(AppError::NotFound(_))
        ));
        let compact_one = load_challenge_build_status(&pool, 7, 1).await.unwrap();
        assert_eq!(compact_one.challenge_id, 1);
        assert_eq!(compact_one.last_build_log.as_deref().unwrap().len(), 16_384);
        assert!(matches!(
            load_challenge_build_status(&pool, 8, 1).await,
            Err(AppError::NotFound(_))
        ));
        let statuses = load_challenge_build_statuses(&pool, 7).await.unwrap();
        assert_eq!(statuses.len(), MAX_BUILD_STATUS_ROWS as usize);
        assert_eq!(statuses.first().unwrap().challenge_id, 1);
        assert_eq!(
            statuses.last().unwrap().challenge_id,
            MAX_BUILD_STATUS_ROWS as i32
        );
        let compact_json = serde_json::to_value(statuses.first().unwrap()).unwrap();
        assert!(compact_json.get("lastBuildLog").is_none());
        assert!(compact_json.get("archiveVersion").is_none());

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
