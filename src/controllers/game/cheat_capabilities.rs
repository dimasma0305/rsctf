//! Public capability inventory for cheat-report rule codes.

use serde_json::Value as Json;

/// Honest, additive metadata for all stable detector codes. This prevents an
/// empty result from being mistaken for proof that every configured rule ran.
pub(super) fn detector_capabilities() -> Vec<Json> {
    use crate::services::suspicion::{SuspicionType as T, DEFAULTS};

    DEFAULTS
        .iter()
        .map(|(ty, _, _)| {
            let (status, scope) = match ty {
                T::StolenFlag => ("active", "jeopardy"),

                T::FingerprintChurn
                | T::IpChurn
                | T::SharedIp
                | T::SharedFingerprint
                | T::UnknownIp
                | T::CrossTeamIp
                | T::SubnetOverlap
                | T::SessionConcurrency => ("background", "allGames"),
                T::Hoarding
                | T::Burst
                | T::HighWrongRate
                | T::SequenceSimilarity
                | T::ZeroWrongAttempts
                | T::SolutionRelay
                | T::AdaptiveFastSolve
                | T::AutomatedPattern
                | T::FirstBloodAnomaly => ("background", "jeopardy"),
                T::CrossTeamContainerAccess
                | T::DelayedSolveSubmission
                | T::InstantSubmitAfterAccess
                | T::AccessIpMismatchAtSubmission => ("background", "jeopardyContainers"),
                T::NoDownload | T::FastSolveOpen | T::FastSolveDownload | T::DirectedSolving => {
                    ("telemetryOnly", "jeopardy")
                }
                T::NoContainer | T::FastSolveContainer | T::SubmitterNeverAccessedContainer => {
                    ("telemetryOnly", "jeopardyContainers")
                }
                T::ClusteredRegistration => ("telemetryOnly", "allGames"),
                T::HoneypotHit | T::HoneypotProtocolHit | T::HoneypotChain => {
                    ("telemetryOnly", "platform")
                }
                T::WrongFlagLeakage => ("telemetryOnly", "jeopardy"),
                T::FlagEgress => ("telemetryOnly", "jeopardyContainers"),

                T::TokenAbuse | T::CollusionGroup | T::HoneypotCanaryFlag => {
                    ("unimplemented", "jeopardy")
                }
            };
            let detail = match status {
                "active" => "Emitted while the relevant request is processed.",
                "background" => "Evaluated asynchronously from persisted game evidence.",
                "telemetryOnly" => {
                    "Telemetry is retained for review, but no suspicion event is emitted."
                }
                _ => "No production detector is currently wired for this rule.",
            };
            serde_json::json!({
                "code": ty.code(),
                "status": status,
                "scope": scope,
                "detail": detail,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_covers_every_stable_rule_with_known_values() {
        let capabilities = detector_capabilities();
        assert_eq!(
            capabilities.len(),
            crate::services::suspicion::DEFAULTS.len()
        );
        for capability in capabilities {
            assert!(matches!(
                capability["status"].as_str(),
                Some("active" | "background" | "telemetryOnly" | "unimplemented")
            ));
            assert!(matches!(
                capability["scope"].as_str(),
                Some("allGames" | "jeopardy" | "jeopardyContainers" | "platform")
            ));
        }
    }

    #[test]
    fn capability_matrix_distinguishes_execution_from_retained_telemetry() {
        let capabilities = detector_capabilities();
        let status = |code: &str| {
            capabilities
                .iter()
                .find(|capability| capability["code"] == code)
                .map(|capability| {
                    (
                        capability["status"].as_str().unwrap(),
                        capability["scope"].as_str().unwrap(),
                    )
                })
                .unwrap()
        };
        assert_eq!(status("StolenFlag"), ("active", "jeopardy"));
        assert_eq!(status("SharedIP"), ("background", "allGames"));
        assert_eq!(
            status("CrossTeamContainerAccess"),
            ("background", "jeopardyContainers")
        );
        assert_eq!(status("WrongFlagLeakage"), ("telemetryOnly", "jeopardy"));
        assert_eq!(status("FastSolve-Open"), ("telemetryOnly", "jeopardy"));
        assert_eq!(
            status("FastSolve-Container"),
            ("telemetryOnly", "jeopardyContainers")
        );
        assert_eq!(
            status("ClusteredRegistration"),
            ("telemetryOnly", "allGames")
        );
        assert_eq!(
            status("SubmitterNeverAccessedContainer"),
            ("telemetryOnly", "jeopardyContainers")
        );
        assert_eq!(status("HoneypotProtocolHit"), ("telemetryOnly", "platform"));
        assert_eq!(status("HoneypotHit"), ("telemetryOnly", "platform"));
        assert_eq!(status("HoneypotChain"), ("telemetryOnly", "platform"));
        assert_eq!(status("TokenAbuse"), ("unimplemented", "jeopardy"));
    }
}
