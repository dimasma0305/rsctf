//! Bounded operational visibility for the read-only WebSocket feeds.

use crate::hubs::admission::WebSocketOperationalMetrics;
use crate::middlewares::privilege_authentication::AdminUser;
use crate::utils::shared::RequestResponse;

pub(super) async fn websocket_metrics(
    _admin: AdminUser,
) -> RequestResponse<WebSocketOperationalMetrics> {
    RequestResponse::ok(crate::hubs::admission::operational_metrics())
}
