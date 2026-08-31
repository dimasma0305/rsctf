//! Ported from RSCTF `Server.cs` — assembles the HTTP router by merging every
//! controller and hub, then applies cross-cutting middleware.

use std::path::Path;

use axum::extract::MatchedPath;
use axum::http::{header, HeaderValue, Request};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::Router;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::timeout::RequestBodyDeadlineLayer;
use tower_http::trace::{MakeSpan, TraceLayer};

use crate::app_state::SharedState;
use crate::{controllers, hubs};

const UNMATCHED_TRACE_ROUTE: &str = "<unmatched>";
const REQUEST_ID_HEADER: &str = "x-rsctf-request-id";

/// Builds bounded-cardinality request spans without copying raw URI path or
/// query data into logs. Some process-local routes contain bearer capabilities
/// in path parameters, so only Axum's route template is safe to record.
#[derive(Clone, Copy)]
struct RedactedHttpMakeSpan;

impl<B> MakeSpan<B> for RedactedHttpMakeSpan {
    fn make_span(&mut self, request: &Request<B>) -> tracing::Span {
        tracing::debug_span!(
            "request",
            method = %request.method(),
            route = trace_route(request),
            request_id = trace_request_id(request).unwrap_or("<none>"),
            version = ?request.version(),
        )
    }
}

fn trace_request_id<B>(request: &Request<B>) -> Option<&str> {
    let value = request.headers().get(REQUEST_ID_HEADER)?.to_str().ok()?;
    if request.method() != axum::http::Method::GET {
        return None;
    }
    let segments: Vec<_> = request
        .uri()
        .path()
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    let prefix = match segments.as_slice() {
        ["api", "game", game_id, "challenges", challenge_id]
            if game_id.parse::<i32>().is_ok() && challenge_id.parse::<i32>().is_ok() =>
        {
            "challenge-challenge-"
        }
        ["api", "game", game_id, "challenges", challenge_id, "solvers", "page"]
            if game_id.parse::<i32>().is_ok() && challenge_id.parse::<i32>().is_ok() =>
        {
            "challenge-solvers-"
        }
        _ => return None,
    };
    let suffix = value.strip_prefix(prefix)?;
    (suffix.len() == 36 && uuid::Uuid::parse_str(suffix).is_ok()).then_some(value)
}

async fn echo_request_id(request: Request<axum::body::Body>, next: Next) -> Response {
    let request_id = trace_request_id(&request).and_then(|value| HeaderValue::from_str(value).ok());
    let mut response = next.run(request).await;
    if let Some(request_id) = request_id {
        response.headers_mut().insert(REQUEST_ID_HEADER, request_id);
    }
    response
}

fn trace_route<B>(request: &Request<B>) -> &str {
    request
        .extensions()
        .get::<MatchedPath>()
        .map_or(UNMATCHED_TRACE_ROUTE, MatchedPath::as_str)
}

/// The merged application routes, without state applied. Constructing this
/// runs every controller's route registration, so route conflicts surface
/// here (this is what the router integration test exercises).
fn common_api_router(game_router: Router<SharedState>) -> Router<SharedState> {
    Router::new()
        .route("/livez", get(crate::services::health::liveness))
        .route("/healthz", get(crate::services::health::readiness))
        .route("/_rsctf/anti-autofill.js", get(anti_autofill_script))
        // --- controllers (mirror RSCTF Controllers/) ---
        .merge(controllers::account::router())
        .merge(controllers::team::router())
        .merge(game_router)
        .merge(controllers::edit::router())
        .merge(controllers::admin::router())
        .merge(controllers::info::router())
        .merge(controllers::donations::router())
        .merge(controllers::assets::router())
        .merge(controllers::api_token::router())
        .merge(controllers::exercise::router())
        .merge(controllers::honeypot::router())
        .merge(controllers::oauth::router())
        .merge(controllers::workers::public_router())
        // --- realtime hubs (SignalR; mirror RSCTF Hubs/) ---
        .merge(hubs::monitor::router())
        .merge(hubs::user::router())
        .merge(hubs::admin::router())
        .merge(hubs::attack::router())
}

pub fn api_router() -> Router<SharedState> {
    common_api_router(controllers::game::router())
        .merge(controllers::event_security::router())
        .merge(controllers::workers::router())
        .merge(controllers::proxy::router())
        .merge(hubs::container::router())
}

/// Stateless public API. Process-local BYOC and container-exec routes are
/// deliberately absent so a load-balancer mistake cannot create split-brain
/// tunnel state on a web replica.
pub fn web_api_router() -> Router<SharedState> {
    common_api_router(controllers::game::web_router())
}

/// Narrow HTTP surface for the privileged singleton network/control owner.
/// Reverse proxies route BYOC agent/image traffic, the container-exec hub, and
/// explicit lifecycle-recovery mutations here; ordinary APIs remain exclusive
/// to the scalable web pool.
pub fn stateful_api_router() -> Router<SharedState> {
    Router::new()
        .route("/livez", get(crate::services::health::liveness))
        .route("/healthz", get(crate::services::health::readiness))
        .merge(controllers::event_security::router())
        .merge(controllers::game::ad::stateful_router())
        .merge(controllers::game::koth::stateful_router())
        .merge(controllers::workers::router())
        .merge(controllers::proxy::router())
        .merge(hubs::container::router())
}

pub fn build_router(state: SharedState) -> Router {
    finish_router(api_router(), state, true)
}

pub fn build_web_router(state: SharedState) -> Router {
    finish_router(web_api_router(), state, true)
}

pub fn build_stateful_router(state: SharedState) -> Router {
    finish_router(stateful_api_router(), state, false)
}

fn finish_router(app: Router<SharedState>, state: SharedState, serve_frontend: bool) -> Router {
    // Reserve API and hub namespaces before the SPA/static fallback is attached.
    // A miss there must remain a typed transport failure, never HTTP 200 HTML.
    let app = app.merge(typed_namespace_fallbacks());
    // Serve the built React frontend. When a static directory exists, unmatched
    // routes fall back to its index document so client-side deep links also work
    // after a browser refresh. The web/ client builds to web/build via pnpm.
    let static_dir = std::env::var("RSCTF_STATIC_DIR").unwrap_or_else(|_| "web/build".to_string());
    let app = if serve_frontend && Path::new(&static_dir).is_dir() {
        let index = format!("{static_dir}/index.html");
        tracing::info!("serving frontend from {static_dir}");
        // Serve index.html (the SPA shell + all deep links) through a handler that
        // injects a tiny anti-autofill script, so the browser's password manager
        // stops autofilling the /admin/settings secret fields (which lack
        // autocomplete attrs in the React client). Real asset files
        // (js/css/…) are still served directly by ServeDir; only the HTML shell is
        // rewritten. Falls back to the raw file if it can't be read at startup.
        let injected = std::fs::read_to_string(&index)
            .ok()
            .map(|html| inject_head(&html, ANTI_AUTOFILL_TAG));
        let spa: axum::routing::MethodRouter = match injected {
            Some(html) => axum::routing::get(move || {
                let html = html.clone();
                async move { axum::response::Html(html) }
            }),
            None => axum::routing::get_service(ServeFile::new(index.clone())),
        };
        app.fallback_service(
            ServeDir::new(&static_dir)
                .append_index_html_on_directories(false)
                .fallback(spa),
        )
    } else {
        app
    };

    // Apply cross-cutting layers after registering the SPA fallback. Axum layers
    // only routes that exist at the time `layer` is called, so this ordering keeps
    // HSTS, frame denial, and the CSP on the HTML shell as well as API responses.
    app.layer(axum::middleware::from_fn_with_state(
        state.clone(),
        crate::middlewares::event_vpn::middleware,
    ))
    // A per-field size limit does not stop a slow client from retaining an
    // almost-complete multipart buffer forever. Cap total body transfer time;
    // buffered upload handlers also take weighted admission permits.
    .layer(RequestBodyDeadlineLayer::new(
        crate::utils::upload::REQUEST_BODY_DEADLINE,
    ))
    // Per-request user-activity stamp (RSCTF's UserInfo.UpdateByHttpContext) —
    // inside the rate limiter, so activity is not stamped for throttled 429s.
    .layer(axum::middleware::from_fn_with_state(
        state.clone(),
        crate::middlewares::user_activity::middleware,
    ))
    .layer(TraceLayer::new_for_http().make_span_with(RedactedHttpMakeSpan))
    .layer(axum::middleware::from_fn_with_state(
        state.clone(),
        crate::middlewares::rate_limiter::global_middleware,
    ))
    .layer(axum::middleware::from_fn_with_state(
        state.clone(),
        crate::middlewares::request_security::csrf_middleware,
    ))
    .layer(axum::middleware::from_fn(
        crate::middlewares::request_security::security_headers,
    ))
    .layer(axum::middleware::from_fn_with_state(
        state.clone(),
        crate::services::health::reject_new_work_while_draining,
    ))
    // Echo wraps admission/draining so even an early 429/503 retains the safe
    // browser reference. Admitted requests record the same value in their
    // redacted trace span above.
    .layer(axum::middleware::from_fn(echo_request_id))
    .with_state(state)
}

fn typed_namespace_fallbacks<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api", any(unmatched_api_route))
        .route("/api/{*path}", any(unmatched_api_route))
        .route("/hub", any(unmatched_hub_route))
        .route("/hub/{*path}", any(unmatched_hub_route))
}

async fn unmatched_api_route() -> Response {
    crate::utils::error::AppError::not_found("API route not found").into_response()
}

async fn unmatched_hub_route() -> Response {
    crate::utils::error::AppError::not_found("Hub route not found").into_response()
}

/// Minimal HTTP surface for a background-only engine replica. Keeping health
/// probes on the same configured bind address gives orchestrators liveness and
/// graceful-drain visibility without accidentally exposing application routes
/// from a worker that is not intended to receive user traffic.
pub fn build_health_router(state: SharedState) -> Router {
    Router::new()
        .route("/livez", get(crate::services::health::liveness))
        .route("/healthz", get(crate::services::health::readiness))
        .layer(TraceLayer::new_for_http().make_span_with(RedactedHttpMakeSpan))
        .with_state(state)
}

/// Insert `snippet` just before `</head>` (or prepend if there's no head tag).
fn inject_head(html: &str, snippet: &str) -> String {
    match html.find("</head>") {
        Some(i) => format!("{}{}{}", &html[..i], snippet, &html[i..]),
        None => format!("{snippet}{html}"),
    }
}

/// Disables password-manager autofill on the /admin/settings secret inputs (which
/// The client renders without autocomplete attrs). Scoped to that route so the
/// login page's autofill keeps working; a MutationObserver re-applies it across the
/// SPA's client-side navigations and React re-renders.
const ANTI_AUTOFILL_TAG: &str = r#"<script src="/_rsctf/anti-autofill.js" defer></script>"#;
const ANTI_AUTOFILL_SCRIPT: &str = r#"(function(){function h(){if(!/^\/admin\/settings/.test(location.pathname))return;document.querySelectorAll("input:not([data-noaf])").forEach(function(e){var t=(e.getAttribute("type")||"").toLowerCase(),n=e.getAttribute("name")||"",d=e.id||"";if(t==="password"||/pass|secret|key|token/i.test(n+" "+d)){e.setAttribute("autocomplete","new-password");e.setAttribute("data-noaf","1")}})}try{new MutationObserver(h).observe(document.documentElement,{childList:!0,subtree:!0})}catch(e){}document.addEventListener("DOMContentLoaded",h);window.addEventListener("load",h);h()})();"#;

async fn anti_autofill_script() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        ANTI_AUTOFILL_SCRIPT,
    )
}

#[cfg(test)]
mod tests {
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::Router;
    use tower::ServiceExt;

    use super::{
        anti_autofill_script, echo_request_id, inject_head, trace_request_id, trace_route,
        typed_namespace_fallbacks, ANTI_AUTOFILL_SCRIPT, ANTI_AUTOFILL_TAG, REQUEST_ID_HEADER,
        UNMATCHED_TRACE_ROUTE,
    };

    const BYOC_IMAGE_ROUTE: &str =
        "/api/game/{game}/ad/byoc/{participation}/{challenge}/image/{token}";

    async fn traced_route(request: Request<Body>) -> String {
        trace_route(&request).to_owned()
    }

    #[tokio::test]
    async fn byoc_capability_is_replaced_with_the_matched_route_template() {
        const SECRET: &str = "fake-capability-token-must-not-be-logged";
        let app = Router::new().route(BYOC_IMAGE_ROUTE, get(traced_route));
        let request = Request::builder()
            .uri(format!(
                "/api/game/7/ad/byoc/11/13/image/{SECRET}?download=secret"
            ))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        let route = std::str::from_utf8(&body).unwrap();

        assert_eq!(route, BYOC_IMAGE_ROUTE);
        assert!(!route.contains(SECRET));
        assert!(!route.contains("download"));
    }

    #[tokio::test]
    async fn fallback_does_not_log_an_unmatched_path_or_query() {
        const SECRET: &str = "unmatched-path-secret";
        let app = Router::new().fallback(traced_route);
        let request = Request::builder()
            .uri(format!("/missing/{SECRET}?token=query-secret"))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        let route = std::str::from_utf8(&body).unwrap();

        assert_eq!(route, UNMATCHED_TRACE_ROUTE);
        assert!(!route.contains(SECRET));
        assert!(!route.contains("query-secret"));
    }

    #[tokio::test]
    async fn safe_client_request_identity_is_echoed_for_support_correlation() {
        let app = Router::new()
            .route(
                "/api/game/{id}/challenges/{challenge_id}/solvers/page",
                get(|| async { "ok" }),
            )
            .layer(axum::middleware::from_fn(echo_request_id));
        let request = Request::builder()
            .uri("/api/game/7/challenges/11/solvers/page")
            .header(
                REQUEST_ID_HEADER,
                "challenge-solvers-018f47d2-0c9a-4b31-8d1f-a1976639466f",
            )
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            trace_request_id(&request),
            Some("challenge-solvers-018f47d2-0c9a-4b31-8d1f-a1976639466f")
        );
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(
            response.headers().get(REQUEST_ID_HEADER),
            Some(&axum::http::HeaderValue::from_static(
                "challenge-solvers-018f47d2-0c9a-4b31-8d1f-a1976639466f"
            ))
        );
    }

    #[test]
    fn unsafe_request_identity_is_never_logged_or_echoed() {
        for (uri, value) in [
            ("/api/game/7/challenges/11", "short"),
            ("/api/game/7/challenges/11", "contains space"),
            ("/api/game/7/challenges/11", "secret/bearer?query"),
            (
                "/api/game/7/challenges/11",
                "safe-but-unscoped-018f47d2-0c9a-4b31-8d1f-a1976639466f",
            ),
            (
                "/api/profile",
                "challenge-challenge-018f47d2-0c9a-4b31-8d1f-a1976639466f",
            ),
            (
                "/api/game/7/challenges/11/solvers/page",
                "challenge-challenge-018f47d2-0c9a-4b31-8d1f-a1976639466f",
            ),
        ] {
            let request = Request::builder()
                .uri(uri)
                .header(REQUEST_ID_HEADER, value)
                .body(Body::empty())
                .unwrap();
            assert_eq!(trace_request_id(&request), None);
        }
    }

    #[test]
    fn spa_injection_uses_an_external_csp_compatible_script() {
        let html = inject_head("<head><title>x</title></head>", ANTI_AUTOFILL_TAG);
        assert!(html.contains(r#"src="/_rsctf/anti-autofill.js""#));
        assert!(!html.contains(ANTI_AUTOFILL_SCRIPT));
    }

    #[tokio::test]
    async fn anti_autofill_script_has_a_javascript_content_type() {
        let response = anti_autofill_script().await.into_response();
        assert_eq!(
            response.headers().get(axum::http::header::CONTENT_TYPE),
            Some(&axum::http::HeaderValue::from_static(
                "text/javascript; charset=utf-8"
            ))
        );
    }

    #[tokio::test]
    async fn unmatched_api_and_hub_routes_return_typed_json_404s() {
        let app = Router::new()
            .route("/api/known", get(|| async { "known" }))
            .merge(typed_namespace_fallbacks())
            .fallback(|| async { axum::response::Html("<!doctype html><title>SPA</title>") });

        for (method, path) in [
            (axum::http::Method::GET, "/api/missing"),
            (axum::http::Method::POST, "/api/missing"),
            (axum::http::Method::GET, "/hub/missing"),
            (axum::http::Method::POST, "/hub/missing"),
            (axum::http::Method::GET, "/api"),
            (axum::http::Method::GET, "/hub"),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);
            assert_eq!(
                response.headers().get(axum::http::header::CONTENT_TYPE),
                Some(&axum::http::HeaderValue::from_static("application/json"))
            );
            let body = to_bytes(response.into_body(), 1024).await.unwrap();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["status"], 404);
            assert!(json["title"]
                .as_str()
                .is_some_and(|title| !title.is_empty()));
        }

        let known = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/known")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(known.status(), axum::http::StatusCode::OK);

        let spa = app
            .oneshot(
                Request::builder()
                    .uri("/games/1/challenges")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(spa.status(), axum::http::StatusCode::OK);
        assert_eq!(
            spa.headers().get(axum::http::header::CONTENT_TYPE),
            Some(&axum::http::HeaderValue::from_static(
                "text/html; charset=utf-8"
            ))
        );
    }
}
