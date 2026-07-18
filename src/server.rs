//! Ported from RSCTF `Server.cs` — assembles the HTTP router by merging every
//! controller and hub, then applies cross-cutting middleware.

use std::path::Path;

use axum::routing::get;
use axum::Router;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

use crate::app_state::SharedState;
use crate::{controllers, hubs};

/// The merged application routes, without state applied. Constructing this
/// runs every controller's route registration, so route conflicts surface
/// here (this is what the router integration test exercises).
fn common_api_router(game_router: Router<SharedState>) -> Router<SharedState> {
    Router::new()
        .route("/livez", get(crate::services::health::liveness))
        .route("/healthz", get(crate::services::health::readiness))
        // --- controllers (mirror RSCTF Controllers/) ---
        .merge(controllers::account::router())
        .merge(controllers::team::router())
        .merge(game_router)
        .merge(controllers::edit::router())
        .merge(controllers::admin::router())
        .merge(controllers::info::router())
        .merge(controllers::assets::router())
        .merge(controllers::api_token::router())
        .merge(controllers::exercise::router())
        .merge(controllers::honeypot::router())
        .merge(controllers::oauth::router())
        // --- realtime hubs (SignalR; mirror RSCTF Hubs/) ---
        .merge(hubs::monitor::router())
        .merge(hubs::user::router())
        .merge(hubs::admin::router())
        .merge(hubs::attack::router())
}

pub fn api_router() -> Router<SharedState> {
    common_api_router(controllers::game::router())
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
/// Reverse proxies route only BYOC agent/image traffic and the container-exec
/// hub here; all ordinary APIs remain exclusive to the scalable web pool.
pub fn stateful_api_router() -> Router<SharedState> {
    Router::new()
        .route("/livez", get(crate::services::health::liveness))
        .route("/healthz", get(crate::services::health::readiness))
        .merge(controllers::game::ad::stateful_router())
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
            .map(|html| inject_head(&html, ANTI_AUTOFILL_SCRIPT));
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
    app
        // Per-request user-activity stamp (RSCTF's UserInfo.UpdateByHttpContext) —
        // inside the rate limiter, so activity is not stamped for throttled 429s.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middlewares::user_activity::middleware,
        ))
        .layer(TraceLayer::new_for_http())
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
        .with_state(state)
}

/// Minimal HTTP surface for a background-only engine replica. Keeping health
/// probes on the same configured bind address gives orchestrators liveness and
/// graceful-drain visibility without accidentally exposing application routes
/// from a worker that is not intended to receive user traffic.
pub fn build_health_router(state: SharedState) -> Router {
    Router::new()
        .route("/livez", get(crate::services::health::liveness))
        .route("/healthz", get(crate::services::health::readiness))
        .layer(TraceLayer::new_for_http())
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
const ANTI_AUTOFILL_SCRIPT: &str = r#"<script>(function(){function h(){if(!/^\/admin\/settings/.test(location.pathname))return;document.querySelectorAll("input:not([data-noaf])").forEach(function(e){var t=(e.getAttribute("type")||"").toLowerCase(),n=e.getAttribute("name")||"",d=e.id||"";if(t==="password"||/pass|secret|key|token/i.test(n+" "+d)){e.setAttribute("autocomplete","new-password");e.setAttribute("data-noaf","1")}})}try{new MutationObserver(h).observe(document.documentElement,{childList:!0,subtree:!0})}catch(e){}document.addEventListener("DOMContentLoaded",h);window.addEventListener("load",h);h()})();</script>"#;
