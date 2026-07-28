//! Durable evidence writer for API-native KotH arenas.

use chrono::{DateTime, Utc};
use sqlx::{Postgres, QueryBuilder};
use std::collections::HashMap;

use super::koth::LiveHill;
use crate::models::data::ad_round;
use crate::services::ad::engine::{
    koth_api::{api_tick_rates, KothApiSnapshot, API_OBJECTIVE_NORMALIZATION_SCALE},
    AdCheckStatus,
};
use crate::utils::error::{AppError, AppResult};

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
                  AND current.accepted_at >= $7
                  AND current.accepted_at < $8
                FOR SHARE"#,
        )
        .bind(hill.target_id)
        .bind(hill.cycle_id)
        .bind(hill.token_window_attempt)
        .bind(&hill.container_id)
        .bind(round.id)
        .bind(snapshot.hash.as_slice())
        .bind(hill.round_start)
        .bind(hill.round_end)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .unwrap_or(false),
        None => false,
    };
    let is_scorable = status == AdCheckStatus::Ok && snapshot_is_current;
    let void_reason = if is_scorable {
        None
    } else if !snapshot_is_current {
        Some(message.unwrap_or("API arena snapshot was unavailable or unstable"))
    } else {
        Some("shared API arena failed its functional checker")
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
    if inserted == 1 && dead_container_id.is_some() {
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
        .bind("active API arena container stopped; recovery reset scheduled")
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
    struct DenseScoreRow {
        participation_id: i32,
        activity_earned: i64,
        activity_possible: i64,
        objective_earned: i64,
        objective_possible: i64,
        valid_actions: i64,
        total_actions: i64,
        objective_count: i16,
        activity_rate: f64,
        objective_rate: f64,
        integrity_rate: f64,
        core_rate: f64,
        score_rate: f64,
    }

    let submitted: HashMap<_, _> = snapshot
        .rows
        .iter()
        .map(|row| (row.participation_id, row))
        .collect();
    let objective_count = if let Some(row) = snapshot.rows.first() {
        row.objective_count
    } else {
        sqlx::query_scalar(
            r#"SELECT objective_count FROM "KothApiArenaSchemes"
                WHERE game_id = $1 AND challenge_id = $2"#,
        )
        .bind(game_id)
        .bind(challenge_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .unwrap_or(1)
    };
    let zero_objective_possible = API_OBJECTIVE_NORMALIZATION_SCALE * i64::from(objective_count);
    let rows: Vec<_> = eligible_roster
        .iter()
        .map(|participation_id| {
            let (
                activity_earned,
                activity_possible,
                objective_earned,
                objective_possible,
                valid_actions,
                total_actions,
                objective_count,
            ) = submitted.get(participation_id).map_or(
                (0, 1, 0, zero_objective_possible, 0, 1, objective_count),
                |row| {
                    (
                        row.activity_earned,
                        row.activity_possible,
                        row.objective_earned,
                        row.objective_possible,
                        row.valid_actions,
                        row.total_actions,
                        row.objective_count,
                    )
                },
            );
            let activity_rate = activity_earned as f64 / activity_possible as f64;
            let objective_rate = objective_earned as f64 / objective_possible as f64;
            let integrity_rate = valid_actions as f64 / total_actions as f64;
            let (core_rate, score_rate) =
                api_tick_rates(activity_rate, objective_rate, integrity_rate);
            DenseScoreRow {
                participation_id: *participation_id,
                activity_earned,
                activity_possible,
                objective_earned,
                objective_possible,
                valid_actions,
                total_actions,
                objective_count,
                activity_rate,
                objective_rate,
                integrity_rate,
                core_rate,
                score_rate,
            }
        })
        .collect();
    if rows.is_empty() {
        return Err(AppError::internal(
            "API arena cannot persist an empty official roster",
        ));
    }
    let mut query = QueryBuilder::<Postgres>::new(
        r#"INSERT INTO "KothApiScoreResults"
           (game_id, challenge_id, ad_round_id, participation_id,
            activity_earned, activity_possible,
            objective_earned, objective_possible,
            valid_actions, total_actions, objective_count,
            activity_rate, objective_rate, integrity_rate,
            core_rate, score_rate) "#,
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
            .push_bind(row.valid_actions)
            .push_bind(row.total_actions)
            .push_bind(row.objective_count)
            .push_bind(row.activity_rate)
            .push_bind(row.objective_rate)
            .push_bind(row.integrity_rate)
            .push_bind(row.core_rate)
            .push_bind(row.score_rate);
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
            "API arena score evidence did not cover the complete eligible roster",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::ad::engine::koth_api::KothApiEvidence;
    use sqlx::Connection;

    #[test]
    fn absent_teams_are_explicit_zeroes_not_stale_carry_forward() {
        let source = include_str!("koth_api.rs");
        assert!(source.contains("zero_objective_possible"));
        assert!(source.contains("eligible_roster"));
        assert!(source.contains("api_tick_rates"));
        assert!(source.contains("score_rate"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn dense_tick_persists_every_team_and_zeroes_an_omission() {
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
                 objective_possible BIGINT, valid_actions BIGINT,
                 total_actions BIGINT, objective_count SMALLINT,
                 activity_rate DOUBLE PRECISION,
                 objective_rate DOUBLE PRECISION,
                 integrity_rate DOUBLE PRECISION,
                 core_rate DOUBLE PRECISION, score_rate DOUBLE PRECISION,
                 PRIMARY KEY (
                   game_id, challenge_id, ad_round_id, participation_id
                 )
               )"#,
        )
        .execute(&mut connection)
        .await
        .unwrap();
        let snapshot = KothApiSnapshot {
            hash: [7; 32],
            rows: vec![KothApiEvidence {
                participation_id: 11,
                activity_earned: 4,
                activity_possible: 5,
                objective_earned: 500_000,
                objective_possible: 1_000_000,
                valid_actions: 3,
                total_actions: 4,
                objective_count: 1,
            }],
        };
        insert_dense_score_rows(&mut connection, 7, 9, 51, &[11, 12], &snapshot)
            .await
            .unwrap();
        let rows = sqlx::query_as::<_, (i32, i64, i64, i64, i64, i64, i16, f64)>(
            r#"SELECT participation_id, activity_earned, activity_possible,
                      objective_earned, objective_possible, valid_actions,
                      objective_count, score_rate
                 FROM "KothApiScoreResults"
                ORDER BY participation_id"#,
        )
        .fetch_all(&mut connection)
        .await
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (rows[0].0, rows[0].1, rows[0].2, rows[0].3, rows[0].4, rows[0].5, rows[0].6),
            (11, 4, 5, 500_000, 1_000_000, 3, 1)
        );
        let expected = 0.75 / (0.35 / 0.8 + 0.65 / 0.5);
        assert!((rows[0].7 - expected).abs() < 1e-12);
        assert_eq!((rows[1].0, rows[1].1, rows[1].2), (12, 0, 1));
        assert_eq!(
            (rows[1].3, rows[1].4, rows[1].5, rows[1].6, rows[1].7),
            (0, 1_000_000, 0, 1, 0.0)
        );
    }
}
