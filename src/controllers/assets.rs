//! Ported from RSCTF `Controllers/AssetsController.cs`.
//!
//! File APIs: upload (admin), download-by-hash, and delete (admin). Public brand
//! assets remain anonymous; challenge attachments and team-owned artifacts are
//! authorized against live game participation before their bytes are loaded.

use axum::body::Body;
use axum::extract::{ConnectInfo, Multipart, Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::Utc;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, Set};
use serde::{Deserialize, Serialize};

use bytes::Bytes;
use std::net::SocketAddr;
use std::time::Duration;

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::{AdminUser, CurrentUser, MaybeUser};
use crate::models::data::{
    attachment, config, flag_context, game, game_challenge, game_event, game_instance, local_file,
    participation, team, user, user_participation,
};
use crate::utils::enums::{ChallengeReviewStatus, ParticipationStatus};
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::MessageResponse;

/// Response row for an uploaded blob (mirrors RSCTF `LocalFile`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalFileResult {
    pub hash: String,
    pub name: String,
    pub size: i64,
}

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/assets", post(upload))
        .route("/assets/{hash}/{filename}", get(download))
        .route(
            "/assets/{hash}/s/{token}/{filename}",
            get(download_with_token),
        )
        .route("/api/assets/{hash}", delete(delete_asset))
}

/// `POST /api/assets` (admin) — multipart upload of one or more files.
pub async fn upload(
    State(st): State<SharedState>,
    AdminUser(_user): AdminUser,
    mut multipart: Multipart,
) -> AppResult<Json<Vec<LocalFileResult>>> {
    let mut results = Vec::new();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::bad_request(format!("multipart error: {e}")))?
    {
        // The uploaded filename, before consuming the field body.
        let file_name = field.file_name().map(|s| s.to_string());
        let bytes = field
            .bytes()
            .await
            .map_err(|e| AppError::bad_request(format!("could not read file: {e}")))?;
        if bytes.is_empty() {
            continue;
        }
        let name = file_name.unwrap_or_else(|| "file".to_string());

        let (blob, _) = crate::services::blob_refs::store_and_acquire(
            st.pg(),
            st.storage.as_ref(),
            &name,
            &bytes,
        )
        .await?;

        results.push(LocalFileResult {
            hash: blob.hash,
            name,
            size: blob.size,
        });
    }

    if results.is_empty() {
        return Err(AppError::bad_request("No file provided"));
    }
    Ok(Json(results))
}

/// Port of RSCTF `AssetsController.IsDownloadAllowed` + `ResolveDownloadTargetsByHash`
/// (by-hash path). Resolves every download *target* a blob backs, then authorizes once:
///
/// - A blob backing **no** target (avatars/posters/logos/orphan uploads) is public
///   (RSCTF `Targets.Count == 0 ⇒ allow`).
/// - A **static** challenge attachment (`game_challenge.attachment_id`) has no source
///   team: any monitor/admin or a participant of that challenge's game may download.
/// - A **dynamic** per-instance attachment (`flag_context.attachment_id` on a
///   `game_instance`) and a **writeup** blob (`participation.writeup_id`) carry the
///   owning team: only a monitor/admin, or a caller whose participation in that game
///   is on the owning team, may download — closing the world-readable hole where anyone
///   with the content hash could pull another team's writeup / dynamic attachment.
///
/// The team gate mirrors RSCTF `GetParticipationsByUser` + `IsTargetAuthorized`:
/// participation membership (a `user_participation` row for the game whose `team_id`
/// equals the source team), not bare team roster.
fn asset_bytes_key(hash: &str) -> String {
    format!("assetblob:{hash}")
}
/// Blob bytes are content-hash immutable. Only small blobs are cached; authorization
/// is deliberately resolved live so membership/challenge revocation is immediate.
const ASSET_BYTES_TTL: Duration = Duration::from_secs(600);
const ASSET_CACHE_MAX_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AssetTarget {
    game_id: i32,
    source_team: Option<i32>,
    challenge_id: Option<i32>,
}

enum AssetGate {
    Public,
    Protected(Vec<AssetTarget>),
    Private,
}

async fn is_explicit_public_reference(st: &SharedState, hash: &str) -> AppResult<bool> {
    if user::Entity::find()
        .filter(user::Column::AvatarHash.eq(hash))
        .count(&st.db)
        .await?
        > 0
        || team::Entity::find()
            .filter(team::Column::AvatarHash.eq(hash))
            .count(&st.db)
            .await?
            > 0
        || game::Entity::find()
            .filter(game::Column::PosterHash.eq(hash))
            .count(&st.db)
            .await?
            > 0
        || config::Entity::find()
            .filter(config::Column::ConfigKey.is_in([
                "GlobalConfig:LogoHash".to_string(),
                "GlobalConfig:FaviconHash".to_string(),
            ]))
            .filter(config::Column::Value.eq(hash))
            .count(&st.db)
            .await?
            > 0
    {
        return Ok(true);
    }
    Ok(false)
}

/// Resolve a blob's download gate — the DB-heavy, user-INDEPENDENT half of
/// authorization: the `(game_id, source_team)` targets that gate this hash (empty ⇒
/// public). Cached by [`cached_asset_gate`]; the cheap per-user check stays per-request.
async fn compute_asset_gate(st: &SharedState, hash: &str) -> AppResult<AssetGate> {
    // Avatar/poster/global uploads predate the Files metadata table and are keyed
    // directly from their owning row. Resolve those references before requiring a
    // LocalFile record so public branding never becomes an accidental 403.
    if is_explicit_public_reference(st, hash).await? {
        return Ok(AssetGate::Public);
    }

    let Some(lf) = local_file::Entity::find()
        .filter(local_file::Column::Hash.eq(hash))
        .filter(local_file::Column::ReferenceCount.gt(0))
        .one(&st.db)
        .await?
    else {
        return Ok(AssetGate::Private);
    };

    // Each target is `(game_id, source_team)`: `None` = static (any participant),
    // `Some(team)` = team-owned (only that team's participants).
    let mut targets = Vec::new();

    let att_ids: Vec<i32> = attachment::Entity::find()
        .filter(attachment::Column::LocalFileId.eq(lf.id))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|a| a.id)
        .collect();

    if !att_ids.is_empty() {
        // Static challenge attachments — no source team.
        for c in game_challenge::Entity::find()
            .filter(game_challenge::Column::AttachmentId.is_in(att_ids.clone()))
            .all(&st.db)
            .await?
        {
            targets.push(AssetTarget {
                game_id: c.game_id,
                source_team: None,
                challenge_id: Some(c.id),
            });
        }

        // Dynamic per-instance attachments: flag_context (attachment) -> game_instance
        // (flag_id) -> participation. Gated to the instance's participation team.
        let fc_ids: Vec<i32> = flag_context::Entity::find()
            .filter(flag_context::Column::AttachmentId.is_in(att_ids))
            .all(&st.db)
            .await?
            .into_iter()
            .map(|f| f.id)
            .collect();
        if !fc_ids.is_empty() {
            let instances = game_instance::Entity::find()
                .filter(game_instance::Column::FlagId.is_in(fc_ids))
                .all(&st.db)
                .await?;
            let part_ids: Vec<i32> = instances.iter().map(|i| i.participation_id).collect();
            if !part_ids.is_empty() {
                let parts: std::collections::HashMap<i32, participation::Model> =
                    participation::Entity::find()
                        .filter(participation::Column::Id.is_in(part_ids))
                        .all(&st.db)
                        .await?
                        .into_iter()
                        .map(|part| (part.id, part))
                        .collect();
                for instance in instances {
                    if let Some(part) = parts.get(&instance.participation_id) {
                        targets.push(AssetTarget {
                            game_id: part.game_id,
                            source_team: Some(part.team_id),
                            challenge_id: Some(instance.challenge_id),
                        });
                    }
                }
            }
        }
    }

    // Writeup blobs are referenced directly by `participation.writeup_id` (an FK to
    // `Files.id`), not via an Attachment — gated to the owning participation team.
    for p in participation::Entity::find()
        .filter(participation::Column::WriteupId.eq(lf.id))
        .all(&st.db)
        .await?
    {
        targets.push(AssetTarget {
            game_id: p.game_id,
            source_team: Some(p.team_id),
            challenge_id: None,
        });
    }

    if targets.is_empty() {
        Ok(AssetGate::Private)
    } else {
        Ok(AssetGate::Protected(targets))
    }
}

/// Authorize a download against the (cached) gate: public ⇒ open; otherwise a
/// monitor/admin, or a participant on a team that satisfies one of the targets.
async fn authorize_asset_download(
    st: &SharedState,
    hash: &str,
    user: &Option<CurrentUser>,
) -> AppResult<()> {
    let gate = compute_asset_gate(st, hash).await?;
    if matches!(gate, AssetGate::Public) {
        return Ok(());
    }
    let Some(u) = user else {
        return Err(AppError::Forbidden);
    };
    if u.is_monitor() {
        return Ok(());
    }
    let AssetGate::Protected(targets) = gate else {
        return Err(AppError::Forbidden);
    };
    for target in targets {
        if let Some(challenge_id) = target.challenge_id {
            let playable = game_challenge::Entity::find()
                .filter(game_challenge::Column::Id.eq(challenge_id))
                .filter(game_challenge::Column::GameId.eq(target.game_id))
                .filter(game_challenge::Column::IsEnabled.eq(true))
                .filter(game_challenge::Column::ReviewStatus.eq(ChallengeReviewStatus::Active))
                .count(&st.db)
                .await?
                > 0;
            let visible_game = game::Entity::find_by_id(target.game_id)
                .one(&st.db)
                .await?
                .is_some_and(|game| !game.hidden);
            if !playable || !visible_game {
                continue;
            }
        }

        let Some(link) = user_participation::Entity::find_by_id((u.id, target.game_id))
            .one(&st.db)
            .await?
        else {
            continue;
        };
        let Some(part) = participation::Entity::find_by_id(link.participation_id)
            .one(&st.db)
            .await?
            .filter(|part| {
                part.game_id == target.game_id
                    && part.team_id == link.team_id
                    && part.status == ParticipationStatus::Accepted
            })
        else {
            continue;
        };
        match target.source_team {
            None => return Ok(()), // static attachment — any participant of the game
            Some(team) if part.team_id == team => return Ok(()),
            Some(_) => continue, // participant, but not on the owning team
        }
    }
    Err(AppError::Forbidden)
}

/// Load a blob, serving cached `Bytes` zero-copy on a hit. Content-hash blobs are
/// immutable, so small ones are cached; larger ones stream from disk to bound memory.
async fn load_asset_bytes(st: &SharedState, hash: &str) -> AppResult<Bytes> {
    let key = asset_bytes_key(hash);
    if let Some(b) = st.cache.get(&key).await {
        return Ok(b);
    }
    let bytes = st.storage.load(hash).await?;
    if bytes.len() <= ASSET_CACHE_MAX_BYTES {
        st.cache.set(&key, &bytes, Some(ASSET_BYTES_TTL)).await;
    }
    Ok(Bytes::from(bytes))
}

/// `GET /assets/{hash}/{filename}` — stream a blob back by content hash.
pub async fn download(
    State(st): State<SharedState>,
    MaybeUser(user): MaybeUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((hash, filename)): Path<(String, String)>,
) -> AppResult<Response> {
    serve_asset(&st, &user, &headers, peer, &hash, &filename, None).await
}

/// `GET /assets/{hash}/s/{token}/{filename}` — secure-token variant of the
/// download route. The token is retained for event compatibility; authorization
/// is enforced from the live attachment/team relationship, not token possession.
pub async fn download_with_token(
    State(st): State<SharedState>,
    MaybeUser(user): MaybeUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((hash, token, filename)): Path<(String, String, String)>,
) -> AppResult<Response> {
    // rsctf does not reproduce RSCTF's per-team secure token, so this path applies
    // the same by-hash authorization as the plain route (public assets open;
    // challenge attachments gated to a monitor/participant). The token segment is
    // still carried into the download GameEvent, mirroring RSCTF.
    serve_asset(
        &st,
        &user,
        &headers,
        peer,
        &hash,
        &filename,
        Some(token.as_str()),
    )
    .await
}

/// Shared body for both download routes: authorize, load the blob, emit the
/// download GameEvent, and stream it back.
async fn serve_asset(
    st: &SharedState,
    user: &Option<CurrentUser>,
    headers: &HeaderMap,
    peer: SocketAddr,
    hash: &str,
    filename: &str,
    token: Option<&str>,
) -> AppResult<Response> {
    authorize_asset_download(st, hash, user).await?;

    // Conditional caching (RSCTF `AssetsController`): a content-hash blob is
    // immutable, so an `ETag` of hash[8..16] lets the browser skip re-downloading.
    let etag = format!("\"{}\"", hash.get(8..16).unwrap_or(""));
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == etag))
    {
        return Ok((StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response());
    }

    let bytes = match load_asset_bytes(st, hash).await {
        Ok(bytes) => bytes,
        Err(_) => {
            // RSCTF `AssetsController` audit event (`Assets_FileNotFound`):
            // Warning-level, TaskStatus.NotFound, no acting user.
            let short = hash.get(..8).unwrap_or(hash);
            crate::services::audit::log(
                &st.db,
                "Warning",
                "AssetsController",
                None,
                crate::services::anti_cheat::client_ip(headers, Some(peer.ip())),
                "NotFound",
                format!("Attempting to fetch non-existing file [{short}] {filename}"),
            )
            .await;
            return Err(AppError::not_found("File not found"));
        }
    };

    // Mirror RSCTF `AssetsController`: a successful challenge-attachment download by a
    // participant emits an `EventType::Download` GameEvent so the abnormal-solve checks
    // (`NoDownload` / `FastSolve-Download`) have input. This is anti-cheat input, NOT part
    // of the response — spawn it so the download isn't billed the event write + dedup scan
    // on the request path (best-effort; a logging failure never breaks the download).
    {
        let st = st.clone();
        let hash = hash.to_string();
        let user = user.clone();
        let token = token.map(|t| t.to_string());
        tokio::spawn(async move {
            let _ = log_attachment_download(&st, &hash, &user, token.as_deref()).await;
        });
    }

    let disposition = format!("attachment; filename=\"{}\"", sanitize(filename));
    let response = (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type_for(filename).to_string()),
            (header::CONTENT_DISPOSITION, disposition),
            (header::ETAG, etag),
        ],
        Body::from(bytes),
    )
        .into_response();
    Ok(response)
}

/// Infer a response Content-Type from the filename extension (RSCTF serves a
/// guessed type, not a blanket `application/octet-stream`). Unknown extensions
/// fall back to octet-stream so an arbitrary blob still downloads.
fn content_type_for(filename: &str) -> &'static str {
    let ext = filename
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "txt" | "md" | "log" => "text/plain; charset=utf-8",
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "xml" => "application/xml",
        "csv" => "text/csv; charset=utf-8",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "tar" => "application/x-tar",
        "7z" => "application/x-7z-compressed",
        "wasm" => "application/wasm",
        "bin" | "exe" | "elf" | "so" => "application/octet-stream",
        _ => "application/octet-stream",
    }
}

/// Emit an `EventType::Download` GameEvent for a participant who downloaded a
/// challenge attachment (static or dynamic), mirroring RSCTF's
/// `AssetsController` download logging so the abnormal-solve checks
/// (`NoDownload` / `FastSolve-Download`) have event input.
///
/// Deduped once per `(team, challenge)` on the event's `values[0]` (the
/// challenge-id string) exactly like the `ChallengeOpened` handler, which keeps
/// the earliest download time — precisely what `FastSolve-Download` (min) and
/// `NoDownload` (existence) read — and avoids event-table bloat. `Values` are
/// `[challengeId, challengeTitle, token]`, matching the RSCTF ordering that puts
/// the parseable challenge id first.
async fn log_attachment_download(
    st: &SharedState,
    hash: &str,
    user: &Option<CurrentUser>,
    token: Option<&str>,
) -> AppResult<()> {
    // Only participants generate download events (RSCTF logs the acting player).
    let Some(u) = user else { return Ok(()) };

    let Some(lf) = local_file::Entity::find()
        .filter(local_file::Column::Hash.eq(hash))
        .filter(local_file::Column::ReferenceCount.gt(0))
        .one(&st.db)
        .await?
    else {
        return Ok(());
    };

    let att_ids: Vec<i32> = attachment::Entity::find()
        .filter(attachment::Column::LocalFileId.eq(lf.id))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|a| a.id)
        .collect();
    if att_ids.is_empty() {
        return Ok(()); // not a challenge attachment (avatar/poster/writeup/orphan)
    }

    // Resolve every challenge this blob is an attachment for — both the static
    // path (`game_challenge.attachment_id`) and the dynamic per-instance path
    // (`flag_context.attachment_id` → its `challenge_id`).
    let mut challenge_ids: std::collections::BTreeSet<i32> = std::collections::BTreeSet::new();
    for c in game_challenge::Entity::find()
        .filter(game_challenge::Column::AttachmentId.is_in(att_ids.clone()))
        .all(&st.db)
        .await?
    {
        challenge_ids.insert(c.id);
    }
    for fc in flag_context::Entity::find()
        .filter(flag_context::Column::AttachmentId.is_in(att_ids))
        .all(&st.db)
        .await?
    {
        if let Some(cid) = fc.challenge_id {
            challenge_ids.insert(cid);
        }
    }
    if challenge_ids.is_empty() {
        return Ok(());
    }

    for cid in challenge_ids {
        let Some(chal) = game_challenge::Entity::find_by_id(cid).one(&st.db).await? else {
            continue;
        };
        // The downloader must be a participant of the challenge's game; use their
        // team for the event (the event's team is carried on `team_id`).
        let Some(link) = user_participation::Entity::find_by_id((u.id, chal.game_id))
            .one(&st.db)
            .await?
        else {
            continue;
        };

        // Dedup once per (team, challenge) — keep the earliest download.
        let cid_str = cid.to_string();
        let already = game_event::Entity::find()
            .filter(game_event::Column::GameId.eq(chal.game_id))
            .filter(game_event::Column::TeamId.eq(link.team_id))
            .filter(game_event::Column::EventType.eq(crate::utils::enums::EventType::Download))
            .all(&st.db)
            .await?
            .into_iter()
            .any(|e| e.values.get(0).and_then(|v| v.as_str()) == Some(cid_str.as_str()));
        if already {
            continue;
        }

        let ev = game_event::ActiveModel {
            game_id: Set(chal.game_id),
            event_type: Set(crate::utils::enums::EventType::Download),
            values: Set(serde_json::json!([
                cid_str,
                chal.title,
                token.unwrap_or("")
            ])),
            publish_time_utc: Set(Utc::now()),
            user_id: Set(Some(u.id)),
            team_id: Set(link.team_id),
            ..Default::default()
        };
        ev.insert(&st.db).await?;
    }

    Ok(())
}

/// `DELETE /api/assets/{hash}` (admin) — delete a blob and its row.
pub async fn delete_asset(
    State(st): State<SharedState>,
    AdminUser(_user): AdminUser,
    Path(hash): Path<String>,
) -> AppResult<MessageResponse> {
    let outcome = crate::services::blob_refs::release_by_hash(st.pg(), &hash).await?;
    if !outcome.found {
        return Err(AppError::not_found("File not found"));
    }

    // Metadata commits before shared storage is touched. Recheck by unique
    // content hash so a concurrent replica that reacquired it keeps the blob.
    if let Some(deleted_hash) = outcome.deleted_hash {
        let purge = crate::services::blob_refs::purge_if_unreferenced(
            st.pg(),
            st.storage.as_ref(),
            &deleted_hash,
        )
        .await;
        // Drop immutable bytes so a stale hit cannot serve deleted content.
        st.cache.remove(&asset_bytes_key(&deleted_hash)).await;
        purge?;
    }

    // Success type is `void`; RSCTF returns an empty 200 body.
    Ok(MessageResponse::ok(""))
}

/// Strip characters that would break a `Content-Disposition` header.
fn sanitize(filename: &str) -> String {
    filename.replace(['"', '\r', '\n'], "")
}
