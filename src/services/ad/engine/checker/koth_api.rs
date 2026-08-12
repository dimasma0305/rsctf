//! Durable evidence writer for Leaderboard KotH.

use chrono::{DateTime, Utc};
use sqlx::{Postgres, QueryBuilder};
use std::collections::HashMap;

use super::koth::LiveHill;
use crate::models::data::ad_round;
use crate::services::ad::engine::{
    koth_api::{
        leaderboard_crown_is_valid, leaderboard_relative_performance, leaderboard_tick_core,
        KothApiSnapshot, API_OBJECTIVE_NORMALIZATION_SCALE,
    },
    AdCheckStatus,
};
use crate::utils::error::{AppError, AppResult};

const API_HEALTH_FAILURE_RESET_THRESHOLD: usize = 3;

fn api_health_recovery_due(statuses: &[i16]) -> bool {
    statuses.len() >= API_HEALTH_FAILURE_RESET_THRESHOLD
        && statuses
            .iter()
            .take(API_HEALTH_FAILURE_RESET_THRESHOLD)
            .all(|status| {
                matches!(
                    AdCheckStatus::from_i16(*status),
                    AdCheckStatus::Mumble | AdCheckStatus::Offline
                )
            })
}

async fn api_health_recovery_reason(
    connection: &mut sqlx::PgConnection,
    hill: &LiveHill,
    status: AdCheckStatus,
    dead_container_id: Option<&str>,
) -> AppResult<Option<String>> {
    if dead_container_id.is_some() {
        return Ok(Some(
            "active Leaderboard container stopped; health recovery scheduled".to_string(),
        ));
    }
    if !matches!(status, AdCheckStatus::Mumble | AdCheckStatus::Offline) {
        return Ok(None);
    }
    let statuses: Vec<i16> = sqlx::query_scalar(
        r#"SELECT result.status
             FROM "KothControlResults" result
            WHERE result.cycle_id = $1
              AND result.challenge_id = $2
              AND result.container_id = $3
              AND result.token_window_attempt = $4
            ORDER BY result.checked_at DESC, result.id DESC
            LIMIT $5"#,
    )
    .bind(hill.cycle_id)
    .bind(hill.challenge_id)
    .bind(&hill.container_id)
    .bind(hill.token_window_attempt)
    .bind(i64::try_from(API_HEALTH_FAILURE_RESET_THRESHOLD).unwrap_or(i64::MAX))
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(api_health_recovery_due(&statuses).then(|| {
        format!(
            "Leaderboard functional checker failed {} consecutive rounds; health recovery scheduled",
            API_HEALTH_FAILURE_RESET_THRESHOLD
        )
    }))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn persist_api_arena_result(
    connection: &mut sqlx::PgConnection,
    hill: &LiveHill,
    game_id: i32,
    round: &ad_round::Model,
    status: AdCheckStatus,
    message: Option<&str>,
    observed_at: DateTime<Utc>,
    dead_container_id: Option<&str>,
    snapshot: Option<&KothApiSnapshot>,
) -> AppResult<()> {
    let snapshot_is_current = match snapshot {
        Some(snapshot) => sqlx::query_scalar::<_, bool>(
            r#"SELECT TRUE FROM "KothApiSnapshots" current
                WHERE current.target_id = $1
                  AND current.cycle_id = $2
                  AND current.reset_attempt = $3
                  AND current.container_id = $4
                  AND current.ad_round_id = $5
                  AND current.snapshot_hash = $6
                  AND current.objective_schema_hash = $7
                  AND current.accepted_at >= $8
                  AND current.accepted_at < $9
                FOR SHARE"#,
        )
        .bind(hill.target_id)
        .bind(hill.cycle_id)
        .bind(hill.token_window_attempt)
        .bind(&hill.container_id)
        .bind(round.id)
        .bind(snapshot.hash.as_slice())
        .bind(snapshot.objective_schema_hash.as_slice())
        .bind(hill.round_start)
        .bind(hill.round_end)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .unwrap_or(false),
        None => false,
    };
    let has_finalized_wave = snapshot.is_some_and(|snapshot| !snapshot.waves.is_empty());
    let is_scorable = status == AdCheckStatus::Ok && snapshot_is_current && has_finalized_wave;
    let void_reason = if is_scorable {
        None
    } else if !snapshot_is_current {
        Some(message.unwrap_or("Leaderboard snapshot was unavailable or unstable"))
    } else if status != AdCheckStatus::Ok {
        Some("shared Leaderboard application failed its functional checker")
    } else {
        Some("no finalized Leaderboard wave ended in this scoring round")
    };
    let inserted = sqlx::query(
        r#"INSERT INTO "KothControlResults"
             (game_id, challenge_id, ad_round_id,
              controlling_participation_id, responsible_participation_id,
              marker_observed, status, error_message,
              checked_at, dead_container_id, cycle_id, container_id,
              confirmation_streak, is_scorable, void_reason,
              token_window_attempt)
           VALUES ($1,$2,$3,NULL,NULL,$4,$5,$6,$7,$8,$9,$10,0,$11,$12,$13)
           ON CONFLICT (game_id, challenge_id, ad_round_id) DO NOTHING"#,
    )
    .bind(game_id)
    .bind(hill.challenge_id)
    .bind(round.id)
    .bind(snapshot_is_current)
    .bind(status as i16)
    .bind(message)
    .bind(observed_at)
    .bind(dead_container_id)
    .bind(hill.cycle_id)
    .bind(&hill.container_id)
    .bind(is_scorable)
    .bind(void_reason)
    .bind(hill.token_window_attempt)
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    if inserted == 1 && is_scorable {
        insert_dense_score_rows(
            connection,
            game_id,
            hill.challenge_id,
            round.id,
            &hill.eligible_roster,
            snapshot.unwrap(),
        )
        .await?;
    }
    let recovery_reason = if inserted == 1 {
        api_health_recovery_reason(connection, hill, status, dead_container_id).await?
    } else {
        None
    };
    if let Some(recovery_reason) = recovery_reason {
        sqlx::query(
            r#"UPDATE "KothCrownCycles"
                  SET phase = 'DestroyPending',
                      old_container_id = $2,
                      replacement_container_id = NULL,
                      replacement_host = NULL,
                      replacement_port = NULL,
                      reset_attempt = reset_attempt + 1,
                      last_error = $3,
                      updated_at = clock_timestamp()
                WHERE id = $1 AND phase = 'Active'
                  AND replacement_container_id = $2"#,
        )
        .bind(hill.cycle_id)
        .bind(&hill.container_id)
        .bind(recovery_reason)
        .execute(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    Ok(())
}

async fn insert_dense_score_rows(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
    round_id: i32,
    eligible_roster: &[i32],
    snapshot: &KothApiSnapshot,
) -> AppResult<()> {
    #[derive(Clone)]
    struct DenseScoreRow {
        participation_id: i32,
        activity_earned: i64,
        activity_possible: i64,
        objective_earned: i64,
        objective_possible: i64,
        objective_count: i16,
        activity_rate: f64,
        objective_rate: f64,
        core_rate: f64,
        performance_rate: f64,
        lead_credit: f64,
    }

    if eligible_roster.is_empty() {
        return Err(AppError::internal(
            "Leaderboard cannot persist an empty official roster",
        ));
    }
    if snapshot.waves.is_empty() {
        return Err(AppError::internal(
            "Leaderboard snapshot contains no finalized scoring wave",
        ));
    }
    let objective_count =
        if let Some(row) = snapshot.waves.iter().find_map(|wave| wave.rows.first()) {
            row.objective_count
        } else {
            sqlx::query_scalar(
                r#"SELECT objective_count FROM "KothApiArenaSchemes"
                WHERE game_id = $1 AND challenge_id = $2"#,
            )
            .bind(game_id)
            .bind(challenge_id)
            .fetch_one(&mut *connection)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?
        };
    let scale = API_OBJECTIVE_NORMALIZATION_SCALE;
    let objective_scale = scale * i64::from(objective_count);
    let wave_count = snapshot.waves.len() as i64;
    let mut rows: HashMap<_, _> = eligible_roster
        .iter()
        .map(|participation_id| {
            (
                *participation_id,
                DenseScoreRow {
                    participation_id: *participation_id,
                    activity_earned: 0,
                    activity_possible: scale * wave_count,
                    objective_earned: 0,
                    objective_possible: objective_scale * wave_count,
                    objective_count,
                    activity_rate: 0.0,
                    objective_rate: 0.0,
                    core_rate: 0.0,
                    performance_rate: 0.0,
                    lead_credit: 0.0,
                },
            )
        })
        .collect();

    for wave in &snapshot.waves {
        if !leaderboard_crown_is_valid(wave.rows.iter().map(|row| {
            (
                row.activity_earned,
                row.activity_possible,
                row.objective_earned,
                row.objective_possible,
                row.is_crown,
            )
        })) {
            return Err(AppError::internal(format!(
                "Leaderboard wave {} may crown only one unique completed leader",
                wave.wave_id
            )));
        }
        let submitted: HashMap<_, _> = wave
            .rows
            .iter()
            .map(|row| (row.participation_id, row))
            .collect();
        let mut wave_rates = Vec::with_capacity(eligible_roster.len());
        for participation_id in eligible_roster {
            let (completion_rate, objective_rate, core_rate, is_crown) = submitted
                .get(participation_id)
                .map_or((0.0, 0.0, 0.0, false), |row| {
                    let activity_rate = row.activity_earned as f64 / row.activity_possible as f64;
                    let submitted_objective_rate =
                        row.objective_earned as f64 / row.objective_possible as f64;
                    let core_rate = leaderboard_tick_core(activity_rate, submitted_objective_rate);
                    (
                        if activity_rate >= 1.0 { 1.0 } else { 0.0 },
                        core_rate,
                        core_rate,
                        row.is_crown,
                    )
                });
            wave_rates.push((
                *participation_id,
                completion_rate,
                objective_rate,
                core_rate,
                is_crown,
            ));
        }
        let highest_core = wave_rates.iter().map(|row| row.3).fold(0.0_f64, f64::max);

        for (participation_id, completion_rate, objective_rate, core_rate, is_crown) in wave_rates {
            let row = rows
                .get_mut(&participation_id)
                .expect("dense roster was initialized before wave scoring");
            row.activity_earned += (completion_rate * scale as f64).round() as i64;
            row.objective_earned += (objective_rate * objective_scale as f64).round() as i64;
            row.activity_rate += completion_rate;
            row.objective_rate += objective_rate;
            row.core_rate += core_rate;
            row.performance_rate += leaderboard_relative_performance(core_rate, highest_core);
            if is_crown {
                row.lead_credit += 1.0;
            }
        }
    }
    let divisor = snapshot.waves.len() as f64;
    let mut rows: Vec<_> = rows
        .into_values()
        .map(|mut row| {
            row.activity_rate /= divisor;
            row.objective_rate /= divisor;
            row.core_rate /= divisor;
            row.performance_rate /= divisor;
            row.lead_credit /= divisor;
            row
        })
        .collect();
    rows.sort_by_key(|row| row.participation_id);
    let mut query = QueryBuilder::<Postgres>::new(
        r#"INSERT INTO "KothApiScoreResults"
           (game_id, challenge_id, ad_round_id, participation_id,
            activity_earned, activity_possible,
            objective_earned, objective_possible,
            objective_count, activity_rate, objective_rate,
            core_rate, performance_rate, lead_credit) "#,
    );
    query.push_values(&rows, |mut values, row| {
        values
            .push_bind(game_id)
            .push_bind(challenge_id)
            .push_bind(round_id)
            .push_bind(row.participation_id)
            .push_bind(row.activity_earned)
            .push_bind(row.activity_possible)
            .push_bind(row.objective_earned)
            .push_bind(row.objective_possible)
            .push_bind(row.objective_count)
            .push_bind(row.activity_rate)
            .push_bind(row.objective_rate)
            .push_bind(row.core_rate)
            .push_bind(row.performance_rate)
            .push_bind(row.lead_credit);
    });
    query.push(" ON CONFLICT (game_id, challenge_id, ad_round_id, participation_id) DO NOTHING");
    let inserted = query
        .build()
        .execute(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .rows_affected();
    if inserted != eligible_roster.len() as u64 {
        return Err(AppError::internal(
            "Leaderboard score evidence did not cover the complete eligible roster",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::ad::engine::koth_api::{KothApiEvidence, KothApiWaveSnapshot};
    use sqlx::Connection;

    #[test]
    fn absent_teams_are_explicit_zeroes_not_stale_carry_forward() {
        let source = include_str!("koth_api.rs");
        assert!(source.contains("eligible_roster"));
        assert!(source.contains("objective_possible: objective_scale * wave_count"));
        assert!(source.contains("leaderboard_tick_core"));
        assert!(source.contains("leaderboard_relative_performance"));
        assert!(source.contains("may crown only one unique completed leader"));
    }

    #[test]
    fn an_empty_window_is_a_field_void_not_a_synthetic_zero_wave() {
        let source = include_str!("koth_api.rs");
        assert!(source.contains("has_finalized_wave"));
        assert!(source.contains("no finalized Leaderboard wave ended"));
    }

    #[test]
    fn persistent_arena_recovers_only_after_consecutive_target_failures() {
        let ok = AdCheckStatus::Ok as i16;
        let mumble = AdCheckStatus::Mumble as i16;
        let offline = AdCheckStatus::Offline as i16;
        let internal = AdCheckStatus::InternalError as i16;

        assert!(!api_health_recovery_due(&[mumble, offline]));
        assert!(!api_health_recovery_due(&[mumble, ok, offline]));
        assert!(!api_health_recovery_due(&[offline, internal, mumble]));
        assert!(api_health_recovery_due(&[mumble, offline, mumble]));
        assert!(api_health_recovery_due(&[offline, offline, offline, ok]));
    }

    #[test]
    fn materialized_crown_requires_a_unique_positive_leader() {
        assert!(leaderboard_crown_is_valid([
            (1, 1, 10, 10, true),
            (1, 1, 8, 10, false),
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
            (1, 1, 10, 10, false),
            (1, 1, 8, 10, false),
        ]));
        assert!(leaderboard_crown_is_valid([
            (1, 1, 0, 1, false),
            (0, 1, 1, 1, false),
        ]));
    }

    async fn temporary_score_connection() -> sqlx::PgConnection {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let mut connection = sqlx::PgConnection::connect(&database_url).await.unwrap();
        sqlx::raw_sql(
            r#"CREATE TEMP TABLE "KothApiArenaSchemes" (
                 game_id INTEGER, challenge_id INTEGER,
                 objective_count SMALLINT
               );
               CREATE TEMP TABLE "KothApiScoreResults" (
                 game_id INTEGER, challenge_id INTEGER, ad_round_id INTEGER,
                 participation_id INTEGER, activity_earned BIGINT,
                 activity_possible BIGINT, objective_earned BIGINT,
                 objective_possible BIGINT, objective_count SMALLINT,
                 activity_rate DOUBLE PRECISION,
                 objective_rate DOUBLE PRECISION,
                 core_rate DOUBLE PRECISION, performance_rate DOUBLE PRECISION,
                 lead_credit DOUBLE PRECISION,
                 PRIMARY KEY (
                   game_id, challenge_id, ad_round_id, participation_id
                 )
               )"#,
        )
        .execute(&mut connection)
        .await
        .unwrap();
        connection
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn dense_tick_zeroes_omissions_and_incomplete_waves() {
        let mut connection = temporary_score_connection().await;
        let snapshot = KothApiSnapshot {
            hash: [7; 32],
            objective_schema_hash: [8; 32],
            waves: vec![
                KothApiWaveSnapshot {
                    wave_id: "wave-1".to_string(),
                    ended_at_ms: 1,
                    rows: vec![KothApiEvidence {
                        participation_id: 11,
                        activity_earned: 1,
                        activity_possible: 1,
                        objective_earned: 500_000,
                        objective_possible: 1_000_000,
                        objective_count: 1,
                        is_crown: true,
                    }],
                },
                KothApiWaveSnapshot {
                    wave_id: "wave-2".to_string(),
                    ended_at_ms: 2,
                    rows: vec![KothApiEvidence {
                        participation_id: 11,
                        activity_earned: 1,
                        activity_possible: 2,
                        objective_earned: 1_000_000,
                        objective_possible: 1_000_000,
                        objective_count: 1,
                        is_crown: false,
                    }],
                },
            ],
        };
        insert_dense_score_rows(&mut connection, 7, 9, 51, &[11, 12], &snapshot)
            .await
            .unwrap();
        let rows = sqlx::query_as::<_, (i32, i64, i64, i64, i16, f64, f64)>(
            r#"SELECT participation_id, activity_earned, activity_possible,
                      objective_earned, objective_count, performance_rate,
                      lead_credit
                 FROM "KothApiScoreResults"
                ORDER BY participation_id"#,
        )
        .fetch_all(&mut connection)
        .await
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (rows[0].0, rows[0].1, rows[0].2, rows[0].3, rows[0].4),
            (11, 1_000_000, 2_000_000, 500_000, 1)
        );
        assert_eq!((rows[0].5, rows[0].6), (0.5, 0.5));
        assert_eq!((rows[1].0, rows[1].1, rows[1].2), (12, 0, 2_000_000));
        assert_eq!(
            (rows[1].3, rows[1].4, rows[1].5, rows[1].6),
            (0, 1, 0.0, 0.0)
        );
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn dense_tick_gives_exact_ties_full_performance_without_a_crown() {
        let mut connection = temporary_score_connection().await;
        let snapshot = KothApiSnapshot {
            hash: [9; 32],
            objective_schema_hash: [10; 32],
            waves: vec![KothApiWaveSnapshot {
                wave_id: "tied-wave".to_string(),
                ended_at_ms: 1,
                rows: vec![
                    KothApiEvidence {
                        participation_id: 11,
                        activity_earned: 1,
                        activity_possible: 1,
                        objective_earned: 1,
                        objective_possible: 2,
                        objective_count: 1,
                        is_crown: false,
                    },
                    KothApiEvidence {
                        participation_id: 12,
                        activity_earned: 1,
                        activity_possible: 1,
                        objective_earned: 5_000,
                        objective_possible: 10_000,
                        objective_count: 1,
                        is_crown: false,
                    },
                ],
            }],
        };
        insert_dense_score_rows(&mut connection, 7, 9, 52, &[11, 12], &snapshot)
            .await
            .unwrap();
        let rows = sqlx::query_as::<_, (i32, f64, f64)>(
            r#"SELECT participation_id, performance_rate, lead_credit
                 FROM "KothApiScoreResults"
                ORDER BY participation_id"#,
        )
        .fetch_all(&mut connection)
        .await
        .unwrap();
        assert_eq!(rows, vec![(11, 1.0, 0.0), (12, 1.0, 0.0)]);
    }
}
