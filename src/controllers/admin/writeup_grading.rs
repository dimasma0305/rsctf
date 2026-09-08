//! Admin-only, non-polled review projection. Never mutates competition scores.
use std::collections::HashMap;

use axum::{
    extract::{Path, State},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    app_state::SharedState,
    controllers::game::{self, build_combined_scoreboard, build_scoreboard_cached},
    middlewares::privilege_authentication::AdminUser,
    utils::error::{AppError, AppResult},
};

const MAX_TEAMS: i64 = 10_000;
const MAX_CELLS: i64 = 100_000;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum GradeMode {
    Jeopardy,
    AttackDefense,
    KingOfTheHill,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GradeChallenge {
    challenge_id: i32,
    title: String,
    mode: GradeMode,
    earned_points: f64,
    overall_points: f64,
    percentage: Option<i16>,
    revision: i32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GradeTeam {
    participation_id: i32,
    team_id: i32,
    name: String,
    division_id: Option<i32>,
    division: Option<String>,
    writeup_url: Option<String>,
    original_score: f64,
    overall_eligible: bool,
    division_eligible: bool,
    challenges: Vec<GradeChallenge>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GradingBoard {
    #[serde(with = "crate::utils::datetime::millis")]
    generated_at: DateTime<Utc>,
    fully_settled: bool,
    teams: Vec<GradeTeam>,
}

#[derive(sqlx::FromRow)]
struct Participation {
    id: i32,
    team_id: i32,
    writeup_url: Option<String>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Grade {
    participation_id: i32,
    challenge_id: i32,
    percentage: Option<i16>,
    revision: i32,
}

async fn load_board(st: &SharedState, id: i32) -> AppResult<GradingBoard> {
    // Bound the complete private ranking: never silently rank a truncated roster.
    let (teams, challenges): (i64, i64) = sqlx::query_as(
        r#"SELECT (SELECT count(*) FROM "Participations" WHERE game_id=$1),
                  (SELECT count(*) FROM "GameChallenges" WHERE game_id=$1)"#,
    )
    .bind(id)
    .fetch_one(st.pg())
    .await
    .map_err(db_error)?;
    if teams > MAX_TEAMS || teams.saturating_mul(challenges) > MAX_CELLS {
        return Err(AppError::payload_too_large(
            "Writeup grading supports up to 10,000 teams and 100,000 team/challenge pairs",
        ));
    }
    let game = game::load_game_cached(st, id).await?;
    let combined = build_combined_scoreboard(st, &game, true).await?;
    let jeopardy = build_scoreboard_cached(st, &game, true).await?;
    let ad = if combined.modes.attack_defense.active {
        Some(game::ad::build_ad_scoreboard_cached(st, id, true).await?)
    } else {
        None
    };
    let koth = if combined.modes.koth.active {
        Some(game::koth::build_koth_scoreboard_cached(st, &game, true).await?)
    } else {
        None
    };
    let participations = sqlx::query_as::<_, Participation>(
        r#"SELECT p.id, p.team_id, CASE WHEN f.id IS NOT NULL
                  THEN '/assets/' || f.hash || '/' || f.name END AS writeup_url
             FROM "Participations" p LEFT JOIN "Files" f ON f.id=p.writeup_id
            WHERE p.game_id=$1 AND p.status=1 ORDER BY p.id LIMIT $2"#,
    )
    .bind(id)
    .bind(MAX_TEAMS + 1)
    .fetch_all(st.pg())
    .await
    .map_err(db_error)?;
    let parts: HashMap<_, _> = participations.into_iter().map(|p| (p.team_id, p)).collect();
    let grades: HashMap<_, _> = sqlx::query_as::<_, Grade>(
        r#"SELECT participation_id, challenge_id, percentage, revision FROM "WriteupGrades"
            WHERE game_id=$1 LIMIT $2"#,
    )
    .bind(id)
    .bind(MAX_CELLS + 1)
    .fetch_all(st.pg())
    .await
    .map_err(db_error)?
    .into_iter()
    .map(|g| ((g.participation_id, g.challenge_id), g))
    .collect();
    let titles: HashMap<_, _> = jeopardy
        .challenges
        .values()
        .flatten()
        .map(|c| (c.id, c.title.clone()))
        .collect();
    let jeopardy_teams: HashMap<_, _> = jeopardy.items.iter().map(|t| (t.id, t)).collect();
    let ad_teams: HashMap<_, _> = ad
        .as_ref()
        .map(|b| b.teams.iter().map(|t| (t.team_id, t)).collect())
        .unwrap_or_default();
    let koth_teams: HashMap<_, _> = koth
        .as_ref()
        .map(|b| b.teams.iter().map(|t| (t.team_id, t)).collect())
        .unwrap_or_default();
    let mut teams = Vec::new();
    for team in combined.items {
        let Some(part) = parts.get(&team.id) else {
            continue;
        };
        let mut cells = Vec::new();
        let mut append = |mode: GradeMode, values: Vec<(i32, f64)>, contribution: f64| {
            let sum: f64 = values.iter().map(|(_, score)| score.max(0.0)).sum();
            for (challenge_id, earned_points) in values {
                let Some(title) = titles.get(&challenge_id) else {
                    continue;
                };
                let grade = grades.get(&(part.id, challenge_id));
                cells.push(GradeChallenge {
                    challenge_id,
                    title: title.clone(),
                    mode: mode.clone(),
                    earned_points,
                    overall_points: if sum > 0.0 {
                        contribution * earned_points / sum
                    } else {
                        0.0
                    },
                    percentage: grade.and_then(|g| g.percentage),
                    revision: grade.map_or(0, |g| g.revision),
                });
            }
        };
        if let Some(row) = jeopardy_teams.get(&team.id) {
            append(
                GradeMode::Jeopardy,
                row.solved_challenges
                    .iter()
                    .map(|c| (c.id, f64::from(c.score)))
                    .collect(),
                team.components.jeopardy.score * combined.modes.jeopardy.weight,
            );
        }
        if let Some(row) = ad_teams.get(&team.id) {
            append(
                GradeMode::AttackDefense,
                row.services
                    .iter()
                    .filter(|c| c.settled_points > 0.0)
                    .map(|c| (c.challenge_id, c.settled_points))
                    .collect(),
                team.components.attack_defense.score * combined.modes.attack_defense.weight,
            );
        }
        if let Some(row) = koth_teams.get(&team.id) {
            append(
                GradeMode::KingOfTheHill,
                row.hills
                    .iter()
                    .filter(|c| c.settled_points > 0.0)
                    .map(|c| (c.challenge_id, c.settled_points))
                    .collect(),
                team.components.koth.score * combined.modes.koth.weight,
            );
        }
        // Distribute the Overall board's final rounding across its cells too,
        // so grading every contribution zero leaves exactly zero points.
        let total: f64 = cells.iter().map(|c| c.overall_points).sum();
        if total > 0.0 {
            for cell in &mut cells {
                cell.overall_points *= team.score / total;
            }
        }
        teams.push(GradeTeam {
            participation_id: part.id,
            team_id: team.id,
            name: team.name,
            division_id: team.division_id,
            division: team.division,
            writeup_url: part.writeup_url.clone(),
            original_score: team.score,
            overall_eligible: team.rank > 0,
            division_eligible: team.division_rank.is_some(),
            challenges: cells,
        });
    }
    Ok(GradingBoard {
        generated_at: combined.generated_at,
        fully_settled: combined.fully_settled,
        teams,
    })
}

pub async fn get_grading(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<i32>,
) -> AppResult<Response> {
    Ok(private_response(load_board(&st, id).await?))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveGrade {
    percentage: Option<i16>,
    expected_revision: i32,
    operation_id: Uuid,
}

impl SaveGrade {
    fn validate(&self) -> AppResult<()> {
        if self.percentage.is_some_and(|v| !(0..=100).contains(&v))
            || self.expected_revision < 0
            || self.operation_id.is_nil()
        {
            return Err(AppError::bad_request("Use a whole percentage from 0 to 100, a valid revision and a non-empty operationId"));
        }
        Ok(())
    }
}

const SAVE_SQL: &str = r#"
INSERT INTO "WriteupGrades" AS grade
    (game_id, participation_id, challenge_id, percentage, revision, operation_id, graded_by)
SELECT $1, p.id, c.id, $4, 1, $6, $7
  FROM "Participations" p JOIN "GameChallenges" c ON c.game_id=p.game_id
 WHERE p.game_id=$1 AND p.id=$2 AND c.id=$3 AND p.status=1
   AND ($5=0 OR EXISTS (SELECT 1 FROM "WriteupGrades" WHERE game_id=$1 AND participation_id=$2 AND challenge_id=$3))
ON CONFLICT (game_id, participation_id, challenge_id) DO UPDATE
SET percentage=EXCLUDED.percentage,
    revision=CASE WHEN grade.operation_id=$6 THEN grade.revision ELSE grade.revision+1 END,
    updated_at=CASE WHEN grade.operation_id=$6 THEN grade.updated_at ELSE now() END,
    operation_id=$6, graded_by=$7
WHERE (grade.revision=$5 AND grade.operation_id<>$6)
   OR (grade.operation_id=$6 AND grade.percentage IS NOT DISTINCT FROM $4)
RETURNING participation_id, challenge_id, percentage, revision
"#;

async fn persist_grade(
    pool: &sqlx::PgPool,
    ids: (i32, i32, i32),
    user: Uuid,
    model: &SaveGrade,
) -> AppResult<Grade> {
    model.validate()?;
    sqlx::query_as::<_, Grade>(SAVE_SQL).bind(ids.0).bind(ids.1).bind(ids.2)
        .bind(model.percentage).bind(model.expected_revision).bind(model.operation_id).bind(user)
        .fetch_optional(pool).await.map_err(db_error)?
        .ok_or_else(|| AppError::conflict("This grade changed or the team/challenge is no longer eligible. Refresh before saving again."))
}

fn db_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

pub async fn save_grade(
    State(st): State<SharedState>,
    AdminUser(admin): AdminUser,
    Path(ids): Path<(i32, i32, i32)>,
    Json(model): Json<SaveGrade>,
) -> AppResult<Response> {
    model.validate()?;
    let board = load_board(&st, ids.0).await?;
    if !board.teams.iter().any(|t| {
        t.participation_id == ids.1 && t.challenges.iter().any(|c| c.challenge_id == ids.2)
    }) {
        return Err(AppError::bad_request(
            "Only a scored challenge on this team's official record can be graded",
        ));
    }
    Ok(private_response(
        persist_grade(st.pg(), ids, admin.id, &model).await?,
    ))
}

fn private_response(value: impl Serialize) -> Response {
    (
        [(axum::http::header::CACHE_CONTROL, "private, no-store")],
        Json(value),
    )
        .into_response()
}

#[cfg(test)]
mod tests;
