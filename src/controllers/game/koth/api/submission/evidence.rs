use std::collections::HashMap;

use chrono::{DateTime, Utc};

use crate::controllers::game::koth::api_contract::NormalizedWave;
use crate::services::ad::engine::koth_api::leaderboard_crown_is_valid;
use crate::utils::enums::{ParticipationStatus, Role};
use crate::utils::error::{AppError, AppResult};

#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
pub(super) struct ResolvedInputRow {
    pub(super) participation_id: i32,
    pub(super) activity_earned: i64,
    pub(super) activity_possible: i64,
    pub(super) objective_earned: i64,
    pub(super) objective_possible: i64,
    pub(super) objective_count: i16,
    pub(super) is_crown: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ResolvedWave {
    pub(super) wave_id: String,
    pub(super) ended_at: DateTime<Utc>,
    pub(super) rows: Vec<ResolvedInputRow>,
}

#[derive(sqlx::FromRow)]
struct StoredSnapshotRow {
    wave_id: Option<String>,
    ended_at: Option<DateTime<Utc>>,
    participation_id: Option<i32>,
    activity_earned: Option<i64>,
    activity_possible: Option<i64>,
    objective_earned: Option<i64>,
    objective_possible: Option<i64>,
    objective_count: Option<i16>,
    is_crown: Option<bool>,
}

#[derive(sqlx::FromRow)]
struct CurrentCapabilityRow {
    participation_id: i32,
    token: String,
}

pub(super) fn validate_resolved_crowns(waves: &[ResolvedWave]) -> AppResult<()> {
    for wave in waves {
        if !leaderboard_crown_is_valid(wave.rows.iter().map(|row| {
            (
                row.activity_earned,
                row.activity_possible,
                row.objective_earned,
                row.objective_possible,
                row.is_crown,
            )
        })) {
            return Err(AppError::bad_request(format!(
                "Leaderboard wave {} may name a Crown only for one unique completed leader",
                wave.wave_id
            )));
        }
    }
    Ok(())
}

pub(super) fn ensure_finalized_waves_are_append_only(
    stored: &[ResolvedWave],
    proposed: &[ResolvedWave],
) -> AppResult<()> {
    if proposed.len() < stored.len() || proposed[..stored.len()] != *stored {
        return Err(AppError::conflict(
            "Leaderboard finalized waves are append-only and cannot be changed or removed",
        ));
    }
    Ok(())
}

fn required_stored<T>(value: Option<T>, field: &'static str) -> AppResult<T> {
    value.ok_or_else(|| {
        AppError::internal(format!("stored Leaderboard snapshot is missing {field}"))
    })
}

pub(super) async fn load_stored_waves(
    connection: &mut sqlx::PgConnection,
    target_id: i32,
    ad_round_id: i32,
    cycle_id: i64,
    reset_attempt: i32,
    container_id: &str,
) -> AppResult<Option<Vec<ResolvedWave>>> {
    let rows = sqlx::query_as::<_, StoredSnapshotRow>(
        r#"SELECT wave.wave_id, wave.ended_at, score.participation_id,
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
              AND snapshot.ad_round_id = $2
              AND snapshot.cycle_id = $3
              AND snapshot.reset_attempt = $4
              AND snapshot.container_id = $5
            ORDER BY wave.ended_at, wave.wave_id, score.participation_id"#,
    )
    .bind(target_id)
    .bind(ad_round_id)
    .bind(cycle_id)
    .bind(reset_attempt)
    .bind(container_id)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if rows.is_empty() {
        return Ok(None);
    }
    let mut waves = Vec::<ResolvedWave>::new();
    for row in rows {
        let (Some(wave_id), Some(ended_at)) = (row.wave_id, row.ended_at) else {
            continue;
        };
        if waves.last().is_none_or(|wave| wave.wave_id != wave_id) {
            waves.push(ResolvedWave {
                wave_id: wave_id.clone(),
                ended_at,
                rows: Vec::new(),
            });
        }
        if let Some(participation_id) = row.participation_id {
            waves
                .last_mut()
                .expect("wave was inserted before its stored evidence")
                .rows
                .push(ResolvedInputRow {
                    participation_id,
                    activity_earned: required_stored(row.activity_earned, "activity_earned")?,
                    activity_possible: required_stored(row.activity_possible, "activity_possible")?,
                    objective_earned: required_stored(row.objective_earned, "objective_earned")?,
                    objective_possible: required_stored(
                        row.objective_possible,
                        "objective_possible",
                    )?,
                    objective_count: required_stored(row.objective_count, "objective_count")?,
                    is_crown: required_stored(row.is_crown, "is_crown")?,
                });
        }
    }
    Ok(Some(waves))
}

pub(super) async fn resolve_current_capabilities(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
    waves: Vec<NormalizedWave>,
) -> AppResult<Vec<ResolvedWave>> {
    if waves.is_empty() {
        return Ok(Vec::new());
    }
    let capabilities = sqlx::query_as::<_, CurrentCapabilityRow>(
        r#"SELECT capability.participation_id, capability.token
             FROM "KothApiTeamTokens" capability
             JOIN "Participations" participation
               ON participation.id = capability.participation_id
              AND participation.game_id = $1
              AND participation.status = $3
             JOIN "Teams" team ON team.id = participation.team_id
             JOIN "KothOfficialConfigs" config ON config.game_id = $1
             JOIN LATERAL jsonb_array_elements(config.roster_snapshot) roster(item)
               ON participation.id = CASE jsonb_typeof(roster.item)
                    WHEN 'number' THEN (roster.item #>> '{}')::integer
                    WHEN 'object' THEN
                      NULLIF(roster.item->>'participationId', '')::integer
                    ELSE NULL
                  END
            WHERE capability.game_id = $1
              AND capability.challenge_id = $2
              AND NOT team.deletion_pending
              AND NOT EXISTS (
                    SELECT 1
                      FROM (
                          SELECT team.captain_id AS user_id
                          UNION
                          SELECT member.user_id
                            FROM "TeamMembers" member
                           WHERE member.team_id = team.id
                      ) roster_member
                      LEFT JOIN "AspNetUsers" account
                        ON account.id = roster_member.user_id
                     WHERE account.id IS NULL OR account.role = $4
              )
            ORDER BY capability.participation_id
            FOR SHARE OF capability"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(ParticipationStatus::Accepted as i16)
    .bind(Role::Banned as i16)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let capabilities: HashMap<_, _> = capabilities
        .into_iter()
        .map(|capability| {
            (
                crate::services::ad::koth_api_capability::token_hash(&capability.token),
                capability.participation_id,
            )
        })
        .collect();
    let mut resolved = Vec::with_capacity(waves.len());
    for wave in waves {
        let ended_at = DateTime::from_timestamp_millis(wave.ended_at_unix_ms)
            .ok_or_else(|| AppError::bad_request("Leaderboard wave timestamp is out of range"))?;
        let mut rows = Vec::with_capacity(wave.rows.len().min(capabilities.len()));
        for row in wave.rows {
            let Some(participation_id) = capabilities.get(&row.token_hash) else {
                continue;
            };
            rows.push(ResolvedInputRow {
                participation_id: *participation_id,
                activity_earned: row.activity_earned,
                activity_possible: row.activity_possible,
                objective_earned: row.objective_earned,
                objective_possible: row.objective_possible,
                objective_count: row.objective_count,
                is_crown: row.is_crown,
            });
        }
        rows.sort_by_key(|row| row.participation_id);
        resolved.push(ResolvedWave {
            wave_id: wave.wave_id,
            ended_at,
            rows,
        });
    }
    Ok(resolved)
}
