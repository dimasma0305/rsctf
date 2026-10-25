//! Optional TCP readiness. Never restart a vulnerable workload or alter SLA results.

use k8s_openapi::api::core::v1::{Pod, Probe, TCPSocketAction};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;

pub(super) fn probe(port: i32) -> Option<Probe> {
    let enabled = std::env::var("RSCTF_K8S_READINESS_PROBE")
        .is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
    enabled.then(|| tcp_probe(port))
}

fn tcp_probe(port: i32) -> Probe {
    Probe {
        tcp_socket: Some(TCPSocketAction {
            port: IntOrString::Int(port),
            host: None,
        }),
        period_seconds: Some(15),
        timeout_seconds: Some(1),
        success_threshold: Some(1),
        failure_threshold: Some(1),
        ..Default::default()
    }
}

pub(super) fn status(pod: &Pod) -> &'static str {
    let phase = pod
        .status
        .as_ref()
        .and_then(|status| status.phase.as_deref());
    let has_probe = pod.spec.as_ref().is_some_and(|spec| {
        spec.containers
            .iter()
            .any(|container| container.readiness_probe.is_some())
    });
    let ready = pod
        .status
        .as_ref()
        .and_then(|status| status.conditions.as_ref())
        .is_some_and(|conditions| {
            conditions
                .iter()
                .any(|condition| condition.type_ == "Ready" && condition.status == "True")
        });
    if phase == Some("Running") && has_probe && !ready {
        "pending"
    } else {
        super::map_phase(phase)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_is_not_ready_until_the_declared_port_accepts_connections() {
        let mut pod: Pod = serde_json::from_value(serde_json::json!({
            "spec": { "containers": [{ "name": "challenge" }] },
            "status": { "phase": "Running", "conditions": [{ "type": "Ready", "status": "False" }] }
        }))
        .unwrap();
        assert_eq!(status(&pod), "running"); // Legacy Pods retain their contract.
        pod.spec.as_mut().unwrap().containers[0].readiness_probe = Some(tcp_probe(8080));
        assert_eq!(status(&pod), "pending");
        pod.status.as_mut().unwrap().conditions.as_mut().unwrap()[0].status = "True".into();
        assert_eq!(status(&pod), "running");
        pod.status.as_mut().unwrap().phase = Some("Failed".into());
        assert_eq!(status(&pod), "destroyed");
        let container = &pod.spec.as_ref().unwrap().containers[0];
        assert!(container.liveness_probe.is_none());
        assert!(container.startup_probe.is_none());
    }
}
