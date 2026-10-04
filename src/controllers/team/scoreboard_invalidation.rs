//! Post-commit scoreboard invalidation for account display-name changes.

use uuid::Uuid;

use crate::app_state::SharedState;
use crate::utils::enums::AnswerResult;
use crate::utils::error::{AppError, AppResult};

pub(crate) const USER_SCOREBOARD_GAMES_SQL: &str = r#"
    SELECT DISTINCT game_id
      FROM "Submissions"
     WHERE user_id = ANY($1)
       AND status = $2
     ORDER BY game_id
"#;

async fn user_scoreboard_game_ids(pool: &sqlx::PgPool, user_ids: &[Uuid]) -> AppResult<Vec<i32>> {
    if user_ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_scalar(USER_SCOREBOARD_GAMES_SQL)
        .bind(user_ids)
        .bind(AnswerResult::Accepted as i16)
        .fetch_all(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

/// Evict every standard/A&D/KotH/combined scoreboard bundle that may embed a
/// renamed solver. The accepted-submission lookup is set-wise and backed by a
/// user-first partial index, so admin batches perform one query rather than one
/// historical-team scan per account.
pub(crate) async fn flush_scoreboards_for_users(
    st: &SharedState,
    user_ids: &[Uuid],
) -> AppResult<()> {
    let game_ids = user_scoreboard_game_ids(st.pg(), user_ids).await?;
    super::flush_scoreboards_for_games(st, &game_ids).await;
    Ok(())
}

pub(crate) async fn flush_scoreboard_for_user(st: &SharedState, user_id: Uuid) -> AppResult<()> {
    flush_scoreboards_for_users(st, &[user_id]).await
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::*;

    #[test]
    fn username_invalidation_targets_only_accepted_solver_games() {
        assert!(USER_SCOREBOARD_GAMES_SQL.contains("user_id = ANY($1)"));
        assert!(USER_SCOREBOARD_GAMES_SQL.contains("status = $2"));
        assert!(USER_SCOREBOARD_GAMES_SQL.contains("SELECT DISTINCT game_id"));
    }

    #[test]
    fn every_supported_username_mutation_calls_the_shared_invalidator() {
        let self_update = include_str!("../account/mod.rs");
        let admin_update = include_str!("../admin/users_mutate.rs");
        let bulk_update = include_str!("../admin/users.rs");
        let bulk_identity = include_str!("../admin/users_bulk_identity.rs");
        assert!(self_update.contains("flush_scoreboard_for_user(&st, user.id)"));
        assert!(admin_update.contains("flush_scoreboard_for_user(&st, userid)"));
        assert!(bulk_update.contains("flush_scoreboards_for_users(&st, &renamed_user_ids)"));
        assert!(bulk_identity.contains("user_name_changed"));
        assert!(
            bulk_update
                .find("flush_scoreboards_for_users(&st, &renamed_user_ids)")
                .unwrap()
                < bulk_update.find("provision_result?;").unwrap(),
            "partial bulk commits must be invalidated before propagating a later-row error"
        );
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn ended_game_solver_rename_resolves_every_affected_board_once() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("username_scoreboard_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"CREATE TABLE "Games" (
                   id INTEGER PRIMARY KEY,
                   end_time_utc TIMESTAMPTZ NOT NULL
               );
               CREATE TABLE "Submissions" (
                   id BIGSERIAL PRIMARY KEY,
                   game_id INTEGER NOT NULL REFERENCES "Games"(id),
                   user_id UUID NOT NULL,
                   status SMALLINT NOT NULL
               );"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "Games" (id, end_time_utc) VALUES
                   (7, clock_timestamp() - INTERVAL '1 day'),
                   (8, clock_timestamp() - INTERVAL '1 day'),
                   (9, clock_timestamp() + INTERVAL '1 day')"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let renamed = Uuid::new_v4();
        let other = Uuid::new_v4();
        for (game_id, user_id, status) in [
            (7, renamed, AnswerResult::Accepted as i16),
            (7, renamed, AnswerResult::Accepted as i16),
            (8, renamed, AnswerResult::WrongAnswer as i16),
            (9, other, AnswerResult::Accepted as i16),
        ] {
            sqlx::query(
                r#"INSERT INTO "Submissions" (game_id, user_id, status)
                   VALUES ($1, $2, $3)"#,
            )
            .bind(game_id)
            .bind(user_id)
            .bind(status)
            .execute(&pool)
            .await
            .unwrap();
        }

        assert_eq!(
            user_scoreboard_game_ids(&pool, &[renamed]).await.unwrap(),
            vec![7]
        );

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
