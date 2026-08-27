//! Role-specific KotH router composition.

use axum::extract::Path;
use axum::response::Redirect;
use axum::routing::post;
use axum::Router;

use crate::app_state::SharedState;

use super::{common_router, recovery_router};

/// Complete monolithic KotH surface. The historical recovery path stays
/// byte-for-byte compatible when one `all` process owns both HTTP and checker
/// execution.
pub fn router() -> Router<SharedState> {
    common_router().merge(recovery_router())
}

/// KotH surface for horizontally scaled, unprivileged web replicas.
///
/// A proxy that cannot match the parameterized legacy route can send it here;
/// the temporary same-origin redirect preserves the POST while moving it under
/// the fixed `/api/stateful` prefix understood by portable Kubernetes Ingress.
pub fn web_router() -> Router<SharedState> {
    common_router().route_service(
        "/api/edit/games/{id}/ad/koth/{challengeId}/recover",
        post(redirect_recover_hill),
    )
}

/// Privileged singleton surface for lifecycle recovery. Custom checker probes
/// install a short-lived uid-scoped firewall rule, so this must never execute
/// on a capability-free web replica.
pub fn stateful_router() -> Router<SharedState> {
    recovery_router()
}

pub(super) async fn redirect_recover_hill(
    Path((game_id, challenge_id)): Path<(i32, i32)>,
) -> Redirect {
    Redirect::temporary(&format!(
        "/api/stateful/edit/games/{game_id}/ad/koth/{challenge_id}/recover"
    ))
}
