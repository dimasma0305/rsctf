use std::io::Write;

use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::{BufMut, Bytes, BytesMut};

use crate::utils::error::{AppError, AppResult};

const MAGIC: &[u8; 8] = b"RSADENC1";
const HEADER_LEN: usize = MAGIC.len() + 3 * size_of::<u32>();
const MIN_COMPRESSION_SIZE: usize = 4 * 1024;
const MAX_CACHE_BUNDLE_SIZE: usize = 4 * 1024 * 1024;

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
    if bytes.get(..MAGIC.len())? != MAGIC {
        return None;
    }
    let raw_len = read_len(bytes, MAGIC.len())?;
    let gzip_len = read_len(bytes, MAGIC.len() + size_of::<u32>())?;
    let brotli_len = read_len(bytes, MAGIC.len() + 2 * size_of::<u32>())?;
    let raw_start = HEADER_LEN;
    let raw_end = raw_start.checked_add(raw_len)?;
    let gzip_end = raw_end.checked_add(gzip_len)?;
    let brotli_end = gzip_end.checked_add(brotli_len)?;
    (brotli_end == bytes.len()).then_some(BundleRanges {
        raw_start,
        raw_end,
        gzip_end,
        brotli_end,
    })
}

/// Validate either the current atomic encoding bundle or a legacy raw JSON
/// body before it is served from cache. Keeping this check independent of
/// content negotiation lets stale-while-revalidate reject corrupt fallback
/// entries without allocating a response first.
pub(super) fn valid_bundle(bytes: &[u8]) -> bool {
    bundle_ranges(bytes).map_or_else(
        || bytes.first() == Some(&b'{'),
        |ranges| bytes.get(ranges.raw_start) == Some(&b'{'),
    )
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

fn encode_bundle(raw: Bytes) -> AppResult<BuiltBoardBody> {
    if raw.len() > MAX_CACHE_BUNDLE_SIZE {
        return Ok(BuiltBoardBody {
            bytes: raw,
            cacheable: false,
        });
    }
    let (gzip, brotli) = compress(&raw).map_err(|error| AppError::internal(error.to_string()))?;
    let raw_len = u32::try_from(raw.len())
        .map_err(|_| AppError::internal("A&D scoreboard exceeds the cache bundle limit"))?;
    let gzip_len = u32::try_from(gzip.len())
        .map_err(|_| AppError::internal("A&D gzip body exceeds the cache bundle limit"))?;
    let brotli_len = u32::try_from(brotli.len())
        .map_err(|_| AppError::internal("A&D Brotli body exceeds the cache bundle limit"))?;
    let capacity = HEADER_LEN
        .checked_add(raw.len())
        .and_then(|size| size.checked_add(gzip.len()))
        .and_then(|size| size.checked_add(brotli.len()))
        .ok_or_else(|| AppError::internal("A&D scoreboard cache bundle is too large"))?;
    if capacity > MAX_CACHE_BUNDLE_SIZE {
        return Ok(BuiltBoardBody {
            bytes: raw,
            cacheable: true,
        });
    }
    let mut bundle = BytesMut::with_capacity(capacity);
    bundle.extend_from_slice(MAGIC);
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

/// Build all negotiated representations once per cacheable scoreboard version.
/// Compression is deliberately off the Tokio worker threads. Oversized bodies
/// remain serviceable as uncached identity responses.
pub(super) async fn build_bundle(raw: Bytes) -> AppResult<BuiltBoardBody> {
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
    tokio::task::spawn_blocking(move || encode_bundle(raw))
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
}

/// Select a zero-copy body slice from the atomic cache bundle. A legacy raw JSON
/// entry can remain for the five-second TTL during an in-place single-replica
/// deployment. Mixed old/new application replicas are not supported by the
/// repository's compose topology because old binaries cannot decode bundles.
pub(super) fn response(bundle: Bytes, headers: &HeaderMap) -> AppResult<Response> {
    let requested = negotiate(headers);
    let ranges = bundle_ranges(&bundle);
    if !valid_bundle(&bundle) {
        return Err(AppError::internal(
            "Corrupt A&D scoreboard cache bundle; retry after cache expiry",
        ));
    }
    if requested.encoding == Encoding::NotAcceptable {
        let mut response = StatusCode::NOT_ACCEPTABLE.into_response();
        response
            .headers_mut()
            .insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
        return Ok(response);
    }
    let (body, encoding) = match ranges {
        Some(ranges) => match requested.encoding {
            Encoding::Brotli => (bundle.slice(ranges.gzip_end..ranges.brotli_end), Some("br")),
            Encoding::Gzip => (bundle.slice(ranges.raw_end..ranges.gzip_end), Some("gzip")),
            Encoding::Identity => (bundle.slice(ranges.raw_start..ranges.raw_end), None),
            Encoding::NotAcceptable => unreachable!(),
        },
        None if requested.identity_allowed => (bundle, None),
        None => {
            let mut response = StatusCode::NOT_ACCEPTABLE.into_response();
            response
                .headers_mut()
                .insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
            return Ok(response);
        }
    };
    let mut response = body.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
        .headers_mut()
        .insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
    if let Some(encoding) = encoding {
        response
            .headers_mut()
            .insert(header::CONTENT_ENCODING, HeaderValue::from_static(encoding));
    }
    Ok(response)
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
            serde_json::to_vec(&serde_json::json!({"teams": vec!["A"; 1000]})).unwrap(),
        );
        let bundle = encode_bundle(raw.clone()).unwrap().bytes;
        let ranges = bundle_ranges(&bundle).unwrap();
        assert_eq!(&bundle[ranges.raw_start..ranges.raw_end], raw.as_ref());

        let mut gzip = flate2::read::GzDecoder::new(&bundle[ranges.raw_end..ranges.gzip_end]);
        let mut decoded = Vec::new();
        gzip.read_to_end(&mut decoded).unwrap();
        assert_eq!(decoded, raw);

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
        let response = response(raw.clone(), &headers("br, gzip")).unwrap();
        assert_eq!(response.headers().get(header::CONTENT_ENCODING), None);
        assert_eq!(response.headers()[header::VARY], "Accept-Encoding");
        assert_eq!(response.body().size_hint().exact(), Some(raw.len() as u64));
    }

    #[test]
    fn corrupt_bundle_is_not_emitted_as_json() {
        let corrupt = Bytes::from_static(b"RSADENC1broken");
        assert!(response(corrupt, &headers("br")).is_err());
        assert!(response(Bytes::from_static(b"broken"), &headers("br")).is_err());
    }

    #[tokio::test]
    async fn response_negotiates_brotli_without_recompressing() {
        let raw = Bytes::from(format!(r#"{{"padding":"{}"}}"#, "a".repeat(64 * 1024)));
        let bundle = build_bundle(raw.clone()).await.unwrap();
        assert!(bundle.cacheable);
        let response = response(bundle.bytes, &headers("gzip;q=0.8, br")).unwrap();
        assert_eq!(response.headers()[header::CONTENT_ENCODING], "br");
        assert_eq!(response.headers()[header::VARY], "Accept-Encoding");

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
        let raw = Bytes::from(vec![b'a'; MIN_COMPRESSION_SIZE - 1]);
        let cached = build_bundle(raw.clone()).await.unwrap();
        assert!(cached.cacheable);
        assert_eq!(cached.bytes, raw);
        assert!(bundle_ranges(&cached.bytes).is_none());
    }

    #[tokio::test]
    async fn oversized_board_is_not_cached() {
        let raw = Bytes::from(vec![b'a'; MAX_CACHE_BUNDLE_SIZE + 1]);
        let built = build_bundle(raw.clone()).await.unwrap();
        assert!(!built.cacheable);
        assert_eq!(built.bytes, raw);
    }

    #[test]
    fn forbidden_identity_on_a_legacy_entry_returns_not_acceptable() {
        let raw = Bytes::from_static(br#"{"teams":[]}"#);
        let response = response(raw, &headers("br, identity;q=0")).unwrap();
        assert_eq!(response.status(), StatusCode::NOT_ACCEPTABLE);
        assert_eq!(response.headers()[header::VARY], "Accept-Encoding");
    }
}
