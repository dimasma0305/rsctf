//! Operator image preflight: pull every enabled, approved challenge image and
//! smoke-start one bounded temporary instance per distinct image before an
//! event. The job is a durable control-plane job; per-challenge outcomes live
//! in `ImagePreflightResults` and cascade with the job's retention purge.

use serde::{Deserialize, Serialize};

use crate::services::worker_store::WorkerCapacitySummary;

mod plan;
mod run;
mod store;

pub use plan::{plan, PreflightPlan};
pub use run::execute_job;
pub use store::{load_results, ImagePreflightResultModel};

/// Per-step outcome stored as its string name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Skipped,
}

impl StepStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Running => "Running",
            Self::Succeeded => "Succeeded",
            Self::Failed => "Failed",
            Self::Skipped => "Skipped",
        }
    }
}

/// Requested resources, either for one instance or summed over expected
/// simultaneous instances.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceTotals {
    pub cpu_millis: i64,
    pub memory_bytes: i64,
    pub storage_bytes: i64,
    pub replicas: i64,
    pub slots: i64,
}

impl ResourceTotals {
    fn scaled(self, instances: i64) -> Self {
        Self {
            cpu_millis: self.cpu_millis.saturating_mul(instances),
            memory_bytes: self.memory_bytes.saturating_mul(instances),
            storage_bytes: self.storage_bytes.saturating_mul(instances),
            replicas: self.replicas.saturating_mul(instances),
            slots: self.slots.saturating_mul(instances),
        }
    }

    fn add(&mut self, other: Self) {
        self.cpu_millis = self.cpu_millis.saturating_add(other.cpu_millis);
        self.memory_bytes = self.memory_bytes.saturating_add(other.memory_bytes);
        self.storage_bytes = self.storage_bytes.saturating_add(other.storage_bytes);
        self.replicas = self.replicas.saturating_add(other.replicas);
        self.slots = self.slots.saturating_add(other.slots);
    }
}

/// Requested totals versus the capacity the platform can account for.
/// `available` is `None` when no trusted worker plane exists: the local Docker
/// and Kubernetes backends expose no aggregate capacity, so only totals are
/// reported there.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapacityReport {
    pub requested: ResourceTotals,
    /// Subset of `requested` that the trusted worker plane would schedule.
    pub worker_requested: ResourceTotals,
    pub available: Option<WorkerCapacitySummary>,
    pub accepted_teams: i64,
    pub instances: i64,
}

/// Durable job result: counts plus the capacity comparison.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreflightSummary {
    pub challenges: usize,
    pub images: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub skipped: usize,
    pub capacity: CapacityReport,
}

pub fn scope_key(game_id: i32) -> String {
    format!("game:{game_id}:image-preflight")
}

const MAX_ERROR_CHARS: usize = 500;

/// Bound stored/displayed error text to one line-ish of actionable detail.
pub(crate) fn bounded_error(text: impl AsRef<str>) -> String {
    let text = text.as_ref().trim();
    let mut out: String = text.chars().take(MAX_ERROR_CHARS).collect();
    if text.chars().count() > MAX_ERROR_CHARS {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_text_is_bounded_and_marked_when_truncated() {
        assert_eq!(bounded_error("  short  "), "short");
        let long = "x".repeat(MAX_ERROR_CHARS + 10);
        let bounded = bounded_error(&long);
        assert_eq!(bounded.chars().count(), MAX_ERROR_CHARS + 1);
        assert!(bounded.ends_with('…'));
    }

    #[test]
    fn totals_scale_and_sum_without_overflow() {
        let one = ResourceTotals {
            cpu_millis: 1_000,
            memory_bytes: 64 << 20,
            storage_bytes: 512 << 20,
            replicas: 1,
            slots: 1,
        };
        let mut total = one.scaled(3);
        assert_eq!(total.cpu_millis, 3_000);
        assert_eq!(total.slots, 3);
        total.add(ResourceTotals {
            cpu_millis: i64::MAX,
            ..ResourceTotals::default()
        });
        assert_eq!(total.cpu_millis, i64::MAX);
        assert_eq!(total.replicas, 3);
    }

    #[test]
    fn status_names_are_the_wire_strings() {
        for (status, name) in [
            (StepStatus::Pending, "Pending"),
            (StepStatus::Running, "Running"),
            (StepStatus::Succeeded, "Succeeded"),
            (StepStatus::Failed, "Failed"),
            (StepStatus::Skipped, "Skipped"),
        ] {
            assert_eq!(status.as_str(), name);
            assert_eq!(serde_json::to_value(status).unwrap(), name);
        }
    }
}
