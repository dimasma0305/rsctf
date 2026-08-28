//! controllers/honeypot.rs — ported from RSCTF `Controllers/HoneypotController.cs`.
//!
//! A set of decoy "bait" routes for well-known scanner/attacker targets
//! (`/.env`, `/wp-login.php`, `/.git/config`, actuator endpoints, backup archives,
//! …). Each hit is retained as raw telemetry. These global routes do not carry a
//! trustworthy game/participation identity, so they never create suspicion events;
//! an authenticated caller id is retained only for manual audit. Every handler
//! returns a plausible 404 so the decoy never reveals itself.

use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use std::net::SocketAddr;

use crate::app_state::SharedState;
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

/// Every bait route funnels here: retain raw telemetry and return an innocuous 404.
async fn bait(
    State(st): State<SharedState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let bait_path = uri.path().to_string();
    let remote_ip = crate::services::anti_cheat::client_ip(&headers, Some(peer.ip()));
    let source = remote_ip.as_deref().unwrap_or_default();
    if crate::services::suspicion::admit_honeypot_source(
        source,
        crate::services::suspicion::HoneypotRouteClass::Http,
    )
    .await
    {
        let user_agent = headers
            .get(header::USER_AGENT)
            .and_then(|value| value.to_str().ok());
        let _ = crate::services::suspicion::enqueue_honeypot_hit(
            &st,
            None,
            &bait_path,
            remote_ip.as_deref(),
            user_agent,
        );
    }

    // Decoy response — a plausible "nothing here", never revealing the trap.
    (StatusCode::NOT_FOUND, "Not Found").into_response()
}
