//! Ported from RSCTF `Controllers/AssetsController.cs`.
//!
//! File APIs: upload (admin), download-by-hash, and delete (admin). Public brand
//! assets remain anonymous; challenge attachments and team-owned artifacts are
//! authorized against live game participation before their bytes are loaded.

use axum::body::Body;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Multipart, Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Serialize;

use bytes::Bytes;
use futures::StreamExt;
use std::net::SocketAddr;
use std::ops::Range;
use std::time::Duration;

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::{AdminUser, CurrentUser, MaybeUser};
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::MessageResponse;

mod authorization;

use authorization::{authorize_asset_download, finalize_asset_download, AssetCachePolicy};

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
        .route(
            "/api/assets",
            post(upload).layer(DefaultBodyLimit::max(
                crate::utils::upload::ASSET_BODY_BYTES,
            )),
        )
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
    let mut uploads = Vec::new();
    let mut total_bytes = 0usize;

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
        if name.len() > 255 {
            return Err(AppError::bad_request("File name is too long"));
        }
        if bytes.len() > crate::utils::upload::ASSET_FILE_BYTES {
            return Err(AppError::bad_request("File is too large"));
        }
        total_bytes = total_bytes
            .checked_add(bytes.len())
            .filter(|total| *total <= crate::utils::upload::ASSET_TOTAL_BYTES)
            .ok_or_else(|| AppError::bad_request("Upload exceeds the total size limit"))?;
        uploads.push((name, bytes));
    }

    if uploads.is_empty() {
        return Err(AppError::bad_request("No file provided"));
    }

    // Validate the complete request before acquiring any blob references. A
    // later oversized part must not leave the earlier parts persisted.
    let mut results = Vec::with_capacity(uploads.len());
    for (name, bytes) in uploads {
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

    Ok(Json(results))
}

fn asset_bytes_key(hash: &str) -> String {
    format!("assetblob:{hash}")
}
/// Blob bytes are content-hash immutable. Only small blobs are cached. The
/// user-specific authorization check remains live; the relationship half has
/// the short bounded cache documented in `authorization`.
const ASSET_BYTES_TTL: Duration = Duration::from_secs(600);
const ASSET_CACHE_MAX_BYTES: usize = 512 * 1024;

/// Load a small blob, serving cached `Bytes` zero-copy on a hit. Callers check
/// the stored size first, so this never allocates for a large attachment.
async fn load_small_asset_bytes(st: &SharedState, hash: &str) -> AppResult<Bytes> {
    let key = asset_bytes_key(hash);
    if let Some(b) = st.cache.get(&key).await {
        return Ok(b);
    }
    let bytes = st.storage.load_bounded(hash, ASSET_CACHE_MAX_BYTES).await?;
    st.cache.set(&key, &bytes, Some(ASSET_BYTES_TTL)).await;
    Ok(Bytes::from(bytes))
}

/// Parse one RFC 9110 byte range and return an exclusive range. Multi-range
/// responses are deliberately rejected: resumable downloads need only a
/// single range, while multipart range bodies add substantial complexity.
fn parse_byte_range(value: &str, size: u64) -> Result<Range<u64>, ()> {
    let value = value.strip_prefix("bytes=").ok_or(())?;
    if value.is_empty() || value.contains(',') || size == 0 {
        return Err(());
    }
    let (start, end) = value.split_once('-').ok_or(())?;
    if start.is_empty() {
        let suffix = end.parse::<u64>().map_err(|_| ())?;
        if suffix == 0 {
            return Err(());
        }
        return Ok(size.saturating_sub(suffix)..size);
    }

    let start = start.parse::<u64>().map_err(|_| ())?;
    if start >= size {
        return Err(());
    }
    let end = if end.is_empty() {
        size
    } else {
        let inclusive = end.parse::<u64>().map_err(|_| ())?;
        if inclusive < start {
            return Err(());
        }
        inclusive.saturating_add(1).min(size)
    };
    Ok(start..end)
}

fn range_not_satisfiable(size: u64, etag: &str, cache_policy: AssetCachePolicy) -> Response {
    let mut response = StatusCode::RANGE_NOT_SATISFIABLE.into_response();
    let headers = response.headers_mut();
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(
        header::CONTENT_RANGE,
        HeaderValue::from_str(&format!("bytes */{size}")).expect("valid content range"),
    );
    headers.insert(
        header::ETAG,
        HeaderValue::from_str(etag).expect("hash ETag is ASCII"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_policy.header()),
    );
    response
}

fn asset_response(
    body: Body,
    status: StatusCode,
    size: u64,
    range: Option<&Range<u64>>,
    filename: &str,
    etag: &str,
    cache_policy: AssetCachePolicy,
) -> AppResult<Response> {
    let mut response = Response::new(body);
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type_for(filename)),
    );
    let disposition = crate::utils::content_disposition::attachment(filename);
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&disposition)
            .map_err(|_| AppError::bad_request("Invalid attachment filename"))?,
    );
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(
            &(range.map(|range| range.end - range.start).unwrap_or(size)).to_string(),
        )
        .expect("u64 content length is ASCII"),
    );
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if let Some(range) = range {
        headers.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {}-{}/{size}", range.start, range.end - 1))
                .expect("valid content range"),
        );
    }
    headers.insert(
        header::ETAG,
        HeaderValue::from_str(etag).expect("hash ETag is ASCII"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_policy.header()),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}

fn signed_download_response(location: &str) -> AppResult<Response> {
    let uri = location
        .parse::<axum::http::Uri>()
        .map_err(|_| AppError::internal("storage returned an invalid signed URL"))?;
    if uri.scheme_str() != Some("https") || uri.authority().is_none() {
        return Err(AppError::internal(
            "signed asset delivery requires an absolute HTTPS URL",
        ));
    }
    let mut response = StatusCode::TEMPORARY_REDIRECT.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::LOCATION,
        HeaderValue::from_str(location)
            .map_err(|_| AppError::internal("storage returned an invalid signed URL"))?,
    );
    // Never cache a credential-bearing redirect. The immutable object response
    // may be cached according to the storage/CDN policy after it validates the
    // short-lived signature.
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}

fn permitted_stream_body(
    stream: crate::storage::BlobByteStream,
    permit: crate::services::asset_admission::AssetDownloadPermit,
) -> Body {
    // The unfold state owns the permit for exactly as long as Axum owns the
    // response body. Completion, transport error, or client disconnect all drop
    // it without a background cleanup task.
    let held = futures::stream::unfold((stream, permit), |(mut stream, permit)| async move {
        stream.next().await.map(|item| (item, (stream, permit)))
    });
    Body::from_stream(held)
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
    let authorization = authorize_asset_download(st, hash, user).await?;
    let cache_policy = authorization.cache_policy;

    // Conditional caching (RSCTF `AssetsController`): a content-hash blob is
    // immutable, so an `ETag` of hash[8..16] lets the browser skip re-downloading.
    let etag = format!("\"{}\"", hash.get(8..16).unwrap_or(""));
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == etag))
    {
        finalize_asset_download(st.pg(), &authorization, token, false).await?;
        return Ok((
            StatusCode::NOT_MODIFIED,
            [
                (header::ETAG, etag),
                (header::CACHE_CONTROL, cache_policy.header().to_string()),
            ],
        )
            .into_response());
    }

    // Preserve the small-asset zero-copy cache. On a miss, fetch metadata only;
    // large attachments are opened as a stream rather than collected in RAM.
    let cached = st.cache.get(&asset_bytes_key(hash)).await;
    let size = match &cached {
        Some(bytes) => bytes.len() as u64,
        None if authorization.file_size.is_some_and(|size| size > 0) => {
            authorization.file_size.expect("positive stored file size")
        }
        None => match st.storage.size(hash).await {
            Ok(size) => size,
            Err(_) => {
                // RSCTF `AssetsController` audit event (`Assets_FileNotFound`):
                // Warning-level, TaskStatus.NotFound, no acting user.
                let short = hash.get(..8).unwrap_or(hash);
                crate::services::audit::log(
                    st,
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
        },
    };

    // If-Range permits a client to resume only while it still has this exact
    // immutable object. A stale validator falls back to a normal full response.
    let requested_range = if headers
        .get(header::IF_RANGE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|validator| validator != etag)
    {
        None
    } else {
        match headers.get(header::RANGE) {
            Some(value) => match value
                .to_str()
                .map_err(|_| ())
                .and_then(|value| parse_byte_range(value, size))
            {
                Ok(range) => Some(range),
                Err(()) => {
                    finalize_asset_download(st.pg(), &authorization, token, false).await?;
                    return Ok(range_not_satisfiable(size, &etag, cache_policy));
                }
            },
            None => None,
        }
    };

    // When an operator explicitly enables signed object-store delivery, RSCTF
    // remains the authorization/audit control plane while the storage endpoint
    // carries the large byte stream. If-Range stays on the proxy path because
    // the object store may use a different ETag; forwarding that validator
    // could unexpectedly restart a resumed download from byte zero.
    if size > ASSET_CACHE_MAX_BYTES as u64
        && authorization.signed_delivery_allowed
        && headers.get(header::IF_RANGE).is_none()
    {
        if let Some(ttl_secs) = st.config.asset_signed_url_ttl_secs {
            match st
                .storage
                .signed_download_url(hash, Duration::from_secs(ttl_secs))
                .await
            {
                Ok(Some(location)) => match signed_download_response(&location) {
                    Ok(response) => {
                        finalize_asset_download(st.pg(), &authorization, token, true).await?;
                        return Ok(response);
                    }
                    Err(error) => tracing::warn!(
                        hash = %hash,
                        %error,
                        "storage returned an unsafe signed asset URL; using proxy stream"
                    ),
                },
                Ok(None) => tracing::warn!(
                    hash = %hash,
                    "signed asset delivery is configured but unavailable; using proxy stream"
                ),
                Err(error) => tracing::warn!(
                    hash = %hash,
                    %error,
                    "signed asset URL generation failed; using proxy stream"
                ),
            }
        }
    }

    let body = match (&cached, &requested_range) {
        (Some(bytes), Some(range)) => {
            let start = usize::try_from(range.start).expect("cached asset fits usize");
            let end = usize::try_from(range.end).expect("cached asset fits usize");
            Body::from(bytes.slice(start..end))
        }
        (Some(bytes), None) => Body::from(bytes.clone()),
        (None, None) if size == 0 => Body::empty(),
        (None, None) if size <= ASSET_CACHE_MAX_BYTES as u64 => {
            match load_small_asset_bytes(st, hash).await {
                Ok(bytes) => Body::from(bytes),
                Err(_) => return Err(AppError::not_found("File not found")),
            }
        }
        (None, range) => {
            let range = range.clone().unwrap_or(0..size);
            let permit = st
                .asset_download_admission
                .try_acquire(user.as_ref().map(|user| user.id), hash)
                .ok_or_else(|| {
                    AppError::unavailable("Attachment download capacity is busy; retry in a moment")
                })?;
            match st.storage.stream_range(hash, range).await {
                Ok(stream) => permitted_stream_body(stream, permit),
                Err(_) => return Err(AppError::not_found("File not found")),
            }
        }
    };

    let status = if requested_range.is_some() {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };

    // Storage preparation can be slow. Revalidate the exact roster, stamp,
    // challenge, and division now; commit the precisely timed Download event
    // under that fence, then release it before Axum begins streaming the body.
    finalize_asset_download(st.pg(), &authorization, token, true).await?;

    asset_response(
        body,
        status,
        size,
        requested_range.as_ref(),
        filename,
        &etag,
        cache_policy,
    )
}

/// Infer a response Content-Type from the filename extension. Formats that a
/// browser can execute as an active same-origin subresource are deliberately
/// forced to octet-stream; the URL's filename is caller-controlled and is not
/// trustworthy metadata for JavaScript, HTML, SVG, CSS, XML, or WebAssembly.
fn content_type_for(filename: &str) -> &'static str {
    const INERT: &str = "application/octet-stream";
    let ext = filename
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "txt" | "md" | "log" => "text/plain; charset=utf-8",
        "json" => "application/json",
        "csv" => "text/csv; charset=utf-8",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "tar" => "application/x-tar",
        "7z" => "application/x-7z-compressed",
        "html" | "htm" | "css" | "js" | "mjs" | "xml" | "svg" | "wasm" => INERT,
        "bin" | "exe" | "elf" | "so" => INERT,
        _ => INERT,
    }
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

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{header, StatusCode};

    use super::{
        asset_response, content_type_for, parse_byte_range, signed_download_response,
        AssetCachePolicy,
    };

    #[test]
    fn caller_chosen_active_extensions_are_always_inert() {
        for filename in [
            "payload.html",
            "payload.HTM",
            "payload.css",
            "payload.js",
            "payload.mjs",
            "payload.xml",
            "payload.svg",
            "payload.wasm",
        ] {
            assert_eq!(
                content_type_for(filename),
                "application/octet-stream",
                "{filename} remained browser-executable"
            );
        }
    }

    #[test]
    fn passive_download_types_keep_their_useful_mime_type() {
        assert_eq!(content_type_for("image.png"), "image/png");
        assert_eq!(content_type_for("notes.txt"), "text/plain; charset=utf-8");
        assert_eq!(content_type_for("archive.zip"), "application/zip");
    }

    #[test]
    fn byte_ranges_cover_resume_open_ended_and_suffix_requests() {
        assert_eq!(parse_byte_range("bytes=0-0", 100), Ok(0..1));
        assert_eq!(parse_byte_range("bytes=40-", 100), Ok(40..100));
        assert_eq!(parse_byte_range("bytes=-10", 100), Ok(90..100));
        assert_eq!(parse_byte_range("bytes=-200", 100), Ok(0..100));
        assert_eq!(parse_byte_range("bytes=90-200", 100), Ok(90..100));
    }

    #[test]
    fn invalid_or_multi_ranges_are_rejected() {
        for value in [
            "items=0-1",
            "bytes=",
            "bytes=100-",
            "bytes=5-4",
            "bytes=-0",
            "bytes=0-1,4-5",
        ] {
            assert_eq!(parse_byte_range(value, 100), Err(()), "{value}");
        }
        assert_eq!(parse_byte_range("bytes=0-0", 0), Err(()));
    }

    #[test]
    fn partial_download_response_exposes_resume_metadata() {
        let response = asset_response(
            Body::from("four"),
            StatusCode::PARTIAL_CONTENT,
            10,
            Some(&(3..7)),
            "challenge.zip",
            "\"12345678\"",
            AssetCachePolicy::PrivateNoStore,
        )
        .unwrap();

        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.headers()[header::ACCEPT_RANGES], "bytes");
        assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes 3-6/10");
        assert_eq!(response.headers()[header::CONTENT_LENGTH], "4");
        assert_eq!(
            response.headers()[header::CONTENT_DISPOSITION],
            "attachment; filename=\"challenge.zip\"; filename*=UTF-8''challenge.zip"
        );
    }

    #[test]
    fn signed_redirect_never_caches_or_leaks_through_referrers() {
        let response = signed_download_response(
            "https://storage.example/assets/hash?X-Amz-Signature=temporary",
        )
        .unwrap();

        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "private, no-store"
        );
        assert_eq!(response.headers()[header::REFERRER_POLICY], "no-referrer");
        assert_eq!(
            response.headers()[header::LOCATION],
            "https://storage.example/assets/hash?X-Amz-Signature=temporary"
        );
        assert!(
            signed_download_response("http://storage.example/assets/hash?token=temporary").is_err()
        );
        assert!(signed_download_response("/relative/path").is_err());
    }
}

#[cfg(test)]
#[path = "assets_tests.rs"]
mod database_tests;
