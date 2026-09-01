//! Transition rules and serialized preconditions for admin user mutations.

use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::models::data::user;
use crate::utils::enums::Role;
use crate::utils::error::{AppError, AppResult};

pub(super) fn role_change_requires_stamp_rotation(current: Role, requested: Option<Role>) -> bool {
    requested.is_some_and(|role| role != current)
}

pub(super) fn role_request_requires_shared_revocation(requested: Option<Role>) -> bool {
    requested == Some(Role::Banned)
}

pub(super) fn unban_requires_prior_shared_revocation(
    current: Role,
    requested: Option<Role>,
) -> bool {
    current == Role::Banned && requested.is_some_and(|role| role != Role::Banned)
}

pub(super) fn email_change_requires_stamp_rotation(
    current_normalized_email: Option<&str>,
    requested_email: Option<&str>,
) -> bool {
    requested_email
        .map(str::trim)
        .filter(|email| !email.is_empty())
        .map(|email| email.to_uppercase())
        .is_some_and(|email| current_normalized_email != Some(email.as_str()))
}

pub(super) fn account_lifecycle_key(user_id: Uuid) -> String {
    format!("account-lifecycle:{user_id}")
}

pub(super) async fn affected_team_ids(pool: &sqlx::PgPool, user_id: Uuid) -> AppResult<Vec<i32>> {
    sqlx::query_scalar(
        r#"SELECT team_id FROM "TeamMembers" WHERE user_id = $1
           UNION
           SELECT team_id FROM "UserParticipations" WHERE user_id = $1
           UNION
           SELECT id AS team_id FROM "Teams" WHERE captain_id = $1
           ORDER BY team_id"#,
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

pub(super) async fn revoke_user_shared_teams(st: &SharedState, user_id: Uuid) -> AppResult<()> {
    for team_id in affected_team_ids(st.pg(), user_id).await? {
        let roster = crate::controllers::team::acquire_roster_mutation(st.pg(), team_id).await?;
        let parts = crate::controllers::team::revoke_team_shared_capabilities(st, team_id).await?;
        roster.release().await?;
        crate::controllers::team::invalidate_removed_membership_cache(st, user_id, &parts).await?;
    }
    Ok(())
}

pub(super) async fn validate_admin_update(
    transaction: &sea_orm::DatabaseTransaction,
    target: &user::Model,
    caller_id: Uuid,
    requested_role: Option<Role>,
) -> AppResult<()> {
    // Admin-war protection: an admin may edit their own profile, but may not
    // mutate a *fellow* admin (ban / demote / rename).
    if target.role == Role::Admin && caller_id != target.id {
        return Err(AppError::bad_request("Cannot modify another administrator"));
    }

    if target.role == Role::Admin
        && requested_role.is_some_and(|role| role != Role::Admin)
        && user::Entity::find()
            .filter(user::Column::Role.eq(Role::Admin))
            .count(transaction)
            .await?
            <= 1
    {
        return Err(AppError::bad_request(
            "Cannot demote or ban the last administrator",
        ));
    }
    Ok(())
}
