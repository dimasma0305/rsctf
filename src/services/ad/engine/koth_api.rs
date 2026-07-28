//! Stable, exact-tick reads for normalized API-arena snapshots.

use chrono::{DateTime, Utc};

pub(crate) const MAX_API_ARENA_TEAMS: usize = 2_000;
pub(crate) const API_OBJECTIVE_NORMALIZATION_SCALE: i64 = 1_000_000;
pub(crate) const API_ACTIVITY_WEIGHT: f64 = 0.35;
pub(crate) const API_OBJECTIVE_WEIGHT: f64 = 0.65;

/// Calculate one normalized API-arena tick. Callers validate that each input is
/// finite and in `[0,1]` before invoking this pure formula.
pub(crate) fn api_tick_rates(
    activity_rate: f64,
    objective_rate: f64,
    integrity_rate: f64,
) -> (f64, f64) {
    let core_rate = if activity_rate == 0.0 || objective_rate == 0.0 {
        0.0
    } else {
        (1.0 / (API_ACTIVITY_WEIGHT / activity_rate + API_OBJECTIVE_WEIGHT / objective_rate))
            .clamp(0.0, 1.0)
    };
    (core_rate, (integrity_rate * core_rate).clamp(0.0, 1.0))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct KothApiEvidence {
    pub(super) participation_id: i32,
    pub(super) activity_earned: i64,
    pub(super) activity_possible: i64,
    pub(super) objective_earned: i64,
    pub(super) objective_possible: i64,
    pub(super) valid_actions: i64,
    pub(super) total_actions: i64,
    pub(super) objective_count: i16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct KothApiSnapshot {
    pub(super) hash: [u8; 32],
    pub(super) rows: Vec<KothApiEvidence>,
}

pub(super) enum KothApiSnapshotRead {
    Observed(KothApiSnapshot),
    Unavailable(String),
}

impl KothApiSnapshotRead {
    fn error(&self) -> Option<&str> {
        match self {
            Self::Observed(_) => None,
            Self::Unavailable(error) => Some(error),
        }
    }
}

#[derive(sqlx::FromRow)]
struct SnapshotRow {
    snapshot_hash: Vec<u8>,
    participation_id: Option<i32>,
    activity_earned: Option<i64>,
    activity_possible: Option<i64>,
    objective_earned: Option<i64>,
    objective_possible: Option<i64>,
    valid_actions: Option<i64>,
    total_actions: Option<i64>,
    objective_count: Option<i16>,
}

/// Read one atomically visible snapshot only when it belongs to the exact
/// runtime, capability generation, and current scoring tick.
#[allow(clippy::too_many_arguments)]
pub(super) async fn read_koth_api_snapshot(
    pool: &sqlx::PgPool,
    target_id: i32,
    cycle_id: i64,
    reset_attempt: i32,
    container_id: &str,
    round_id: i32,
    round_start: DateTime<Utc>,
    round_end: DateTime<Utc>,
) -> KothApiSnapshotRead {
    let rows = sqlx::query_as::<_, SnapshotRow>(
        r#"SELECT snapshot.snapshot_hash,
                  score.participation_id,
                  score.activity_earned, score.activity_possible,
                  score.objective_earned, score.objective_possible,
                  score.valid_actions, score.total_actions,
                  score.objective_count
             FROM "KothApiSnapshots" snapshot
        LEFT JOIN "KothApiSnapshotScores" score
               ON score.target_id = snapshot.target_id
            WHERE snapshot.target_id = $1
              AND snapshot.cycle_id = $2
              AND snapshot.reset_attempt = $3
              AND snapshot.container_id = $4
              AND snapshot.ad_round_id = $5
              AND snapshot.accepted_at >= $6
              AND snapshot.accepted_at < $7
            ORDER BY score.participation_id"#,
    )
    .bind(target_id)
    .bind(cycle_id)
    .bind(reset_attempt)
    .bind(container_id)
    .bind(round_id)
    .bind(round_start)
    .bind(round_end)
    .fetch_all(pool)
    .await;
    let rows = match rows {
        Ok(rows) if !rows.is_empty() => rows,
        Ok(_) => {
            return KothApiSnapshotRead::Unavailable(
                "KotH API referee has not submitted this scoring tick".to_string(),
            )
        }
        Err(error) => {
            return KothApiSnapshotRead::Unavailable(format!(
                "KotH API snapshot read failed: {error}"
            ))
        }
    };
    let hash: [u8; 32] = match rows[0].snapshot_hash.as_slice().try_into() {
        Ok(hash) => hash,
        Err(_) => {
            return KothApiSnapshotRead::Unavailable(
                "KotH API snapshot has an invalid digest".to_string(),
            )
        }
    };
    if rows
        .iter()
        .any(|row| row.snapshot_hash.as_slice() != hash.as_slice())
    {
        return KothApiSnapshotRead::Unavailable(
            "KotH API snapshot changed during its read".to_string(),
        );
    }
    let evidence = rows
        .into_iter()
        .filter_map(|row| {
            Some(KothApiEvidence {
                participation_id: row.participation_id?,
                activity_earned: row.activity_earned?,
                activity_possible: row.activity_possible?,
                objective_earned: row.objective_earned?,
                objective_possible: row.objective_possible?,
                valid_actions: row.valid_actions?,
                total_actions: row.total_actions?,
                objective_count: row.objective_count?,
            })
        })
        .collect();
    KothApiSnapshotRead::Observed(KothApiSnapshot {
        hash,
        rows: evidence,
    })
}

/// Accept evidence only when the exact same snapshot brackets the functional
/// probe. Missing, malformed, or changing input voids the field-wide tick.
pub(super) fn stable_koth_api_snapshot(
    before: KothApiSnapshotRead,
    after: KothApiSnapshotRead,
) -> (Option<KothApiSnapshot>, Option<String>) {
    match (before, after) {
        (KothApiSnapshotRead::Observed(before), KothApiSnapshotRead::Observed(after))
            if before == after =>
        {
            (Some(after), None)
        }
        (KothApiSnapshotRead::Observed(_), KothApiSnapshotRead::Observed(_)) => (
            None,
            Some("KotH API snapshot changed during the functional probe".to_string()),
        ),
        (before, after) => (
            None,
            Some(
                before
                    .error()
                    .or_else(|| after.error())
                    .unwrap_or("KotH API snapshot unavailable")
                    .to_string(),
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(value: i64) -> KothApiSnapshotRead {
        KothApiSnapshotRead::Observed(KothApiSnapshot {
            hash: [value as u8; 32],
            rows: vec![KothApiEvidence {
                participation_id: 7,
                activity_earned: value,
                activity_possible: 10,
                objective_earned: value,
                objective_possible: 10,
                valid_actions: 1,
                total_actions: 1,
                objective_count: 1,
            }],
        })
    }

    #[test]
    fn only_an_exact_bracketed_snapshot_is_stable() {
        assert!(stable_koth_api_snapshot(snapshot(5), snapshot(5))
            .0
            .is_some());
        let changed = stable_koth_api_snapshot(snapshot(5), snapshot(6));
        assert!(changed.0.is_none());
        assert!(changed.1.unwrap().contains("changed"));
    }

    #[test]
    fn missing_snapshot_voids_instead_of_carrying_a_prior_tick() {
        let missing = stable_koth_api_snapshot(
            KothApiSnapshotRead::Unavailable("no current tick".to_string()),
            snapshot(5),
        );
        assert!(missing.0.is_none());
        assert_eq!(missing.1.as_deref(), Some("no current tick"));
    }

    #[test]
    fn api_tick_requires_both_play_channels_and_applies_integrity_same_tick() {
        assert_eq!(api_tick_rates(0.0, 1.0, 1.0), (0.0, 0.0));
        assert_eq!(api_tick_rates(1.0, 0.0, 1.0), (0.0, 0.0));
        assert_eq!(api_tick_rates(1.0, 1.0, 0.0), (1.0, 0.0));
        assert_eq!(api_tick_rates(1.0, 1.0, 1.0), (1.0, 1.0));
    }
}
