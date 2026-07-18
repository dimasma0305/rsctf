//! controllers/honeypot.rs — ported from RSCTF `Controllers/HoneypotController.cs`.
//!
//! A set of decoy "bait" routes for well-known scanner/attacker targets
//! (`/.env`, `/wp-login.php`, `/.git/config`, actuator endpoints, backup archives,
//! …). A real player has no reason to fetch these, so a hit from an authenticated
//! participant is a strong probe/automation signal: each hit is logged and, when
//! attributable to an active participation, raises `HoneypotHit` (and, once enough
//! distinct baits are tripped in a window, `HoneypotChain`). Every handler returns
//! a plausible 404 so the decoy never reveals itself.

use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use std::net::SocketAddr;

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::MaybeUser;

/// The bait paths (RSCTF `HoneypotBait`). Any request to one of these is a decoy
/// hit — none correspond to a real rsctf resource.
const BAITS: &[&str] = &[
    "/.git/config",
    "/.git/HEAD",
    "/.svn/wc.db",
    "/.DS_Store",
    "/.env",
    "/.aws/credentials",
    "/wp-admin",
    "/wp-admin/",
    "/wp-login.php",
    "/phpmyadmin",
    "/phpmyadmin/",
    "/phpmyadmin/index.php",
    "/server-status",
    "/actuator",
    "/actuator/env",
    "/actuator/health",
    "/_ignition/execute-solution",
    "/cgi-bin/luci",
    "/backup.zip",
    "/backup.tar.gz",
    "/database.sql",
    "/admin-portal/login",
    "/admin-portal/dashboard",
    "/api/internal/users.json",
    "/internal/debug-console",
    "/_debug/console",
    "/wp-content/uploads/backup-2024-q3.zip",
    "/db-export.php",
    "/backups/db-dump.sql",
    "/sitemap-internal.xml",
];

pub fn router() -> Router<SharedState> {
    let mut router = Router::new();
    for &path in BAITS {
        // GET is the scanner default; a few RSCTF baits also accept POST (login /
        // execute forms) — accept both so a POST probe is caught too.
        router = router.route(path, get(bait).post(bait));
    }
    router
}

/// Every bait route funnels here: log the hit (attributing it to the caller's live
/// participation when authenticated) and return an innocuous 404.
async fn bait(
    State(st): State<SharedState>,
    MaybeUser(user): MaybeUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let bait_path = uri.path().to_string();
    let remote_ip = crate::services::anti_cheat::client_ip(&headers, Some(peer.ip()));
    // SameSite=Lax cookies accompany cross-site top-level GET navigations. Never
    // let another site frame/link a bait URL and assign suspicion to the victim;
    // requests without same-origin browser provenance remain anonymous signals.
    let user = user.filter(|_| {
        crate::middlewares::request_security::same_origin(
            &headers,
            st.config.public_url.as_deref(),
            st.config.cookie_secure,
        )
    });
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    crate::services::suspicion::record_honeypot_hit(&st, user, &bait_path, remote_ip, user_agent)
        .await;

    // Decoy response — a plausible "nothing here", never revealing the trap.
    (StatusCode::NOT_FOUND, "Not Found").into_response()
}
