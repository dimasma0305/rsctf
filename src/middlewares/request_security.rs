//! Browser request-boundary protections for the cookie-authenticated SPA.

use axum::extract::{Request, State};
use axum::http::header::{CONTENT_SECURITY_POLICY, COOKIE, HOST, ORIGIN, REFERER};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::SESSION_COOKIE;
use crate::utils::error::AppError;

const CSP_VALUE: &str = "base-uri 'self'; frame-ancestors 'none'; object-src 'none'; \
    script-src 'self' https://challenges.cloudflare.com; \
    worker-src 'self' blob:; frame-src https://challenges.cloudflare.com";

/// Reject cross-origin browser mutations and cookie-authenticated WebSocket opens.
/// Bearer clients are not CSRF-prone because browsers do not attach their token
/// automatically, so command-line and automation clients remain origin-independent.
pub async fn csrf_middleware(State(st): State<SharedState>, req: Request, next: Next) -> Response {
    if csrf_violation(
        &req,
        st.config.public_url.as_deref(),
        st.config.cookie_secure,
    ) {
        return AppError::Forbidden.into_response();
    }
    next.run(req).await
}

fn csrf_violation(req: &Request, public_url: Option<&str>, cookie_secure: bool) -> bool {
    needs_origin_check(req)
        && has_session_cookie(req.headers())
        && !same_origin(req.headers(), public_url, cookie_secure)
}

/// Add headers that are safe for both API responses and the same-origin SPA.
pub async fn security_headers(req: Request, next: Next) -> Response {
    let mut response = next.run(req).await;
    let headers = response.headers_mut();
    headers.insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    );
    headers.insert(
        HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    headers.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    headers.insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP_VALUE));
    headers.insert(
        HeaderName::from_static("strict-transport-security"),
        HeaderValue::from_static("max-age=31536000"),
    );
    response
}

fn needs_origin_check(req: &Request) -> bool {
    !matches!(
        *req.method(),
        Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE
    ) || is_websocket_upgrade(req.headers())
}

fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get("upgrade")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
}

fn has_session_cookie(headers: &HeaderMap) -> bool {
    let Some(cookies) = headers.get(COOKIE).and_then(|value| value.to_str().ok()) else {
        return false;
    };
    cookies.split(';').any(|pair| {
        pair.trim()
            .split_once('=')
            .is_some_and(|(name, value)| name == SESSION_COOKIE && !value.is_empty())
    })
}

#[derive(Debug, PartialEq, Eq)]
struct NormalizedOrigin {
    scheme: String,
    authority: String,
}

pub(crate) fn same_origin(
    headers: &HeaderMap,
    public_url: Option<&str>,
    cookie_secure: bool,
) -> bool {
    let expected = match public_url {
        Some(public_url) => parse_origin(public_url),
        None => headers
            .get(HOST)
            .and_then(|value| value.to_str().ok())
            .and_then(normalize_authority)
            .map(|authority| NormalizedOrigin {
                scheme: if cookie_secure { "https" } else { "http" }.to_string(),
                authority,
            }),
    };
    let Some(expected) = expected else {
        return false;
    };

    let source = headers
        .get(ORIGIN)
        .or_else(|| headers.get(REFERER))
        .and_then(|value| value.to_str().ok());
    let Some(source) = source else {
        return false;
    };
    if source.eq_ignore_ascii_case("null") {
        return false;
    }

    parse_origin(source).is_some_and(|source| source == expected)
}

fn parse_origin(value: &str) -> Option<NormalizedOrigin> {
    let uri = value.parse::<Uri>().ok()?;
    let scheme = uri.scheme_str()?.to_ascii_lowercase();
    if !matches!(scheme.as_str(), "http" | "https") {
        return None;
    }
    let authority = normalize_authority(uri.authority()?.as_str())?;
    Some(NormalizedOrigin { scheme, authority })
}

fn normalize_authority(value: &str) -> Option<String> {
    let authority = value.trim().trim_end_matches('.').to_ascii_lowercase();
    if authority.is_empty() || authority.contains(['/', '\\', '@']) {
        return None;
    }
    Some(authority)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::header::AUTHORIZATION;

    fn headers(host: &str, origin: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, host.parse().unwrap());
        if let Some(origin) = origin {
            headers.insert(ORIGIN, origin.parse().unwrap());
        }
        headers
    }

    #[test]
    fn same_origin_requires_matching_scheme_and_authority() {
        let secure = headers("ctf.example:8443", Some("https://ctf.example:8443"));
        assert!(same_origin(&secure, None, true));
        assert!(!same_origin(
            &headers("ctf.example:8443", Some("http://ctf.example:8443")),
            None,
            true
        ));
        assert!(same_origin(
            &headers("localhost:8080", Some("http://localhost:8080")),
            None,
            false
        ));
        assert!(!same_origin(
            &headers("ctf.example", Some("https://evil.ctf.example")),
            None,
            true
        ));
        assert!(!same_origin(
            &headers("ctf.example", Some("null")),
            None,
            true
        ));
        assert!(!same_origin(&headers("ctf.example", None), None, true));
    }

    #[test]
    fn configured_public_origin_is_authoritative() {
        let proxied = headers("rsctf:8080", Some("https://ctf.example/account"));
        assert!(same_origin(
            &proxied,
            Some("https://ctf.example/base"),
            true
        ));
        assert!(!same_origin(&proxied, Some("http://ctf.example"), false));
    }

    #[test]
    fn csrf_cookie_parser_accepts_only_the_rsctf_cookie() {
        let mut headers = HeaderMap::new();
        headers.insert(
            COOKIE,
            HeaderValue::from_static("Other=x; RSCTF_Token=session"),
        );
        assert!(has_session_cookie(&headers));
        headers.insert(
            COOKIE,
            HeaderValue::from_static("Other=x; Unknown_Token=stale-session"),
        );
        assert!(!has_session_cookie(&headers));
        headers.insert(COOKIE, HeaderValue::from_static("NotRSCTF_Token=session"));
        assert!(!has_session_cookie(&headers));
    }

    #[test]
    fn bearer_shape_does_not_bypass_cookie_csrf_check() {
        let request = Request::builder()
            .method(Method::POST)
            .header(HOST, "ctf.example")
            .header(ORIGIN, "https://evil.example")
            .header(COOKIE, "RSCTF_Token=session")
            .header(AUTHORIZATION, "Bearer syntactically-nonempty")
            .body(Body::empty())
            .unwrap();
        assert!(csrf_violation(&request, None, true));

        let bearer_only = Request::builder()
            .method(Method::POST)
            .header(AUTHORIZATION, "Bearer api-token")
            .body(Body::empty())
            .unwrap();
        assert!(!csrf_violation(&bearer_only, None, true));
    }

    #[test]
    fn csp_blocks_inline_and_untrusted_scripts_without_breaking_captcha_workers() {
        assert!(CSP_VALUE.contains("script-src 'self' https://challenges.cloudflare.com"));
        assert!(CSP_VALUE.contains("worker-src 'self' blob:"));
        assert!(CSP_VALUE.contains("frame-src https://challenges.cloudflare.com"));
        assert!(!CSP_VALUE.contains("'unsafe-inline'"));
        assert!(!CSP_VALUE.contains("'unsafe-eval'"));
    }
}
