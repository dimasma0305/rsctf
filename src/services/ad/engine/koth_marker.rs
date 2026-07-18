//! Trusted interpretation of the attacker-controlled KotH control marker.

use chrono::{DateTime, Utc};

/// The in-container marker a team writes its minted control token into to claim a
/// hill. The checker reads it on both sides of the independent functional probe.
const KOTH_KING_PATH: &str = "/koth/king";

/// Marker commands run inside attacker-controlled containers. Two brackets at
/// this limit bound adversarial delay while leaving ample room around the
/// independent functional checker and tolerating runtime exec startup latency.
const KOTH_MARKER_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

pub(super) enum KothMarkerRead {
    Observed(Option<String>),
    Unavailable(String),
}

impl KothMarkerRead {
    fn error(&self) -> Option<&str> {
        match self {
            Self::Observed(_) => None,
            Self::Unavailable(error) => Some(error),
        }
    }
}

/// Return a controller only when the same marker brackets the functional probe.
/// Marker access is attacker-controlled state inside the hill, so its failure is
/// diagnostic only and must not rewrite a valid functional verdict into a void.
pub(super) fn stable_koth_marker(
    before: KothMarkerRead,
    after: KothMarkerRead,
) -> (Option<String>, bool, Option<String>) {
    match (before, after) {
        (KothMarkerRead::Observed(before), KothMarkerRead::Observed(after)) if before == after => {
            (after, true, None)
        }
        (KothMarkerRead::Observed(_), KothMarkerRead::Observed(_)) => (None, false, None),
        (before, after) => {
            let error = before
                .error()
                .or_else(|| after.error())
                .unwrap_or("control marker unavailable")
                .to_string();
            (None, false, Some(error))
        }
    }
}

/// The marker/probe sample belongs to the match only when the complete
/// observation finished strictly before the configured event deadline.
pub(super) fn observation_precedes_deadline(
    observed_at: DateTime<Utc>,
    event_end: DateTime<Utc>,
) -> bool {
    observed_at < event_end
}

pub(super) async fn read_koth_marker(
    containers: &dyn crate::services::container::ContainerManager,
    container_id: Option<&str>,
) -> KothMarkerRead {
    let Some(container_id) = container_id else {
        return KothMarkerRead::Unavailable("hill container is unavailable".to_string());
    };
    let read = containers.exec(
        container_id,
        vec![
            "sh".into(),
            "-c".into(),
            format!("head -c 257 {KOTH_KING_PATH} 2>/dev/null"),
        ],
    );
    match tokio::time::timeout(KOTH_MARKER_READ_TIMEOUT, read).await {
        Ok(Ok(output)) => {
            let marker = output.trim();
            KothMarkerRead::Observed(
                (!marker.is_empty() && marker.len() <= 256).then(|| marker.to_owned()),
            )
        }
        Ok(Err(error)) => {
            KothMarkerRead::Unavailable(format!("control marker read failed: {error}"))
        }
        Err(_) => KothMarkerRead::Unavailable("control marker read timed out".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_marker_elects_nobody_without_voiding_functional_evidence() {
        let (controller, observed, error) = stable_koth_marker(
            KothMarkerRead::Unavailable("marker timed out".to_string()),
            KothMarkerRead::Observed(Some("koth_new".to_string())),
        );

        assert_eq!(controller, None);
        assert!(!observed);
        assert_eq!(error.as_deref(), Some("marker timed out"));
    }

    #[test]
    fn only_a_stable_marker_elects_a_controller_candidate() {
        let (controller, observed, error) = stable_koth_marker(
            KothMarkerRead::Observed(Some("koth_new".to_string())),
            KothMarkerRead::Observed(Some("koth_new".to_string())),
        );

        assert_eq!(controller.as_deref(), Some("koth_new"));
        assert!(observed);
        assert_eq!(error, None);
    }

    #[test]
    fn attacker_controlled_marker_reads_have_a_small_bound() {
        assert_eq!(KOTH_MARKER_READ_TIMEOUT, std::time::Duration::from_secs(2));
    }

    #[test]
    fn event_deadline_is_a_strict_observation_fence() {
        let end = Utc::now();
        assert!(observation_precedes_deadline(
            end - chrono::Duration::milliseconds(1),
            end,
        ));
        assert!(!observation_precedes_deadline(end, end));
        assert!(!observation_precedes_deadline(
            end + chrono::Duration::milliseconds(1),
            end,
        ));
    }
}
