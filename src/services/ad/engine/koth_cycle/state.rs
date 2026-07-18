use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Persisted reset state. String values are deliberately stable because they
/// are also exposed by the admin/player APIs.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum CrownPhase {
    FinalizePending,
    SnapshotPending,
    DestroyPending,
    CreatePending,
    PublishPending,
    CapabilityPending,
    ReadinessPending,
    FirewallPending,
    #[default]
    Active,
    CooldownReleasePending,
    Completed,
    Failed,
    Ended,
}

impl CrownPhase {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::FinalizePending => "FinalizePending",
            Self::SnapshotPending => "SnapshotPending",
            Self::DestroyPending => "DestroyPending",
            Self::CreatePending => "CreatePending",
            Self::PublishPending => "PublishPending",
            Self::CapabilityPending => "CapabilityPending",
            Self::ReadinessPending => "ReadinessPending",
            Self::FirewallPending => "FirewallPending",
            Self::Active => "Active",
            Self::CooldownReleasePending => "CooldownReleasePending",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
            Self::Ended => "Ended",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "FinalizePending" => Self::FinalizePending,
            "SnapshotPending" => Self::SnapshotPending,
            "DestroyPending" => Self::DestroyPending,
            "CreatePending" => Self::CreatePending,
            "PublishPending" => Self::PublishPending,
            "CapabilityPending" => Self::CapabilityPending,
            "ReadinessPending" => Self::ReadinessPending,
            "FirewallPending" => Self::FirewallPending,
            "Active" => Self::Active,
            "CooldownReleasePending" => Self::CooldownReleasePending,
            "Completed" => Self::Completed,
            "Failed" => Self::Failed,
            "Ended" => Self::Ended,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CrownCyclePosition {
    pub cycle_number: i32,
    pub epoch: i32,
    /// One-based tick within the configured cycle.
    pub tick: i32,
    pub is_boundary: bool,
}

/// Return the immutable crown-cycle coordinates for an authoritative
/// round. Rounds before the official boundary are outside crown scoring.
pub(crate) fn cycle_position(
    round: i32,
    official_start: i32,
    epoch_ticks: i32,
    cycle_ticks: i32,
) -> Option<CrownCyclePosition> {
    if round < official_start
        || official_start < 1
        || epoch_ticks < 1
        || cycle_ticks < 1
        || epoch_ticks % cycle_ticks != 0
    {
        return None;
    }
    let offset = round - official_start;
    Some(CrownCyclePosition {
        cycle_number: offset / cycle_ticks + 1,
        epoch: offset / epoch_ticks + 1,
        tick: offset % cycle_ticks + 1,
        is_boundary: offset % cycle_ticks == 0,
    })
}

/// Select every tied leader from confirmed, healthy control evidence.  If
/// cooling all leaders would leave no challenger, the persisted cooldown set is
/// empty for this cycle.
pub(crate) fn select_cycle_champions(
    accepted_participations: &[i32],
    healthy_controlled_ticks: &[(i32, i64)],
) -> Vec<i32> {
    let accepted: std::collections::BTreeSet<i32> =
        accepted_participations.iter().copied().collect();
    let mut totals = BTreeMap::<i32, i64>::new();
    for &(participation_id, ticks) in healthy_controlled_ticks {
        if accepted.contains(&participation_id) && ticks > 0 {
            *totals.entry(participation_id).or_default() += ticks;
        }
    }
    let Some(lead) = totals.values().copied().max() else {
        return Vec::new();
    };
    let champions: Vec<i32> = totals
        .into_iter()
        .filter_map(|(participation_id, ticks)| (ticks == lead).then_some(participation_id))
        .collect();
    if accepted.len().saturating_sub(champions.len()) < 1 {
        Vec::new()
    } else {
        champions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycle_coordinates_are_stable_across_epochs() {
        assert_eq!(
            cycle_position(13, 10, 12, 3),
            Some(CrownCyclePosition {
                cycle_number: 2,
                epoch: 1,
                tick: 1,
                is_boundary: true,
            })
        );
        assert_eq!(cycle_position(21, 10, 12, 3).unwrap().epoch, 1);
        assert_eq!(cycle_position(22, 10, 12, 3).unwrap().epoch, 2);
        assert_eq!(cycle_position(9, 10, 12, 3), None);
        assert_eq!(cycle_position(10, 10, 12, 5), None);
    }

    #[test]
    fn cycle_champion_uses_most_healthy_confirmed_ticks() {
        assert_eq!(
            select_cycle_champions(&[1, 2, 3], &[(1, 2), (2, 1), (1, 1)]),
            vec![1]
        );
    }

    #[test]
    fn tied_leaders_are_all_cooled_when_a_challenger_remains() {
        assert_eq!(
            select_cycle_champions(&[1, 2, 3], &[(1, 2), (2, 2), (3, 1)]),
            vec![1, 2]
        );
    }

    #[test]
    fn cooldown_is_disabled_when_it_would_remove_every_challenger() {
        assert!(select_cycle_champions(&[1, 2], &[(1, 2), (2, 2)]).is_empty());
        assert!(select_cycle_champions(&[1], &[(1, 2)]).is_empty());
    }
}
