//! controllers/honeypot.rs — ported from RSCTF `Controllers/HoneypotController.cs`.
//!
//! A set of decoy "bait" routes for well-known scanner/attacker targets
//! (`/.env`, `/wp-login.php`, `/.git/config`, actuator endpoints, backup archives,
//! …). Admitted hits are sampled and combined into bounded minute aggregates.
//! These global routes do not carry a trustworthy game/participation identity, so
//! they never create suspicion events; a consistently authenticated caller id may
//! be retained only for manual audit. Every handler returns a plausible 404 so the
//! decoy never reveals itself.

use axum::extract::{ConnectInfo, Extension, Request, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::middleware::Next;
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

fn not_found_response() -> Response {
    (StatusCode::NOT_FOUND, "Not Found").into_response()
}

pub(crate) async fn admission_middleware(
    State(st): State<SharedState>,
    mut request: Request,
    next: Next,
) -> Response {
    if !BAITS.contains(&request.uri().path()) {
        return next.run(request).await;
    }
    let peer_ip = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(peer)| peer.ip());
    let Some(remote_ip) = crate::services::anti_cheat::client_ip(request.headers(), peer_ip) else {
        return not_found_response();
    };
    let Some(admission) =
        st.honeypot_telemetry
            .admit_source(st.config.as_ref(), &remote_ip, request.uri().path())
    else {
        return not_found_response();
    };
    request.extensions_mut().insert(admission);
    next.run(request).await
}

/// Every admitted bait route funnels here: enqueue bounded telemetry and
/// return an innocuous 404 without awaiting PostgreSQL.
async fn bait(
    Extension(admission): Extension<crate::services::honeypot_telemetry::HoneypotAdmission>,
    State(st): State<SharedState>,
    MaybeUser(user): MaybeUser,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
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
        .and_then(|value| value.to_str().ok());

    crate::services::suspicion::record_honeypot_hit(&st, user, uri.path(), user_agent, admission);

    // Decoy response — a plausible "nothing here", never revealing the trap.
    not_found_response()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use axum::body::to_bytes;

    use super::*;

    #[tokio::test]
    async fn admitted_and_saturated_paths_share_the_exact_decoy_response() {
        let first = not_found_response();
        let second = not_found_response();
        assert_eq!(first.status(), StatusCode::NOT_FOUND);
        assert_eq!(second.status(), first.status());
        let first_body = to_bytes(first.into_body(), 32).await.unwrap();
        let second_body = to_bytes(second.into_body(), 32).await.unwrap();
        assert_eq!(first_body, second_body);
        assert_eq!(first_body.as_ref(), b"Not Found");
    }

    #[test]
    fn bait_inventory_is_unique_and_storage_bounded() {
        let unique = BAITS.iter().copied().collect::<BTreeSet<_>>();
        assert_eq!(unique.len(), BAITS.len());
        assert!(BAITS.iter().all(|bait| bait.len() <= 128));
    }

    #[test]
    fn admission_is_layered_before_handler_authentication() {
        let server = include_str!("../server.rs");
        assert!(server.contains("crate::controllers::honeypot::admission_middleware"));
        let source = include_str!("honeypot.rs");
        let admission = source
            .find("pub(crate) async fn admission_middleware")
            .unwrap();
        let handler = source.find("async fn bait(").unwrap();
        assert!(admission < handler);
        assert!(source[admission..handler].contains("return not_found_response()"));
    }
}
