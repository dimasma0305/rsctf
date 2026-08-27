use std::io::Write;

use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::{BufMut, Bytes, BytesMut};
use sha2::{Digest, Sha256};

use crate::utils::error::{AppError, AppResult};

// V1 remains the A&D/combined format and stays readable during cutover. V2 is
// isolated to versioned standard/KotH keys and adds a semantic-version digest
// while keeping all three representations atomic.
const LEGACY_MAGIC: &[u8; 8] = b"RSADENC1";
const MAGIC: &[u8; 8] = b"RSADENC2";
const VERSION_DIGEST_LEN: usize = 32;
const LEGACY_HEADER_LEN: usize = LEGACY_MAGIC.len() + 3 * size_of::<u32>();
const HEADER_LEN: usize = MAGIC.len() + 3 * size_of::<u32>() + VERSION_DIGEST_LEN;
const MIN_COMPRESSION_SIZE: usize = 4 * 1024;
/// One Redis/L1 value may not monopolize the bounded cache working set.
const MAX_CACHE_BUNDLE_SIZE: usize = 4 * 1024 * 1024;
/// Identity, gzip, and Brotli scoreboard representations have one hard public
/// response bound. A body beyond this limit is rejected instead of becoming an
/// unbounded uncached response on every synchronized poll.
const MAX_WIRE_REPRESENTATION_SIZE: usize = 8 * 1024 * 1024;
const CACHE_CONTROL_VALUE: &str = "private, no-cache, max-age=0";
const SCOREBOARD_VERSION_HEADER: &str = "x-scoreboard-version";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Encoding {
    Brotli,
    Gzip,
    Identity,
    NotAcceptable,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Negotiated {
    encoding: Encoding,
    identity_allowed: bool,
}

#[derive(Clone, Copy, Debug)]
struct BundleRanges {
    version_start: Option<usize>,
    raw_start: usize,
    raw_end: usize,
    gzip_end: usize,
    brotli_end: usize,
}

pub(super) struct BuiltBoardBody {
    pub bytes: Bytes,
    pub cacheable: bool,
}

fn quality(field: &str) -> Option<f32> {
    let (name, value) = field.trim().split_once('=')?;
    name.trim()
        .eq_ignore_ascii_case("q")
        .then(|| value.trim().parse::<f32>().ok())
        .flatten()
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(0.0, 1.0))
}

fn record_quality(slot: &mut Option<f32>, value: f32) {
    *slot = Some(slot.map_or(value, |previous| previous.max(value)));
}

fn negotiate(headers: &HeaderMap) -> Negotiated {
    let mut br = None;
    let mut gzip = None;
    let mut wildcard = None;
    let mut identity = None;
    let mut saw_value = false;
    for value in headers.get_all(header::ACCEPT_ENCODING) {
        let Ok(value) = value.to_str() else {
            continue;
        };
        saw_value = true;
        for item in value.split(',') {
            let mut fields = item.trim().split(';');
            let name = fields.next().unwrap_or_default().trim();
            let q = fields
                .find_map(|field| {
                    let (name, _) = field.trim().split_once('=')?;
                    name.trim()
                        .eq_ignore_ascii_case("q")
                        .then(|| quality(field).unwrap_or(0.0))
                })
                .unwrap_or(1.0);
            if name.eq_ignore_ascii_case("br") {
                record_quality(&mut br, q);
            } else if name.eq_ignore_ascii_case("gzip") || name.eq_ignore_ascii_case("x-gzip") {
                record_quality(&mut gzip, q);
            } else if name.eq_ignore_ascii_case("identity") {
                record_quality(&mut identity, q);
            } else if name == "*" {
                record_quality(&mut wildcard, q);
            }
        }
    }
    if !saw_value {
        return Negotiated {
            encoding: Encoding::Identity,
            identity_allowed: true,
        };
    }
    let br = br.or(wildcard).unwrap_or(0.0);
    let gzip = gzip.or(wildcard).unwrap_or(0.0);
    let identity_allowed = identity.unwrap_or_else(|| {
        if wildcard.is_some_and(|quality| quality == 0.0) {
            0.0
        } else {
            1.0
        }
    }) > 0.0;
    // Explicit identity quality participates in preference ordering. When
    // omitted, identity remains the fallback without overriding a listed coding.
    let identity_preference = identity.unwrap_or(0.0);
    let encoding = if br > 0.0 && br >= gzip && br >= identity_preference {
        Encoding::Brotli
    } else if gzip > 0.0 && gzip >= identity_preference {
        Encoding::Gzip
    } else if identity_allowed {
        Encoding::Identity
    } else {
        Encoding::NotAcceptable
    };
    Negotiated {
        encoding,
        identity_allowed,
    }
}

fn read_len(bytes: &[u8], offset: usize) -> Option<usize> {
    let value = bytes.get(offset..offset + size_of::<u32>())?;
    Some(u32::from_be_bytes(value.try_into().ok()?) as usize)
}

fn bundle_ranges(bytes: &[u8]) -> Option<BundleRanges> {
    let (header_len, version_start) = match bytes.get(..MAGIC.len())? {
        magic if magic == MAGIC => (HEADER_LEN, Some(LEGACY_HEADER_LEN)),
        magic if magic == LEGACY_MAGIC => (LEGACY_HEADER_LEN, None),
        _ => return None,
    };
    let raw_len = read_len(bytes, MAGIC.len())?;
    let gzip_len = read_len(bytes, MAGIC.len() + size_of::<u32>())?;
    let brotli_len = read_len(bytes, MAGIC.len() + 2 * size_of::<u32>())?;
    if raw_len > MAX_WIRE_REPRESENTATION_SIZE
        || gzip_len > MAX_WIRE_REPRESENTATION_SIZE
        || brotli_len > MAX_WIRE_REPRESENTATION_SIZE
    {
        return None;
    }
    let raw_start = header_len;
    let raw_end = raw_start.checked_add(raw_len)?;
    let gzip_end = raw_end.checked_add(gzip_len)?;
    let brotli_end = gzip_end.checked_add(brotli_len)?;
    (brotli_end == bytes.len()).then_some(BundleRanges {
        version_start,
        raw_start,
        raw_end,
        gzip_end,
        brotli_end,
    })
}

fn version_digest<'a>(bytes: &'a [u8], ranges: &BundleRanges) -> Option<&'a [u8]> {
    let start = ranges.version_start?;
    bytes.get(start..start + VERSION_DIGEST_LEN)
}

/// Validate either the current atomic encoding bundle or a legacy raw JSON
/// body before it is served from cache. Keeping this check independent of
/// content negotiation lets stale-while-revalidate reject corrupt fallback
/// entries without allocating a response first.
pub(super) fn valid_bundle(bytes: &[u8]) -> bool {
    bundle_ranges(bytes).map_or_else(
        || bytes.len() <= MAX_WIRE_REPRESENTATION_SIZE && bytes.first() == Some(&b'{'),
        |ranges| bytes.get(ranges.raw_start) == Some(&b'{'),
    )
}

/// Return the identity JSON representation from an atomic encoding bundle.
/// The slice is zero-copy, so internal callers can deserialize the same cached
/// board without rebuilding it or decompressing a negotiated response.
pub(super) fn identity_body(bundle: Bytes) -> AppResult<Bytes> {
    if !valid_bundle(&bundle) {
        return Err(AppError::internal("Corrupt scoreboard cache bundle"));
    }
    Ok(match bundle_ranges(&bundle) {
        Some(ranges) => bundle.slice(ranges.raw_start..ranges.raw_end),
        None => bundle,
    })
}

fn compress(raw: &[u8]) -> std::io::Result<(Vec<u8>, Vec<u8>)> {
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gzip.write_all(raw)?;
    let gzip = gzip.finish()?;

    let mut writer = brotli::CompressorWriter::new(Vec::new(), 64 * 1024, 4, 22);
    writer.write_all(raw)?;
    writer.flush()?;
    let brotli = writer.into_inner();
    Ok((gzip, brotli))
}

/// Preserve the established v1 bundle byte-for-byte for A&D and combined
/// scoreboards. Older replicas understand this marker during a rolling cutover;
/// only the newly namespaced standard/KotH cache keys use the versioned format.
fn encode_legacy_bundle(raw: Bytes) -> AppResult<BuiltBoardBody> {
    let (gzip, brotli) = compress(&raw).map_err(|error| AppError::internal(error.to_string()))?;
    let raw_len = u32::try_from(raw.len())
        .map_err(|_| AppError::internal("scoreboard exceeds the cache bundle limit"))?;
    let gzip_len = u32::try_from(gzip.len())
        .map_err(|_| AppError::internal("scoreboard gzip body exceeds the cache bundle limit"))?;
    let brotli_len = u32::try_from(brotli.len())
        .map_err(|_| AppError::internal("scoreboard Brotli body exceeds the cache bundle limit"))?;
    let capacity = LEGACY_HEADER_LEN
        .checked_add(raw.len())
        .and_then(|size| size.checked_add(gzip.len()))
        .and_then(|size| size.checked_add(brotli.len()))
        .ok_or_else(|| AppError::internal("scoreboard cache bundle is too large"))?;
    if capacity > MAX_CACHE_BUNDLE_SIZE {
        return Ok(BuiltBoardBody {
            bytes: raw,
            cacheable: true,
        });
    }
    let mut bundle = BytesMut::with_capacity(capacity);
    bundle.extend_from_slice(LEGACY_MAGIC);
    bundle.put_u32(raw_len);
    bundle.put_u32(gzip_len);
    bundle.put_u32(brotli_len);
    bundle.extend_from_slice(&raw);
    bundle.extend_from_slice(&gzip);
    bundle.extend_from_slice(&brotli);
    Ok(BuiltBoardBody {
        bytes: bundle.freeze(),
        cacheable: true,
    })
}

fn stable_version_digest(
    raw: &[u8],
    scope: &[u8],
    volatile_number_field: Option<&[u8]>,
) -> AppResult<[u8; VERSION_DIGEST_LEN]> {
    let mut hasher = Sha256::new();
    hasher.update(b"rsctf-scoreboard-version-v1\0");
    hasher.update(scope);
    hasher.update(b"\0");
    if let Some(field) = volatile_number_field {
        let start = raw
            .windows(field.len())
            .position(|window| window == field)
            .map(|offset| offset + field.len())
            .ok_or_else(|| AppError::internal("scoreboard version field is missing"))?;
        let mut end = start;
        if raw.get(end) == Some(&b'-') {
            end += 1;
        }
        let digits_start = end;
        while raw.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        if end == digits_start || !matches!(raw.get(end).copied(), Some(b',') | Some(b'}')) {
            return Err(AppError::internal(
                "scoreboard version field is not an integer",
            ));
        }
        // The generation timestamp is deliberately excluded. The weak ETag
        // remains stable while every scoring/lifecycle field is unchanged.
        hasher.update(&raw[..start]);
        hasher.update(&raw[end..]);
    } else {
        hasher.update(raw);
    }
    Ok(hasher.finalize().into())
}

fn encode_bundle(
    raw: Bytes,
    scope: &[u8],
    volatile_number_field: Option<&[u8]>,
) -> AppResult<BuiltBoardBody> {
    if raw.len() > MAX_WIRE_REPRESENTATION_SIZE {
        return Err(AppError::internal(format!(
            "scoreboard identity body exceeds the {} byte response limit",
            MAX_WIRE_REPRESENTATION_SIZE
        )));
    }
    let version = stable_version_digest(&raw, scope, volatile_number_field)?;
    let (gzip, brotli) = if raw.len() < MIN_COMPRESSION_SIZE {
        (Vec::new(), Vec::new())
    } else {
        compress(&raw).map_err(|error| AppError::internal(error.to_string()))?
    };
    if gzip.len() > MAX_WIRE_REPRESENTATION_SIZE || brotli.len() > MAX_WIRE_REPRESENTATION_SIZE {
        return Err(AppError::internal(format!(
            "scoreboard encoded body exceeds the {} byte response limit",
            MAX_WIRE_REPRESENTATION_SIZE
        )));
    }
    let raw_len = u32::try_from(raw.len())
        .map_err(|_| AppError::internal("scoreboard exceeds the response limit"))?;
    let gzip_len = u32::try_from(gzip.len())
        .map_err(|_| AppError::internal("scoreboard gzip body exceeds the response limit"))?;
    let brotli_len = u32::try_from(brotli.len())
        .map_err(|_| AppError::internal("scoreboard Brotli body exceeds the response limit"))?;
    let capacity = HEADER_LEN
        .checked_add(raw.len())
        .and_then(|size| size.checked_add(gzip.len()))
        .and_then(|size| size.checked_add(brotli.len()))
        .ok_or_else(|| AppError::internal("scoreboard cache bundle is too large"))?;
    let mut bundle = BytesMut::with_capacity(capacity);
    bundle.extend_from_slice(MAGIC);
    bundle.put_u32(raw_len);
    bundle.put_u32(gzip_len);
    bundle.put_u32(brotli_len);
    bundle.extend_from_slice(&version);
    bundle.extend_from_slice(&raw);
    bundle.extend_from_slice(&gzip);
    bundle.extend_from_slice(&brotli);
    Ok(BuiltBoardBody {
        bytes: bundle.freeze(),
        cacheable: capacity <= MAX_CACHE_BUNDLE_SIZE,
    })
}

/// Preserve the established A&D/combined cache behavior: small or oversized
/// bodies stay identity-only, while cache-sized encodings are built off Tokio.
/// Every representation still obeys the public hard limit.
pub(super) async fn build_bundle(raw: Bytes) -> AppResult<BuiltBoardBody> {
    if raw.len() > MAX_WIRE_REPRESENTATION_SIZE {
        return Err(AppError::internal(format!(
            "scoreboard identity body exceeds the {} byte response limit",
            MAX_WIRE_REPRESENTATION_SIZE
        )));
    }
    if raw.len() > MAX_CACHE_BUNDLE_SIZE {
        return Ok(BuiltBoardBody {
            bytes: raw,
            cacheable: false,
        });
    }
    if raw.len() < MIN_COMPRESSION_SIZE {
        return Ok(BuiltBoardBody {
            bytes: raw,
            cacheable: true,
        });
    }
    tokio::task::spawn_blocking(move || encode_legacy_bundle(raw))
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
}

/// Build a weakly versioned board whose generation timestamp does not force a
/// new validator when every substantive field is unchanged. `scope` separates
/// monitor/live and public/frozen representations even when their bodies happen
/// to be identical, preventing a browser validator from crossing an auth view.
/// Hashing and compression run off Tokio; the cache and wire caps remain
/// independent so an oversized cache value cannot grow Redis/L1.
pub(super) async fn build_stable_bundle(
    raw: Bytes,
    scope: String,
    volatile_number_field: &'static [u8],
) -> AppResult<BuiltBoardBody> {
    tokio::task::spawn_blocking(move || {
        encode_bundle(raw, scope.as_bytes(), Some(volatile_number_field))
    })
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
}

fn etag_for(
    bundle: &[u8],
    ranges: &BundleRanges,
    validator_scope: Option<&str>,
) -> Option<(String, String)> {
    let base = version_digest(bundle, ranges)?;
    let version = match validator_scope {
        Some(scope) => {
            let mut hasher = Sha256::new();
            hasher.update(b"rsctf-scoreboard-validator-view-v1\0");
            hasher.update(scope.as_bytes());
            hasher.update(b"\0");
            hasher.update(base);
            hex::encode(hasher.finalize())
        }
        None => hex::encode(base),
    };
    Some((format!("W/\"rsctf-scoreboard-{version}\""), version))
}

fn weak_etag_value(value: &str) -> &str {
    value.trim().strip_prefix("W/").unwrap_or(value.trim())
}

fn if_none_match(headers: &HeaderMap, etag: &str) -> bool {
    headers.get_all(header::IF_NONE_MATCH).iter().any(|value| {
        value.to_str().is_ok_and(|value| {
            value.split(',').any(|candidate| {
                let candidate = candidate.trim();
                candidate == "*" || weak_etag_value(candidate) == weak_etag_value(etag)
            })
        })
    })
}

fn insert_common_headers(response: &mut Response, etag: Option<&(String, String)>) {
    response
        .headers_mut()
        .insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
    if let Some((etag, version)) = etag {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(CACHE_CONTROL_VALUE),
        );
        response.headers_mut().insert(
            header::ETAG,
            HeaderValue::from_str(etag).expect("SHA-256 scoreboard ETag is ASCII"),
        );
        response.headers_mut().insert(
            SCOREBOARD_VERSION_HEADER,
            HeaderValue::from_str(version).expect("SHA-256 scoreboard version is ASCII"),
        );
    }
}

/// Select a zero-copy body slice from the atomic cache bundle. A legacy raw JSON
/// entry can remain for its short TTL; established v1 A&D/combined bundles stay
/// valid. Neither has a semantic validator, while v2 responses do.
fn response_with_scope(
    bundle: Bytes,
    headers: &HeaderMap,
    validator_scope: Option<&str>,
) -> AppResult<Response> {
    let ranges = bundle_ranges(&bundle);
    if !valid_bundle(&bundle) {
        return Err(AppError::internal(
            "Corrupt scoreboard cache bundle; retry after cache expiry",
        ));
    }
    let etag = ranges
        .as_ref()
        .and_then(|ranges| etag_for(&bundle, ranges, validator_scope));
    if etag
        .as_ref()
        .is_some_and(|(etag, _)| if_none_match(headers, etag))
    {
        let mut response = StatusCode::NOT_MODIFIED.into_response();
        insert_common_headers(&mut response, etag.as_ref());
        return Ok(response);
    }
    let requested = negotiate(headers);
    if requested.encoding == Encoding::NotAcceptable {
        let mut response = StatusCode::NOT_ACCEPTABLE.into_response();
        insert_common_headers(&mut response, etag.as_ref());
        return Ok(response);
    }
    let (body, encoding) = match ranges {
        Some(ranges) => match requested.encoding {
            Encoding::Brotli if ranges.brotli_end > ranges.gzip_end => {
                (bundle.slice(ranges.gzip_end..ranges.brotli_end), Some("br"))
            }
            Encoding::Gzip if ranges.gzip_end > ranges.raw_end => {
                (bundle.slice(ranges.raw_end..ranges.gzip_end), Some("gzip"))
            }
            Encoding::Identity => (bundle.slice(ranges.raw_start..ranges.raw_end), None),
            Encoding::Brotli | Encoding::Gzip if requested.identity_allowed => {
                (bundle.slice(ranges.raw_start..ranges.raw_end), None)
            }
            Encoding::Brotli | Encoding::Gzip | Encoding::NotAcceptable => {
                let mut response = StatusCode::NOT_ACCEPTABLE.into_response();
                insert_common_headers(&mut response, etag.as_ref());
                return Ok(response);
            }
        },
        None if requested.identity_allowed => (bundle, None),
        None => {
            let mut response = StatusCode::NOT_ACCEPTABLE.into_response();
            insert_common_headers(&mut response, etag.as_ref());
            return Ok(response);
        }
    };
    let mut response = body.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    insert_common_headers(&mut response, etag.as_ref());
    if let Some(encoding) = encoding {
        response
            .headers_mut()
            .insert(header::CONTENT_ENCODING, HeaderValue::from_static(encoding));
    }
    Ok(response)
}

/// Negotiate an established legacy/A&D/combined response. Versioned bundles
/// use their build scope directly; legacy bodies have no validator.
pub(super) fn response(bundle: Bytes, headers: &HeaderMap) -> AppResult<Response> {
    response_with_scope(bundle, headers, None)
}

/// Negotiate a standard/KotH response after authorization and lifecycle gates.
/// The small view salt prevents a validator retained across an account change
/// from matching another authorization view, while compression and full-body
/// hashing remain precomputed off the Tokio request path.
pub(super) fn scoped_response(
    bundle: Bytes,
    headers: &HeaderMap,
    validator_scope: &str,
) -> AppResult<Response> {
    response_with_scope(bundle, headers, Some(validator_scope))
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read};

    use axum::body::HttpBody;

    use super::*;

    fn headers(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT_ENCODING, value.parse().unwrap());
        headers
    }

    #[test]
    fn negotiation_honors_quality_and_prefers_brotli_on_a_tie() {
        assert_eq!(negotiate(&headers("gzip, br")).encoding, Encoding::Brotli);
        assert_eq!(
            negotiate(&headers("br;q=0.4, gzip;q=0.8")).encoding,
            Encoding::Gzip
        );
        assert_eq!(
            negotiate(&headers("br;q=0, gzip;q=0")).encoding,
            Encoding::Identity
        );
        assert_eq!(negotiate(&headers("*;q=0.5")).encoding, Encoding::Brotli);
        assert_eq!(
            negotiate(&headers("br;q=0.2, identity;q=1")).encoding,
            Encoding::Identity
        );
        assert_eq!(
            negotiate(&headers("br;q=0.5, identity;q=0.1")).encoding,
            Encoding::Brotli
        );
        assert_eq!(
            negotiate(&headers("br;q=0, gzip;q=0, identity;q=0")).encoding,
            Encoding::NotAcceptable
        );
        assert_eq!(
            negotiate(&headers("*;Q=0")).encoding,
            Encoding::NotAcceptable
        );
    }

    #[test]
    fn bundle_round_trips_every_representation() {
        let raw = Bytes::from(
            serde_json::to_vec(&serde_json::json!({"teams": vec!["A"; 2000]})).unwrap(),
        );
        let bundle = encode_bundle(raw.clone(), b"test", None).unwrap().bytes;
        let ranges = bundle_ranges(&bundle).unwrap();
        assert!(version_digest(&bundle, &ranges).is_some());
        assert_eq!(&bundle[ranges.raw_start..ranges.raw_end], raw.as_ref());

        let mut gzip = flate2::read::GzDecoder::new(&bundle[ranges.raw_end..ranges.gzip_end]);
        let mut decoded = Vec::new();
        gzip.read_to_end(&mut decoded).unwrap();
        assert_eq!(decoded, raw);
        assert_eq!(identity_body(bundle.clone()).unwrap(), raw);

        let mut brotli = brotli::Decompressor::new(
            Cursor::new(&bundle[ranges.gzip_end..ranges.brotli_end]),
            64 * 1024,
        );
        decoded.clear();
        brotli.read_to_end(&mut decoded).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn legacy_single_replica_raw_entry_stays_identity_encoded() {
        let raw = Bytes::from_static(br#"{"teams":[]}"#);
        assert_eq!(identity_body(raw.clone()).unwrap(), raw);
        let response = response(raw.clone(), &headers("br, gzip")).unwrap();
        assert_eq!(response.headers().get(header::CONTENT_ENCODING), None);
        assert_eq!(response.headers()[header::VARY], "Accept-Encoding");
        assert_eq!(response.body().size_hint().exact(), Some(raw.len() as u64));
    }

    #[test]
    fn v1_bundle_remains_readable_without_a_semantic_validator() {
        let raw = Bytes::from(format!(r#"{{"padding":"{}"}}"#, "a".repeat(8 * 1024)));
        let legacy = encode_legacy_bundle(raw.clone()).unwrap().bytes;

        assert!(valid_bundle(&legacy));
        assert_eq!(identity_body(legacy.clone()).unwrap(), raw);
        let response = response(legacy, &headers("gzip")).unwrap();
        assert_eq!(response.headers()[header::CONTENT_ENCODING], "gzip");
        assert!(!response.headers().contains_key(header::ETAG));
        assert!(!response.headers().contains_key(header::CACHE_CONTROL));
    }

    #[test]
    fn corrupt_bundle_is_not_emitted_as_json() {
        let corrupt = Bytes::from_static(b"RSADENC1broken");
        assert!(response(corrupt, &headers("br")).is_err());
        assert!(response(Bytes::from_static(b"broken"), &headers("br")).is_err());
    }

    #[tokio::test]
    async fn response_negotiates_brotli_without_recompressing() {
        let raw = Bytes::from(format!(
            r#"{{"generatedAt":1,"padding":"{}"}}"#,
            "a".repeat(64 * 1024)
        ));
        let bundle = build_stable_bundle(
            raw.clone(),
            "standard-public-7".to_owned(),
            b"\"generatedAt\":",
        )
        .await
        .unwrap();
        assert!(bundle.cacheable);
        let response = response(bundle.bytes, &headers("gzip;q=0.8, br")).unwrap();
        assert_eq!(response.headers()[header::CONTENT_ENCODING], "br");
        assert_eq!(response.headers()[header::VARY], "Accept-Encoding");
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            CACHE_CONTROL_VALUE
        );
        assert!(response.headers().contains_key(header::ETAG));
        assert!(response.headers().contains_key(SCOREBOARD_VERSION_HEADER));

        let encoded = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(encoded.len() < raw.len() / 10);
        let mut decoder = brotli::Decompressor::new(Cursor::new(encoded), 64 * 1024);
        let mut decoded = Vec::new();
        decoder.read_to_end(&mut decoded).unwrap();
        assert_eq!(decoded, raw);
    }

    #[tokio::test]
    async fn small_board_skips_compression_work() {
        let raw = Bytes::from(format!(r#"{{"padding":"{}"}}"#, "a".repeat(512)));
        let cached = build_bundle(raw.clone()).await.unwrap();
        assert!(cached.cacheable);
        assert_eq!(cached.bytes, raw);
        assert!(bundle_ranges(&cached.bytes).is_none());
    }

    #[tokio::test]
    async fn oversized_board_is_not_cached() {
        let raw = Bytes::from(format!(
            r#"{{"padding":"{}"}}"#,
            "a".repeat(MAX_CACHE_BUNDLE_SIZE)
        ));
        let built = build_bundle(raw.clone()).await.unwrap();
        assert!(!built.cacheable);
        assert_eq!(built.bytes, raw);
    }

    #[tokio::test]
    async fn response_body_has_an_explicit_hard_limit() {
        let raw = Bytes::from(format!(
            r#"{{"padding":"{}"}}"#,
            "a".repeat(MAX_WIRE_REPRESENTATION_SIZE)
        ));
        assert!(build_bundle(raw.clone()).await.is_err());
        assert!(
            !valid_bundle(&raw),
            "legacy raw entries obey the same hard limit"
        );
    }

    #[tokio::test]
    async fn stable_validator_ignores_only_generation_time_and_honors_scope() {
        let first = build_stable_bundle(
            Bytes::from_static(br#"{"generatedAt":100,"score":7}"#),
            "koth-public-9".to_owned(),
            b"\"generatedAt\":",
        )
        .await
        .unwrap();
        let second = build_stable_bundle(
            Bytes::from_static(br#"{"generatedAt":200,"score":7}"#),
            "koth-public-9".to_owned(),
            b"\"generatedAt\":",
        )
        .await
        .unwrap();
        let changed = build_stable_bundle(
            Bytes::from_static(br#"{"generatedAt":200,"score":8}"#),
            "koth-public-9".to_owned(),
            b"\"generatedAt\":",
        )
        .await
        .unwrap();
        let monitor = build_stable_bundle(
            Bytes::from_static(br#"{"generatedAt":200,"score":7}"#),
            "koth-monitor-9".to_owned(),
            b"\"generatedAt\":",
        )
        .await
        .unwrap();
        let etag = |bundle: &Bytes| {
            let ranges = bundle_ranges(bundle).unwrap();
            etag_for(bundle, &ranges, None).unwrap().0
        };
        assert_eq!(etag(&first.bytes), etag(&second.bytes));
        assert_ne!(etag(&second.bytes), etag(&changed.bytes));
        assert_ne!(etag(&second.bytes), etag(&monitor.bytes));
    }

    #[tokio::test]
    async fn shared_bundle_validator_is_salted_by_authorization_view() {
        let built = build_stable_bundle(
            Bytes::from_static(br#"{"generatedAt":100,"score":7}"#),
            "koth-live-9".to_owned(),
            b"\"generatedAt\":",
        )
        .await
        .unwrap();
        let public =
            scoped_response(built.bytes.clone(), &headers("identity"), "koth-public").unwrap();
        let monitor = scoped_response(built.bytes, &headers("identity"), "koth-monitor").unwrap();
        assert_ne!(
            public.headers()[header::ETAG],
            monitor.headers()[header::ETAG]
        );
        assert_ne!(
            public.headers()[SCOREBOARD_VERSION_HEADER],
            monitor.headers()[SCOREBOARD_VERSION_HEADER]
        );
    }

    #[tokio::test]
    async fn matching_validator_returns_empty_304_without_negotiation_work() {
        let built = build_stable_bundle(
            Bytes::from_static(br#"{"updateTimeUtc":100,"items":[]}"#),
            "standard-public-7".to_owned(),
            b"\"updateTimeUtc\":",
        )
        .await
        .unwrap();
        let initial = response(built.bytes.clone(), &headers("gzip, br")).unwrap();
        let etag = initial.headers()[header::ETAG].clone();
        let version = initial.headers()[SCOREBOARD_VERSION_HEADER].clone();
        let mut conditional = headers("br;q=0, gzip;q=0, identity;q=0");
        conditional.insert(header::IF_NONE_MATCH, etag.clone());
        let unchanged = response(built.bytes, &conditional).unwrap();
        assert_eq!(unchanged.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(unchanged.headers()[header::ETAG], etag);
        assert_eq!(unchanged.headers()[SCOREBOARD_VERSION_HEADER], version);
        assert_eq!(unchanged.body().size_hint().exact(), Some(0));
    }

    #[test]
    fn forbidden_identity_on_a_legacy_entry_returns_not_acceptable() {
        let raw = Bytes::from_static(br#"{"teams":[]}"#);
        let response = response(raw, &headers("br, identity;q=0")).unwrap();
        assert_eq!(response.status(), StatusCode::NOT_ACCEPTABLE);
        assert_eq!(response.headers()[header::VARY], "Accept-Encoding");
    }
}
