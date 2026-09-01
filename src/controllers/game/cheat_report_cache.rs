//! Serialized anti-cheat report cache and conditional HTTP response handling.

use super::*;
use axum::http::{header, HeaderValue};
use bytes::{BufMut, Bytes, BytesMut};
use sha2::{Digest, Sha256};

const MAGIC: &[u8; 8] = b"RSCTFAC1";
const VERSION_LEN: usize = 32;
const HEADER_LEN: usize = MAGIC.len() + 1 + VERSION_LEN;
const MAX_REPORT_BUNDLE_BYTES: usize = 4 * 1024 * 1024;
const MAX_REPORT_BODY_BYTES: usize = MAX_REPORT_BUNDLE_BYTES - HEADER_LEN;
const LIVE_REPORT_TTL: std::time::Duration = std::time::Duration::from_secs(5);
const SEALED_REPORT_TTL: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 60 * 60);
const REPORT_BUILD_CONCURRENCY: usize = 2;
const CACHE_CONTROL: &str = "private, no-cache, max-age=0";
const REPORT_VERSION_HEADER: &str = "x-anticheat-report-version";

static REPORT_BUILD_ADMISSION: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| {
        std::sync::Arc::new(tokio::sync::Semaphore::new(REPORT_BUILD_CONCURRENCY))
    });
static REPORT_SF: std::sync::LazyLock<crate::utils::single_flight::SingleFlight<ReportFill>> =
    std::sync::LazyLock::new(crate::utils::single_flight::SingleFlight::new);

#[derive(Clone, Debug, Default)]
enum ReportFill {
    Ready(Bytes),
    Busy,
    TooLarge(String),
    Failed(String),
    #[default]
    TimedOut,
}

#[derive(Debug)]
enum ReportCacheError {
    Busy,
    TooLarge(String),
    Failed(String),
}

/// A single-flight deadline may drop its waiter while blocking-pool work keeps
/// running. The nested task owns admission until the entire fill really ends,
/// so timeout/cancellation cannot admit overlapping non-cancellable work.
async fn run_admitted_fill<Fill>(
    permit: tokio::sync::OwnedSemaphorePermit,
    fill: Fill,
) -> ReportFill
where
    Fill: std::future::Future<Output = ReportFill> + Send + 'static,
{
    tokio::spawn(async move {
        let _permit = permit;
        fill.await
    })
    .await
    .unwrap_or_else(|error| {
        ReportFill::Failed(format!("anti-cheat report fill task failed: {error}"))
    })
}

fn cache_key(game_id: i32) -> String {
    format!("_AntiCheatReportWireV1_{game_id}")
}

fn semantic_version(raw: &[u8], scope: &str) -> Result<[u8; VERSION_LEN], String> {
    const GENERATED_AT: &[u8] = b"\"generatedAt\":";
    let start = raw
        .windows(GENERATED_AT.len())
        .position(|window| window == GENERATED_AT)
        .map(|offset| offset + GENERATED_AT.len())
        .ok_or_else(|| "anti-cheat report generatedAt field is missing".to_string())?;
    let mut end = start;
    if raw.get(end) == Some(&b'-') {
        end += 1;
    }
    let digits_start = end;
    while raw.get(end).is_some_and(u8::is_ascii_digit) {
        end += 1;
    }
    if end == digits_start || !matches!(raw.get(end), Some(b',') | Some(b'}')) {
        return Err("anti-cheat report generatedAt field is invalid".to_string());
    }
    let mut digest = Sha256::new();
    digest.update(b"rsctf-anticheat-report-version-v1\0");
    digest.update(scope.as_bytes());
    digest.update(b"\0");
    digest.update(&raw[..start]);
    digest.update(&raw[end..]);
    Ok(digest.finalize().into())
}

fn encode_report(model: CheatReport, scope: &str) -> Result<Bytes, ReportFill> {
    let sealed = model.sealed_at.is_some();
    let raw = serde_json::to_vec(&model)
        .map_err(|error| ReportFill::Failed(format!("serialize anti-cheat report: {error}")))?;
    if raw.len() > MAX_REPORT_BODY_BYTES {
        return Err(ReportFill::TooLarge(format!(
            "Anti-cheat report exceeds the {MAX_REPORT_BODY_BYTES} byte response limit"
        )));
    }
    let version = semantic_version(&raw, scope).map_err(ReportFill::Failed)?;
    let capacity = HEADER_LEN
        .checked_add(raw.len())
        .ok_or_else(|| ReportFill::TooLarge("Anti-cheat report is too large".to_string()))?;
    let mut bundle = BytesMut::with_capacity(capacity);
    bundle.extend_from_slice(MAGIC);
    bundle.put_u8(u8::from(sealed));
    bundle.extend_from_slice(&version);
    bundle.extend_from_slice(&raw);
    Ok(bundle.freeze())
}

fn valid_bundle(bundle: &[u8]) -> bool {
    bundle.len() > HEADER_LEN
        && bundle.get(..MAGIC.len()) == Some(MAGIC.as_slice())
        && bundle.get(MAGIC.len()).is_some_and(|flag| *flag <= 1)
        && bundle.get(HEADER_LEN) == Some(&b'{')
        && bundle.len() <= MAX_REPORT_BUNDLE_BYTES
}

fn report_body(bundle: Bytes) -> AppResult<Bytes> {
    if !valid_bundle(&bundle) {
        return Err(AppError::internal("Corrupt anti-cheat report cache entry"));
    }
    Ok(bundle.slice(HEADER_LEN..))
}

fn report_version(bundle: &[u8]) -> Option<&[u8]> {
    valid_bundle(bundle).then(|| &bundle[MAGIC.len() + 1..HEADER_LEN])
}

fn sealed_bundle(bundle: &[u8]) -> bool {
    valid_bundle(bundle) && bundle[MAGIC.len()] == 1
}

async fn cached_report_bundle<Build, BuildFuture>(
    cache: std::sync::Arc<dyn crate::services::cache::Cache>,
    key: String,
    build: Build,
) -> Result<Bytes, ReportCacheError>
where
    Build: FnOnce() -> BuildFuture + Send + 'static,
    BuildFuture: std::future::Future<Output = AppResult<CheatReport>> + Send + 'static,
{
    if let Some(bundle) = cache.get(&key).await {
        if valid_bundle(&bundle) {
            return Ok(bundle);
        }
        tracing::warn!(
            cache_key = key,
            "evicting corrupt anti-cheat report cache entry"
        );
        cache.remove(&key).await;
    }

    let cache_for_fill = cache.clone();
    let key_for_fill = key.clone();
    let fill = REPORT_SF
        .run(&key, move || async move {
            if let Some(bundle) = cache_for_fill.get(&key_for_fill).await {
                if valid_bundle(&bundle) {
                    return ReportFill::Ready(bundle);
                }
                cache_for_fill.remove(&key_for_fill).await;
            }
            let Ok(permit) = REPORT_BUILD_ADMISSION.clone().try_acquire_owned() else {
                return ReportFill::Busy;
            };
            run_admitted_fill(permit, async move {
                let model = match build().await {
                    Ok(model) => model,
                    Err(AppError::PayloadTooLarge(message)) => {
                        return ReportFill::TooLarge(message);
                    }
                    Err(error) => return ReportFill::Failed(error.to_string()),
                };
                let encoding_scope = key_for_fill.clone();
                let bundle = match tokio::task::spawn_blocking(move || {
                    encode_report(model, &encoding_scope)
                })
                .await
                {
                    Ok(Ok(bundle)) => bundle,
                    Ok(Err(error)) => return error,
                    Err(error) => {
                        return ReportFill::Failed(format!(
                            "anti-cheat report serialization task failed: {error}"
                        ));
                    }
                };
                let ttl = if sealed_bundle(&bundle) {
                    SEALED_REPORT_TTL
                } else {
                    LIVE_REPORT_TTL
                };
                cache_for_fill.set(&key_for_fill, &bundle, Some(ttl)).await;
                ReportFill::Ready(bundle)
            })
            .await
        })
        .await;

    match fill {
        ReportFill::Ready(bundle) => Ok(bundle),
        ReportFill::Busy => Err(ReportCacheError::Busy),
        ReportFill::TooLarge(message) => Err(ReportCacheError::TooLarge(message)),
        ReportFill::Failed(message) => Err(ReportCacheError::Failed(message)),
        ReportFill::TimedOut => Err(ReportCacheError::Busy),
    }
}

fn weak_etag_matches(headers: &HeaderMap, current: &str) -> bool {
    let current = current.strip_prefix("W/").unwrap_or(current);
    headers.get_all(header::IF_NONE_MATCH).iter().any(|value| {
        value.to_str().is_ok_and(|value| {
            value.split(',').any(|candidate| {
                let candidate = candidate.trim();
                candidate == "*" || candidate.strip_prefix("W/").unwrap_or(candidate) == current
            })
        })
    })
}

fn insert_report_headers(response: &mut Response, etag: &str, version: &str) -> AppResult<()> {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(CACHE_CONTROL),
    );
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(etag)
            .map_err(|error| AppError::internal(format!("build anti-cheat ETag: {error}")))?,
    );
    response.headers_mut().insert(
        REPORT_VERSION_HEADER,
        HeaderValue::from_str(version).map_err(|error| {
            AppError::internal(format!("build anti-cheat report version: {error}"))
        })?,
    );
    Ok(())
}

fn conditional_response(bundle: Bytes, headers: &HeaderMap) -> AppResult<Response> {
    let version = report_version(&bundle)
        .map(hex::encode)
        .ok_or_else(|| AppError::internal("Corrupt anti-cheat report cache entry"))?;
    let etag = format!("W/\"rsctf-anticheat-{version}\"");
    if weak_etag_matches(headers, &etag) {
        let mut response = StatusCode::NOT_MODIFIED.into_response();
        insert_report_headers(&mut response, &etag, &version)?;
        return Ok(response);
    }
    let mut response = report_body(bundle)?.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    insert_report_headers(&mut response, &etag, &version)?;
    Ok(response)
}

fn busy_response() -> Response {
    let mut response =
        AppError::unavailable("Anti-cheat report workers are busy; retry shortly").into_response();
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    response
}

pub(super) async fn serve_report(
    st: &SharedState,
    game_id: i32,
    headers: &HeaderMap,
) -> AppResult<Response> {
    let key = cache_key(game_id);
    let state = st.clone();
    let bundle = match cached_report_bundle(st.cache.clone(), key, move || async move {
        super::cheat::build_cheat_report(&state, game_id).await
    })
    .await
    {
        Ok(bundle) => bundle,
        Err(ReportCacheError::Busy) => return Ok(busy_response()),
        Err(ReportCacheError::TooLarge(message)) => {
            return Err(AppError::payload_too_large(message));
        }
        Err(ReportCacheError::Failed(message)) => return Err(AppError::internal(message)),
    };
    conditional_response(bundle, headers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    fn report(generated_at: DateTime<Utc>, sealed_at: Option<DateTime<Utc>>) -> CheatReport {
        CheatReport {
            generated_at,
            sealed_at,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn concurrent_tabs_singleflight_one_serialized_report_build() {
        let cache: Arc<dyn crate::services::cache::Cache> =
            Arc::new(crate::services::cache::InMemoryCache::new());
        let builds = Arc::new(AtomicUsize::new(0));
        let key = format!("anti-cheat-report-test:{}", uuid::Uuid::new_v4());
        let readers = (0..24).map(|_| {
            let cache = cache.clone();
            let key = key.clone();
            let builds = builds.clone();
            async move {
                cached_report_bundle(cache, key, move || async move {
                    builds.fetch_add(1, Ordering::SeqCst);
                    tokio::task::yield_now().await;
                    Ok(report(Utc::now(), None))
                })
                .await
                .unwrap()
            }
        });
        let bundles = futures::future::join_all(readers).await;
        assert_eq!(builds.load(Ordering::SeqCst), 1);
        assert!(bundles.iter().all(|bundle| valid_bundle(bundle)));
    }

    #[tokio::test]
    async fn cancelled_fill_waiter_cannot_release_admission_early() {
        let admission = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = admission.clone().try_acquire_owned().unwrap();
        let started = Arc::new(tokio::sync::Notify::new());
        let finish = Arc::new(tokio::sync::Notify::new());
        let inner_started = started.clone();
        let inner_finish = finish.clone();
        let waiter = tokio::spawn(run_admitted_fill(permit, async move {
            inner_started.notify_one();
            inner_finish.notified().await;
            ReportFill::Busy
        }));
        started.notified().await;
        waiter.abort();
        tokio::task::yield_now().await;
        assert!(admission.clone().try_acquire_owned().is_err());

        finish.notify_one();
        for _ in 0..32 {
            if let Ok(released) = admission.clone().try_acquire_owned() {
                drop(released);
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("detached report fill did not release admission after completing");
    }

    #[tokio::test]
    async fn semantic_version_ignores_generation_time_and_returns_bodyless_304() {
        let first = encode_report(
            report(DateTime::from_timestamp(10, 0).unwrap(), None),
            "game-1",
        )
        .unwrap();
        let second = encode_report(
            report(DateTime::from_timestamp(20, 0).unwrap(), None),
            "game-1",
        )
        .unwrap();
        assert_eq!(report_version(&first), report_version(&second));

        let initial = conditional_response(first, &HeaderMap::new()).unwrap();
        let etag = initial.headers()[header::ETAG].clone();
        let mut headers = HeaderMap::new();
        headers.insert(header::IF_NONE_MATCH, etag);
        let unchanged = conditional_response(second, &headers).unwrap();
        assert_eq!(unchanged.status(), StatusCode::NOT_MODIFIED);
        assert!(to_bytes(unchanged.into_body(), 1).await.unwrap().is_empty());
    }

    #[test]
    fn sealed_bit_is_part_of_the_cached_generation() {
        let sealed_at = DateTime::from_timestamp(30, 0).unwrap();
        let bundle = encode_report(report(sealed_at, Some(sealed_at)), "game-1").unwrap();
        assert!(sealed_bundle(&bundle));
    }

    #[test]
    fn report_overload_response_is_retryable() {
        let response = busy_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()[header::RETRY_AFTER], "1");
    }

    #[test]
    fn serialized_report_has_a_hard_cache_and_wire_bound() {
        let mut oversized = report(DateTime::from_timestamp(10, 0).unwrap(), None);
        oversized.ip_analysis = vec![serde_json::json!("x".repeat(MAX_REPORT_BODY_BYTES))];
        assert!(matches!(
            encode_report(oversized, "game-1"),
            Err(ReportFill::TooLarge(_))
        ));
    }
}
