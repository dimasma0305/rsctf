//! Shared authorization for organizer application operations.

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::CurrentUser;
use crate::models::data::game_manager;
use crate::utils::error::AppError;
use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};

/// Authorize a game-scoped edit. Succeeds when the caller is a platform admin,
/// or a co-organizer (a `game_manager` row exists for `(game_id, user.id)`).
/// Mirrors RSCTF `[RequireGameAdmin]` (`EventManager` membership OR the `Admin`
/// role), letting a manager edit THEIR game without platform-wide admin.
pub(crate) async fn manager_or_admin(
    st: &SharedState,
    user: &CurrentUser,
    game_id: i32,
) -> Result<(), AppError> {
    if user.is_admin() {
        return Ok(());
    }
    let is_manager = game_manager::Entity::find()
        .filter(game_manager::Column::GameId.eq(game_id))
        .filter(game_manager::Column::UserId.eq(user.id))
        .count(&st.db)
        .await?
        > 0;
    if is_manager {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}
