use super::*;

/// One bounded game-history row in the account stats response.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameStatItem {
    pub game_id: i32,
    pub game_title: String,
    #[serde(with = "crate::utils::datetime::millis")]
    pub end_time_utc: chrono::DateTime<Utc>,
    pub solves: i32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserStatsModel {
    pub total_solves: i32,
    pub total_first_bloods: i32,
    pub games_participated: i32,
    pub solves_by_category: std::collections::BTreeMap<String, i32>,
    pub games: Vec<GameStatItem>,
}

const MAX_ACCOUNT_GAME_HISTORY: i64 = 100;

#[derive(sqlx::FromRow)]
struct AccountStatsTotalRow {
    total_solves: i64,
    games_participated: i64,
    total_first_bloods: i64,
}

#[derive(sqlx::FromRow)]
struct AccountCategoryStatRow {
    category: i16,
    solves: i64,
}

#[derive(sqlx::FromRow)]
struct AccountGameStatRow {
    game_id: i32,
    game_title: String,
    end_time_utc: chrono::DateTime<Utc>,
    solves: i64,
}

fn bounded_stat_count(value: i64) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

/// Compact aggregates only: accepted answers never leave PostgreSQL.
pub async fn stats(
    State(st): State<SharedState>,
    user: CurrentUser,
) -> AppResult<RequestResponse<UserStatsModel>> {
    use crate::utils::enums::{AnswerResult, ChallengeCategory};
    use sea_orm::ActiveEnum;

    let mut connection = st
        .pg()
        .acquire()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let accepted = AnswerResult::Accepted as i16;
    let totals = sqlx::query_as::<_, AccountStatsTotalRow>(
        r#"WITH solved AS MATERIALIZED (
               SELECT DISTINCT submission.game_id, submission.challenge_id
                 FROM "Submissions" submission
                WHERE submission.user_id = $1 AND submission.status = $2
           )
           SELECT COUNT(*)::bigint AS total_solves,
                  COUNT(DISTINCT solved.game_id)::bigint AS games_participated,
                  (
                    SELECT COUNT(*)::bigint
                      FROM "FirstSolves" first_solve
                      JOIN "Submissions" submission
                        ON submission.id = first_solve.submission_id
                     WHERE submission.user_id = $1 AND submission.status = $2
                  ) AS total_first_bloods
             FROM solved"#,
    )
    .bind(user.id)
    .bind(accepted)
    .fetch_one(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let categories = sqlx::query_as::<_, AccountCategoryStatRow>(
        r#"SELECT challenge.category, COUNT(*)::bigint AS solves
             FROM (
                 SELECT DISTINCT challenge_id
                   FROM "Submissions"
                  WHERE user_id = $1 AND status = $2
             ) solved
             JOIN "GameChallenges" challenge ON challenge.id = solved.challenge_id
            GROUP BY challenge.category
            ORDER BY challenge.category"#,
    )
    .bind(user.id)
    .bind(accepted)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let game_rows = sqlx::query_as::<_, AccountGameStatRow>(
        r#"SELECT game.id AS game_id, game.title AS game_title,
                  game.end_time_utc, COUNT(*)::bigint AS solves
             FROM (
                 SELECT DISTINCT game_id, challenge_id
                   FROM "Submissions"
                  WHERE user_id = $1 AND status = $2
             ) solved
             JOIN "Games" game ON game.id = solved.game_id
            GROUP BY game.id, game.title, game.end_time_utc
            ORDER BY game.end_time_utc DESC, game.id DESC
            LIMIT $3"#,
    )
    .bind(user.id)
    .bind(accepted)
    .bind(MAX_ACCOUNT_GAME_HISTORY)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let solves_by_category = categories
        .into_iter()
        .map(|row| {
            let category = <ChallengeCategory as ActiveEnum>::try_from_value(&row.category)
                .map_err(|error| AppError::internal(error.to_string()))?;
            let name = serde_json::to_value(category)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .ok_or_else(|| AppError::internal("invalid challenge category"))?;
            Ok((name, bounded_stat_count(row.solves)))
        })
        .collect::<AppResult<std::collections::BTreeMap<_, _>>>()?;
    let games = game_rows
        .into_iter()
        .map(|row| GameStatItem {
            game_id: row.game_id,
            game_title: row.game_title,
            end_time_utc: row.end_time_utc,
            solves: bounded_stat_count(row.solves),
        })
        .collect();

    Ok(RequestResponse::ok(UserStatsModel {
        total_solves: bounded_stat_count(totals.total_solves),
        total_first_bloods: bounded_stat_count(totals.total_first_bloods),
        games_participated: bounded_stat_count(totals.games_participated),
        solves_by_category,
        games,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_history_and_wire_counts_are_bounded() {
        assert_eq!(MAX_ACCOUNT_GAME_HISTORY, 100);
        assert_eq!(bounded_stat_count(i64::MAX), i32::MAX);
    }
}
