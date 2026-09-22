//! Bounded operational visibility for the read-only WebSocket feeds.

use axum::extract::State;
use serde::Serialize;

use crate::app_state::SharedState;
use crate::hubs::admission::WebSocketOperationalMetrics;
use crate::middlewares::privilege_authentication::AdminUser;
use crate::services::docker_admission::DockerAdmissionMetrics;
use crate::services::event_bus::EventBusOperationalMetrics;
use crate::utils::shared::RequestResponse;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RealtimeOperationalMetrics {
    websocket: WebSocketOperationalMetrics,
    fanout: EventBusOperationalMetrics,
    docker: DockerAdmissionMetrics,
}

pub(super) async fn realtime_metrics(
    State(st): State<SharedState>,
    _admin: AdminUser,
) -> RequestResponse<RealtimeOperationalMetrics> {
    RequestResponse::ok(RealtimeOperationalMetrics {
        websocket: crate::hubs::admission::operational_metrics(),
        fanout: st.events.operational_metrics(),
        docker: crate::services::docker_admission::docker_admission().metrics(),
    })
}
