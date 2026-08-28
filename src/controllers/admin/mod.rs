//! Ported from RSCTF `Controllers/AdminController.cs` (+ `Services/Config/ConfigService.cs`).
//!
//! Route prefix `/api/admin`, every endpoint requires `AdminUser`. Paths mirror
//! the documented frontend contract exactly — all lowercase except the `MyIp`
//! diagnostic, which the client requests with capitalised casing.
//!
//! Core and operational endpoints are grouped into focused sibling modules;
//! the router below preserves the existing React client paths and wire models.

pub mod ad;
mod flag_egress;
#[path = "participation.rs"]
mod participation_review;
pub(crate) mod users_manager_autocomplete;

use std::collections::BTreeMap;
use std::io::Write;

use crate::middlewares::rate_limiter::{limited, Policy};
use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use chrono::{DateTime, Duration, Utc};

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::AdminUser;
use crate::models::data::{
    api_token, build_record, config, division, game, game_challenge, game_manager, local_file,
    repo_binding, repo_binding_scan, team, user,
};
use crate::utils::crypto_utils::hash_password_async;
use crate::utils::enums::{
    ChallengeBuildStatus, ChallengeCategory, ParticipationStatus, RepoWatchStatus, ReviewRating,
    Role,
};
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::{ArrayResponse, MessageResponse, RequestResponse};
pub use flag_egress::*;
pub use participation_review::*;
use users_manager_autocomplete::manager_autocomplete;

// ─── DTOs ──────────────────────────────────────────────────────────────────

/// Paginated user/team list query (`?count=&skip=&search=`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListQuery {
    #[serde(default = "default_count")]
    pub count: u64,
    #[serde(default)]
    pub skip: u64,
    #[serde(default)]
    pub search: Option<String>,
}

fn default_count() -> u64 {
    100
}

/// Body of the `users/search` and `teams/search` endpoints.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchModel {
    #[serde(default)]
    pub hint: String,
}

// ─── Router ──────────────────────────────────────────────────────────────────

pub fn router() -> Router<SharedState> {
    Router::new()
        // --- Diagnostics ---
        .route("/api/admin/MyIp", get(my_ip))
        // --- Config ---
        .route(
            "/api/admin/config",
            get(get_config)
                .put(update_config)
                .layer(DefaultBodyLimit::max(96 * 1024)),
        )
        .route(
            "/api/admin/config/logo",
            post(logo_upload)
                .layer(DefaultBodyLimit::max(
                    crate::utils::upload::IMAGE_BODY_BYTES,
                ))
                .merge(delete(logo_delete)),
        )
        .route(
            "/api/admin/config/logo/stage/{operation_id}",
            post(stage_branding).layer(DefaultBodyLimit::max(
                crate::utils::upload::IMAGE_BODY_BYTES,
            )),
        )
        .route(
            "/api/admin/config/operations/{operation_id}",
            get(get_settings_operation),
        )
        // --- Dashboard / trends / reviews / cheat reports / writeups ---
        .route(
            "/api/admin/dashboard",
            limited(Policy::Query, get(dashboard)),
        )
        .route(
            "/api/admin/Games/{id}/FlagEgress",
            limited(Policy::Query, get(get_flag_egress)),
        )
        .route(
            "/api/admin/Games/{id}/FlagEgress/backfill",
            limited(Policy::Query, get(get_flag_egress_backfill)),
        )
        .route(
            "/api/admin/submissiontrend",
            limited(Policy::Query, get(submission_trend)),
        )
        .route("/api/admin/reviews", limited(Policy::Query, get(reviews)))
        .route(
            "/api/admin/cheat-reports",
            limited(Policy::Query, get(cheat_reports)),
        )
        .route(
            "/api/admin/writeups",
            limited(Policy::Query, get(all_writeups)),
        )
        .route(
            "/api/admin/writeups/{id}",
            limited(Policy::Query, get(game_writeups)),
        )
        .route(
            "/api/admin/writeups/{id}/all",
            limited(Policy::Query, get(download_all_writeups)),
        )
        // --- Users ---
        .route("/api/admin/users", get(users).post(add_users))
        .route(
            "/api/admin/users/import",
            post(import_users).layer(DefaultBodyLimit::max(1024 * 1024)),
        )
        .route("/api/admin/users/credentials/send", post(send_credentials))
        .route("/api/admin/users/search", post(search_users))
        .route(
            "/api/admin/users/manager-autocomplete",
            limited(Policy::Query, get(manager_autocomplete)),
        )
        .route(
            "/api/admin/users/{userid}",
            get(user_info).put(update_user).delete(delete_user),
        )
        .route("/api/admin/users/{userid}/password", delete(reset_password))
        // --- Teams ---
        .route("/api/admin/teams", get(teams))
        .route("/api/admin/teams/search", post(search_teams))
        .route(
            "/api/admin/teams/{id}",
            put(update_team).delete(delete_team),
        )
        // --- Participation ---
        .route("/api/admin/participation/{id}", put(update_participation))
        // --- Logs ---
        .route("/api/admin/logs", get(logs))
        // --- Instances ---
        .route("/api/admin/instances", get(instances))
        .route(
            "/api/admin/instances/filter-options",
            get(instance_filter_options),
        )
        .route("/api/admin/instances/{id}", delete(destroy_instance))
        .route("/api/admin/instances/{id}/stats", get(instance_stats))
        // --- Files ---
        .route("/api/admin/files", get(files))
        // --- Diagnostics: captcha / email test ---
        .route(
            "/api/admin/captcha/test",
            limited(Policy::Concurrency, post(test_captcha)),
        )
        .route(
            "/api/admin/email/test",
            limited(Policy::Concurrency, post(test_email)),
        )
        // --- Bulk rebuild ---
        .route("/api/admin/games/{gameId}/bulkrebuild", post(bulk_rebuild))
        // --- Anti-cheat ---
        .route("/api/admin/anticheatblocks", get(list_anti_cheat_blocks))
        .route(
            "/api/admin/anticheatblocks/{id}",
            delete(delete_anti_cheat_block),
        )
        .route(
            "/api/admin/games/{gameId}/anti-cheat/derive",
            post(derive_event_security_findings),
        )
        .route(
            "/api/admin/games/{gameId}/anti-cheat/fusion/{participationId}",
            get(fused_event_security_breakdown),
        )
        .route(
            "/api/admin/games/{gameId}/anti-cheat/findings/{findingId}/review",
            post(review_event_security_finding),
        )
        .route(
            "/api/admin/games/{gameId}/anti-cheat/telemetry/purge",
            post(purge_event_security_telemetry),
        )
        .route(
            "/api/admin/games/{gameId}/vpn-override",
            post(create_event_vpn_override),
        )
        .route(
            "/api/admin/games/{gameId}/vpn-overrides",
            get(list_event_vpn_overrides),
        )
        .route(
            "/api/admin/games/{gameId}/vpn-override/{overrideId}/revoke",
            post(revoke_event_vpn_override),
        )
        // --- Auto-build pipeline ---
        .route("/api/admin/builds", get(list_builds))
        .route("/api/admin/builds/inprogress", get(builds_in_progress))
        .route(
            "/api/admin/builds/images",
            limited(Policy::Query, get(build_images)),
        )
        .route("/api/admin/builds/images", delete(delete_build_image))
        .route("/api/admin/builds/bulkdelete", post(bulk_delete_builds))
        .route("/api/admin/builds/prunefailed", post(prune_failed_builds))
        .route("/api/admin/builds/pruneimages", post(prune_images))
        .route(
            "/api/admin/builds/storage",
            limited(Policy::Query, get(build_storage_status)),
        )
        .route(
            "/api/admin/builds/prunestorage",
            post(cleanup_build_storage),
        )
        .route("/api/admin/builds/{auditId}", delete(delete_build))
        .route(
            "/api/admin/builds/{auditId}/reenqueue",
            post(reenqueue_build),
        )
        // --- Repo bindings ---
        .route(
            "/api/admin/repobindings",
            get(list_repo_bindings).post(create_repo_binding),
        )
        .route(
            "/api/admin/repobindings/{id}",
            put(update_repo_binding).delete(delete_repo_binding),
        )
        .route("/api/admin/repobindings/{id}/scan", post(scan_repo_binding))
        .route(
            "/api/admin/repobindings/{id}/scans",
            get(repo_binding_scans),
        )
        // Admin A&D controller (round advance, service registration) under admin.
        .merge(ad::router())
}

#[cfg(test)]
mod dashboard_route_admission_tests {
    #[test]
    fn expensive_dashboard_activity_reads_require_admin_and_query_admission() {
        let router_source = include_str!("mod.rs");
        let compact_router = router_source
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let handler_sources = [
            router_source,
            include_str!("dashboard.rs"),
            include_str!("anti_cheat.rs"),
        ]
        .join("\n");

        for (path, handler) in [
            ("/api/admin/dashboard", "dashboard"),
            ("/api/admin/submissiontrend", "submission_trend"),
            ("/api/admin/reviews", "reviews"),
            ("/api/admin/cheat-reports", "cheat_reports"),
            ("/api/admin/writeups", "all_writeups"),
            ("/api/admin/writeups/{id}", "game_writeups"),
            ("/api/admin/writeups/{id}/all", "download_all_writeups"),
        ] {
            assert!(
                compact_router.contains(&format!("\"{path}\"")),
                "missing {path}"
            );
            assert!(
                compact_router.contains(&format!("limited(Policy::Query, get({handler}))")),
                "{path} must retain named query-work admission"
            );

            let signature_start = handler_sources
                .find(&format!("pub async fn {handler}"))
                .unwrap_or_else(|| panic!("missing handler {handler}"));
            let signature_end = (signature_start + 320).min(handler_sources.len());
            assert!(
                handler_sources[signature_start..signature_end].contains("AdminUser"),
                "{handler} must retain backend admin authentication"
            );
        }
    }
}

// ─── Container instances ───────────────────────────────────────────────────────

/// `GET /api/admin/files` — paginated uploaded-file listing.
pub async fn files(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Query(q): Query<ListQuery>,
) -> AppResult<ArrayResponse<LocalFileModel>> {
    let count = q.count.clamp(0, 500);
    let total = local_file::Entity::find()
        .filter(local_file::Column::ReferenceCount.gt(0))
        .count(&st.db)
        .await? as i64;
    let rows = local_file::Entity::find()
        .filter(local_file::Column::ReferenceCount.gt(0))
        .order_by_asc(local_file::Column::Id)
        .offset(q.skip)
        .limit(count)
        .all(&st.db)
        .await?;

    let data = rows
        .into_iter()
        .map(|f| LocalFileModel {
            hash: f.hash,
            name: f.name,
        })
        .collect();
    Ok(ArrayResponse::new(data, total))
}

// ─── Writeups ──────────────────────────────────────────────────────────────────

/// RSCTF `WriteupInfoModel` (per-game view).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteupInfoModel {
    pub divisions: BTreeMap<String, String>,
    pub writeups: Vec<WriteupInfo>,
    pub total: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameWriteupQuery {
    #[serde(default = "default_count")]
    pub count: u64,
    #[serde(default)]
    pub skip: u64,
    #[serde(default)]
    pub division_id: Option<i32>,
}

#[derive(sqlx::FromRow)]
struct GameWriteupRow {
    participation_id: i32,
    division_id: Option<i32>,
    game_title: String,
    hash: String,
    file_name: String,
    upload_time_utc: DateTime<Utc>,
    team_id: i32,
    team_name: String,
    team_bio: Option<String>,
    team_avatar_hash: Option<String>,
    team_locked: bool,
    total: i64,
}

/// `GET /api/admin/writeups/{id}` — writeups submitted for a single game.
pub async fn game_writeups(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<i32>,
    Query(q): Query<GameWriteupQuery>,
) -> AppResult<RequestResponse<WriteupInfoModel>> {
    game::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Game not found"))?;

    // All of the game's divisions (id -> name), like RSCTF's GetWriteups.
    let divisions = division::Entity::find()
        .filter(division::Column::GameId.eq(id))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|d| (d.id.to_string(), d.name))
        .collect();

    let count = q.count.clamp(1, 100);
    let rows = sqlx::query_as::<_, GameWriteupRow>(
        r#"SELECT participation.id AS participation_id,
                  participation.division_id,
                  game.title AS game_title,
                  file.hash,
                  file.name AS file_name,
                  file.upload_time_utc,
                  team.id AS team_id,
                  team.name AS team_name,
                  team.bio AS team_bio,
                  team.avatar_hash AS team_avatar_hash,
                  team.locked AS team_locked,
                  COUNT(*) OVER()::bigint AS total
             FROM "Participations" participation
             JOIN "Games" game ON game.id = participation.game_id
             JOIN "Teams" team ON team.id = participation.team_id
             JOIN "Files" file ON file.id = participation.writeup_id
            WHERE participation.game_id = $1
              AND ($4::integer IS NULL OR participation.division_id = $4)
            ORDER BY participation.id
            LIMIT $2 OFFSET $3"#,
    )
    .bind(id)
    .bind(i64::try_from(count).unwrap_or(100))
    .bind(i64::try_from(q.skip).unwrap_or(i64::MAX))
    .bind(q.division_id)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let total = rows.first().map_or(0, |row| row.total);
    let writeups = rows
        .into_iter()
        .map(|row| WriteupInfo {
            id: row.participation_id,
            team: TeamInfoModel {
                id: row.team_id,
                name: row.team_name,
                bio: row.team_bio,
                avatar: row
                    .team_avatar_hash
                    .map(|hash| format!("/assets/{hash}/avatar")),
                locked: row.team_locked,
                members: Vec::new(),
            },
            game_title: row.game_title,
            url: format!("/assets/{}/{}", row.hash, row.file_name),
            upload_time_utc: row.upload_time_utc,
            division_id: row.division_id,
        })
        .collect();

    Ok(RequestResponse::ok(WriteupInfoModel {
        divisions,
        writeups,
        total,
    }))
}

/// `GET /api/admin/writeups/{id}/all` — download every writeup for a game as a
/// single streamed zip archive.
const WRITEUP_ZIP_CHUNK_BYTES: usize = 64 * 1024;
const MAX_WRITEUP_ARCHIVE_ENTRIES: usize = 2_048;
const MAX_WRITEUP_ARCHIVE_BYTES: usize = 128 * 1024 * 1024;

struct WriteupArchiveSource {
    hash: String,
    entry: String,
}

#[derive(sqlx::FromRow)]
struct WriteupArchiveRow {
    participation_id: i32,
    team_name: String,
    hash: String,
    file_name: String,
    file_size: i64,
}

struct WriteupArchiveFile {
    entry: String,
    bytes: Vec<u8>,
}

type WriteupZipChunk = Result<bytes::Bytes, std::io::Error>;

struct ZipStreamWriter {
    output: tokio::sync::mpsc::Sender<WriteupZipChunk>,
    buffered: Vec<u8>,
}

impl ZipStreamWriter {
    fn new(output: tokio::sync::mpsc::Sender<WriteupZipChunk>) -> Self {
        Self {
            output,
            buffered: Vec::with_capacity(WRITEUP_ZIP_CHUNK_BYTES),
        }
    }

    fn send_buffer(&mut self) -> std::io::Result<()> {
        if self.buffered.is_empty() {
            return Ok(());
        }
        let chunk = std::mem::replace(
            &mut self.buffered,
            Vec::with_capacity(WRITEUP_ZIP_CHUNK_BYTES),
        );
        self.output
            .blocking_send(Ok(bytes::Bytes::from(chunk)))
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "client disconnected"))
    }

    fn finish(mut self) -> std::io::Result<()> {
        self.send_buffer()
    }
}

impl Write for ZipStreamWriter {
    fn write(&mut self, mut input: &[u8]) -> std::io::Result<usize> {
        let input_len = input.len();
        while !input.is_empty() {
            let available = WRITEUP_ZIP_CHUNK_BYTES - self.buffered.len();
            let take = available.min(input.len());
            self.buffered.extend_from_slice(&input[..take]);
            input = &input[take..];
            if self.buffered.len() == WRITEUP_ZIP_CHUNK_BYTES {
                self.send_buffer()?;
            }
        }
        Ok(input_len)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.send_buffer()
    }
}

pub async fn download_all_writeups(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<i32>,
) -> AppResult<Response> {
    let permit = match st
        .bulk_export_admission
        .try_acquire(std::sync::Arc::clone(&st.cache), MAX_WRITEUP_ARCHIVE_BYTES)
        .await
    {
        Ok(permit) => std::sync::Arc::new(permit),
        Err(_) => return Ok(crate::services::bulk_export::overload_response()),
    };
    let game = game::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Game not found"))?;

    let rows = sqlx::query_as::<_, WriteupArchiveRow>(
        r#"SELECT participation.id AS participation_id,
                  team.name AS team_name,
                  file.hash,
                  file.name AS file_name,
                  file.file_size
             FROM "Participations" participation
             JOIN "Teams" team ON team.id = participation.team_id
             JOIN "Files" file ON file.id = participation.writeup_id
            WHERE participation.game_id = $1
            ORDER BY participation.id
            LIMIT $2"#,
    )
    .bind(id)
    .bind(i64::try_from(MAX_WRITEUP_ARCHIVE_ENTRIES + 1).unwrap_or(i64::MAX))
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if rows.len() > MAX_WRITEUP_ARCHIVE_ENTRIES {
        return Err(AppError::payload_too_large(format!(
            "Writeup archives are limited to {MAX_WRITEUP_ARCHIVE_ENTRIES} files"
        )));
    }
    let mut total_bytes = 0usize;
    let mut sources = Vec::with_capacity(rows.len());
    for row in rows {
        let file_size = usize::try_from(row.file_size)
            .map_err(|_| AppError::bad_request("Writeup has an invalid stored size"))?;
        if file_size > crate::utils::upload::WRITEUP_FILE_BYTES {
            return Err(AppError::payload_too_large(
                "A writeup exceeds the file limit",
            ));
        }
        total_bytes = total_bytes
            .checked_add(file_size)
            .filter(|total| *total <= MAX_WRITEUP_ARCHIVE_BYTES)
            .ok_or_else(|| AppError::payload_too_large("Writeup archive exceeds 128 MiB"))?;
        sources.push(WriteupArchiveSource {
            hash: row.hash,
            entry: format!(
                "{}-{}-{}",
                row.participation_id,
                sanitize_entry(&row.team_name),
                sanitize_entry(&row.file_name)
            ),
        });
    }

    let (file_sender, mut file_receiver) = tokio::sync::mpsc::channel::<WriteupArchiveFile>(1);
    let (output_sender, output_receiver) = tokio::sync::mpsc::channel::<WriteupZipChunk>(8);

    let error_sender = output_sender.clone();
    let worker_permit = std::sync::Arc::clone(&permit);
    tokio::task::spawn_blocking(move || {
        let _permit = worker_permit;
        let outcome = (|| -> Result<(), String> {
            let writer = ZipStreamWriter::new(output_sender);
            let mut zip = zip::ZipWriter::new_stream(writer);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            while let Some(file) = file_receiver.blocking_recv() {
                zip.start_file(file.entry, options)
                    .map_err(|error| format!("zip entry: {error}"))?;
                zip.write_all(&file.bytes)
                    .map_err(|error| format!("zip write: {error}"))?;
            }
            let writer = zip
                .finish()
                .map_err(|error| format!("zip finish: {error}"))?;
            writer
                .into_inner()
                .finish()
                .map_err(|error| format!("zip stream: {error}"))
        })();
        if let Err(error) = outcome {
            let _ = error_sender.blocking_send(Err(std::io::Error::other(error)));
        }
    });

    let storage = st.storage.clone();
    let loader_permit = std::sync::Arc::clone(&permit);
    tokio::spawn(async move {
        let _permit = loader_permit;
        for source in sources {
            let bytes = match storage
                .load_bounded(&source.hash, crate::utils::upload::WRITEUP_FILE_BYTES)
                .await
            {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        hash = %source.hash,
                        "skipping unavailable writeup in archive"
                    );
                    continue;
                }
            };
            if file_sender
                .send(WriteupArchiveFile {
                    entry: source.entry,
                    bytes,
                })
                .await
                .is_err()
            {
                break;
            }
        }
    });

    let filename = format!(
        "Writeups-{}-{}.zip",
        sanitize_entry(&game.title),
        Utc::now().format("%Y%m%d-%H.%M.%S")
    );
    let disposition = format!("attachment; filename=\"{filename}\"");

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (header::CONTENT_DISPOSITION, disposition),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
        ],
        crate::services::bulk_export::permitted_stream_body(
            tokio_stream::wrappers::ReceiverStream::new(output_receiver),
            permit,
        ),
    )
        .into_response())
}

/// Strip path separators / control characters from a zip entry component so a
/// crafted team or game name can't escape the archive or break the header.
fn sanitize_entry(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | '\n' | '\r' | '"' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect()
}

#[cfg(test)]
mod writeup_archive_tests {
    use super::*;
    use std::io::{Cursor, Read};

    #[test]
    fn writeup_archive_admits_before_any_projection_or_blob_read() {
        let source = include_str!("mod.rs");
        let handler = source.find("pub async fn download_all_writeups(").unwrap();
        let body = &source[handler..];
        let admission = body.find("bulk_export_admission").unwrap();
        let projection = body.find("query_as::<_, WriteupArchiveRow>").unwrap();
        let blob_read = body.find("load_bounded").unwrap();
        assert!(admission < projection);
        assert!(admission < blob_read);
    }

    #[test]
    fn streamed_writeup_zip_is_valid_without_buffering_the_archive() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel::<WriteupZipChunk>(8);
        let worker = std::thread::spawn(move || {
            let writer = ZipStreamWriter::new(sender);
            let mut zip = zip::ZipWriter::new_stream(writer);
            zip.start_file("team-writeup.pdf", zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"%PDF-test").unwrap();
            zip.finish().unwrap().into_inner().finish().unwrap();
        });

        let mut archive_bytes = Vec::new();
        while let Some(chunk) = receiver.blocking_recv() {
            archive_bytes.extend_from_slice(&chunk.unwrap());
        }
        worker.join().unwrap();

        let mut archive = zip::ZipArchive::new(Cursor::new(archive_bytes)).unwrap();
        let mut contents = Vec::new();
        archive
            .by_name("team-writeup.pdf")
            .unwrap()
            .read_to_end(&mut contents)
            .unwrap();
        assert_eq!(contents, b"%PDF-test");
    }

    #[test]
    fn archive_entry_names_cannot_create_paths() {
        assert_eq!(sanitize_entry("../team\\name\r\n"), ".._team_name__");
    }
}

// ─── Auto-build / repo-binding wire models ─────────────────────────────────────

/// RSCTF `BulkRebuildResultModel`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkRebuildResultModel {
    pub enqueued: i32,
    pub skipped: i32,
    pub messages: Vec<String>,
}

/// RSCTF `ChallengeAuditModel`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeAuditModel {
    pub yaml_text: Option<String>,
    pub files: Vec<Value>,
    pub previews: BTreeMap<String, String>,
    pub archive_available: bool,
    pub build_status: Option<String>,
    pub last_build_log: Option<String>,
}

/// Generic `void` success (200, empty envelope).
pub async fn void_ok(_admin: AdminUser) -> MessageResponse {
    MessageResponse::ok("")
}

/// Default `PruneResultModel` success.
pub async fn prune_result(_admin: AdminUser) -> RequestResponse<PruneResultModel> {
    RequestResponse::ok(PruneResultModel {
        removed: 0,
        messages: Vec::new(),
    })
}

/// Default `ChallengeAuditModel` success.
pub async fn challenge_audit(
    _admin: AdminUser,
    Path(_audit_id): Path<i64>,
) -> RequestResponse<ChallengeAuditModel> {
    RequestResponse::ok(ChallengeAuditModel {
        yaml_text: None,
        files: Vec::new(),
        previews: BTreeMap::new(),
        archive_available: false,
        build_status: None,
        last_build_log: None,
    })
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

async fn load_user(st: &SharedState, id: Uuid) -> AppResult<user::Model> {
    user::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("User not found"))
}

/// Generate a random, human-typable reset password.
fn generate_password() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
    let uuid = Uuid::new_v4();
    let bytes = uuid.as_bytes();
    let mut out = String::with_capacity(16);
    for b in bytes.iter().take(16) {
        out.push(ALPHABET[(*b as usize) % ALPHABET.len()] as char);
    }
    out
}

// ─── Submodules ────────────────────────────────────────────────────────────────

mod anti_cheat;
mod builds;
mod dashboard;
mod diagnostics;
mod instances;
mod logs;
mod repo_bindings;
mod settings;
mod teams;
mod users;
mod users_bulk_identity;
mod users_credentials;
mod users_import_results;
mod users_mutate;
pub use anti_cheat::*;
pub use builds::*;
pub use dashboard::*;
pub use diagnostics::*;
pub use instances::*;
pub use logs::*;
pub use repo_bindings::*;
pub use settings::*;
pub use teams::*;
pub use users::*;
pub use users_credentials::*;
pub use users_mutate::*;
