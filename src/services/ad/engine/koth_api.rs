//! Stable, exact-tick reads for normalized Leaderboard snapshots.

use chrono::{DateTime, Utc};

pub(crate) const MAX_LEADERBOARD_TEAMS: usize = 2_000;
pub(crate) const API_OBJECTIVE_NORMALIZATION_SCALE: i64 = 1_000_000;
pub(crate) const API_RELATIVE_PERFORMANCE_EXPONENT: f64 = 0.75;
/// Finalized waves use a lagged, contiguous settlement window so the referee
/// can publish through the cutoff before the functional probe begins. The
/// next round owns the previous round's short tail; waves are never sampled
/// from a still-open interval.
pub(crate) const API_WAVE_SETTLEMENT_LAG_SECONDS: i64 = 20;

/// Gate one normalized objective score on completion of the current wave.
/// Partial activity is progress telemetry, not a scoreable completed run.
pub(crate) fn leaderboard_tick_core(activity_rate: f64, objective_rate: f64) -> f64 {
    if activity_rate < 1.0 || objective_rate == 0.0 {
        0.0
    } else {
        objective_rate.clamp(0.0, 1.0)
    }
}

/// Normalize a completed team's native score against the best completed score
/// in the same finalized wave. The fixed concave curve keeps close fields
/// competitive without allowing roster size to dilute anyone's score.
pub(crate) fn leaderboard_relative_performance(core_rate: f64, best_rate: f64) -> f64 {
    if core_rate <= 0.0 || best_rate <= 0.0 {
        0.0
    } else {
        (core_rate / best_rate)
            .clamp(0.0, 1.0)
            .powf(API_RELATIVE_PERFORMANCE_EXPONENT)
    }
}

/// Validate the one optional Crown directly from normalized integer evidence.
/// Cross multiplication keeps admission and materialization identical even
/// when equal ratios use different numerators and denominators.
pub(crate) fn leaderboard_crown_is_valid(
    rows: impl IntoIterator<Item = (i64, i64, i64, i64, bool)>,
) -> bool {
    let rows: Vec<_> = rows.into_iter().collect();
    let completed: Vec<_> = rows
        .iter()
        .filter(|row| row.1 > 0 && row.3 > 0 && row.0 == row.1 && row.2 > 0)
        .collect();
    let crowns: Vec<_> = rows.iter().filter(|row| row.4).collect();
    let Some(best) = completed.iter().copied().max_by(|left, right| {
        (i128::from(left.2) * i128::from(right.3)).cmp(&(i128::from(right.2) * i128::from(left.3)))
    }) else {
        return crowns.is_empty();
    };
    let leaders = completed
        .iter()
        .filter(|row| {
            i128::from(row.2) * i128::from(best.3) == i128::from(best.2) * i128::from(row.3)
        })
        .count();
    if leaders > 1 {
        return crowns.is_empty();
    }
    crowns.len() == 1
        && crowns[0].0 == crowns[0].1
        && crowns[0].2 > 0
        && crowns[0].3 > 0
        && i128::from(crowns[0].2) * i128::from(best.3)
            == i128::from(best.2) * i128::from(crowns[0].3)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct KothApiEvidence {
    pub(super) participation_id: i32,
    pub(super) activity_earned: i64,
    pub(super) activity_possible: i64,
    pub(super) objective_earned: i64,
    pub(super) objective_possible: i64,
    pub(super) objective_count: i16,
    pub(super) is_crown: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct KothApiWaveSnapshot {
    pub(super) wave_id: String,
    pub(super) ended_at_ms: i64,
    pub(super) rows: Vec<KothApiEvidence>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct KothApiSnapshot {
    pub(super) hash: [u8; 32],
    pub(super) objective_schema_hash: [u8; 32],
    pub(super) waves: Vec<KothApiWaveSnapshot>,
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
    objective_schema_hash: Vec<u8>,
    wave_id: Option<String>,
    ended_at_ms: Option<i64>,
    participation_id: Option<i32>,
    activity_earned: Option<i64>,
    activity_possible: Option<i64>,
    objective_earned: Option<i64>,
    objective_possible: Option<i64>,
    objective_count: Option<i16>,
    is_crown: Option<bool>,
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
        r#"SELECT snapshot.snapshot_hash, snapshot.objective_schema_hash,
                  wave.wave_id,
                  (EXTRACT(EPOCH FROM wave.ended_at) * 1000)::bigint AS ended_at_ms,
                  score.participation_id,
                  score.activity_earned, score.activity_possible,
                  score.objective_earned, score.objective_possible,
                  score.objective_count, score.is_crown
             FROM "KothApiSnapshots" snapshot
        LEFT JOIN "KothApiSnapshotWaves" wave
               ON wave.target_id = snapshot.target_id
        LEFT JOIN "KothApiSnapshotScores" score
               ON score.target_id = wave.target_id
              AND score.wave_id = wave.wave_id
            WHERE snapshot.target_id = $1
              AND snapshot.cycle_id = $2
              AND snapshot.reset_attempt = $3
              AND snapshot.container_id = $4
              AND snapshot.ad_round_id = $5
              AND snapshot.accepted_at >= $6
              AND snapshot.accepted_at < $7
            ORDER BY wave.ended_at, wave.wave_id, score.participation_id"#,
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
    let objective_schema_hash: [u8; 32] = match rows[0].objective_schema_hash.as_slice().try_into()
    {
        Ok(hash) => hash,
        Err(_) => {
            return KothApiSnapshotRead::Unavailable(
                "Leaderboard snapshot has an invalid objective schema digest".to_string(),
            )
        }
    };
    if rows
        .iter()
        .any(|row| row.objective_schema_hash.as_slice() != objective_schema_hash.as_slice())
    {
        return KothApiSnapshotRead::Unavailable(
            "Leaderboard objective schema changed during its read".to_string(),
        );
    }
    let mut waves = Vec::<KothApiWaveSnapshot>::new();
    for row in rows {
        let (Some(wave_id), Some(ended_at_ms)) = (row.wave_id, row.ended_at_ms) else {
            continue;
        };
        if waves.last().is_none_or(|wave| wave.wave_id != wave_id) {
            waves.push(KothApiWaveSnapshot {
                wave_id: wave_id.clone(),
                ended_at_ms,
                rows: Vec::new(),
            });
        }
        if let Some(participation_id) = row.participation_id {
            waves
                .last_mut()
                .expect("wave was inserted before its evidence")
                .rows
                .push(KothApiEvidence {
                    participation_id,
                    activity_earned: row.activity_earned.unwrap_or_default(),
                    activity_possible: row.activity_possible.unwrap_or(1),
                    objective_earned: row.objective_earned.unwrap_or_default(),
                    objective_possible: row.objective_possible.unwrap_or(1),
                    objective_count: row.objective_count.unwrap_or(1),
                    is_crown: row.is_crown.unwrap_or(false),
                });
        }
    }
    KothApiSnapshotRead::Observed(KothApiSnapshot {
        hash,
        objective_schema_hash,
        waves,
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
            objective_schema_hash: [7; 32],
            waves: vec![KothApiWaveSnapshot {
                wave_id: "wave-1".to_string(),
                ended_at_ms: 1,
                rows: vec![KothApiEvidence {
                    participation_id: 7,
                    activity_earned: value,
                    activity_possible: 10,
                    objective_earned: value,
                    objective_possible: 10,
                    objective_count: 1,
                    is_crown: true,
                }],
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
    fn leaderboard_tick_requires_both_play_channels() {
        assert_eq!(leaderboard_tick_core(0.0, 1.0), 0.0);
        assert_eq!(leaderboard_tick_core(0.5, 1.0), 0.0);
        assert_eq!(leaderboard_tick_core(1.0, 0.0), 0.0);
        assert_eq!(leaderboard_tick_core(1.0, 1.0), 1.0);
    }

    #[test]
    fn leaderboard_relative_curve_is_roster_independent() {
        assert_eq!(leaderboard_relative_performance(0.0, 1.0), 0.0);
        assert_eq!(leaderboard_relative_performance(1.0, 1.0), 1.0);
        let close = leaderboard_relative_performance(0.99, 1.0);
        assert!((close - 0.99_f64.powf(0.75)).abs() < 1e-12);
        assert_eq!(
            leaderboard_relative_performance(20.0 / 150.0, 1.0),
            (20.0_f64 / 150.0).powf(0.75)
        );
    }

    #[test]
    fn leaderboard_crown_uses_exact_ratios_and_requires_a_unique_leader() {
        assert!(leaderboard_crown_is_valid([
            (1, 1, 3, 3, true),
            (1, 1, 2, 3, false),
        ]));
        assert!(leaderboard_crown_is_valid([
            (1, 1, 1, 3, false),
            (1, 1, 2, 6, false),
        ]));
        assert!(!leaderboard_crown_is_valid([
            (1, 1, 1, 3, true),
            (1, 1, 2, 6, false),
        ]));
        assert!(!leaderboard_crown_is_valid([
            (1, 1, 3, 3, false),
            (1, 1, 2, 3, false),
        ]));
        assert!(leaderboard_crown_is_valid([
            (0, 1, 0, 1, false),
            (1, 1, 0, 1, false),
        ]));
    }
}
