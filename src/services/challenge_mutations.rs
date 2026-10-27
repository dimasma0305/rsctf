//! Shared mutation fence for application operations and HTTP editors.
use crate::app_state::SharedState;
use crate::models::data::game_challenge;
use crate::utils::error::{AppError, AppResult};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use sqlx::{Executor, Postgres};

pub(crate) async fn load_challenge(
    st: &SharedState,
    game_id: i32,
    c_id: i32,
) -> AppResult<game_challenge::Model> {
    game_challenge::Entity::find()
        .filter(game_challenge::Column::Id.eq(c_id))
        .filter(game_challenge::Column::GameId.eq(game_id))
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Challenge not found"))
}

pub(crate) const PENDING_MUTATION_SQL: &str = r#"SELECT challenge.deletion_pending, game.deletion_pending
         FROM "GameChallenges" challenge
         JOIN "Games" game ON game.id = challenge.game_id
        WHERE challenge.id = $1 AND challenge.game_id = $2
        FOR SHARE OF challenge, game"#;

pub(crate) fn validate_pending_mutation(state: Option<(bool, bool)>) -> AppResult<()> {
    match state {
        None => Err(AppError::not_found("Challenge not found")),
        Some((true, _)) | Some((_, true)) => {
            Err(AppError::conflict("Challenge or game is being deleted"))
        }
        Some((false, false)) => Ok(()),
    }
}

/// Retained transaction callers keep both identities stable through commit.
pub(crate) async fn reject_pending_mutation<'e, E>(
    executor: E,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<()>
where
    E: Executor<'e, Database = Postgres>,
{
    let state = sqlx::query_as::<_, (bool, bool)>(PENDING_MUTATION_SQL)
        .bind(challenge_id)
        .bind(game_id)
        .fetch_optional(executor)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    validate_pending_mutation(state)
}
