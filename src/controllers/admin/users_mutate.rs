//! Admin user mutation handlers (update/delete/reset-password) — split from
//! users.rs to keep each file under the 1000-line rule.
use super::*;

fn role_change_requires_stamp_rotation(current: Role, requested: Option<Role>) -> bool {
    requested.is_some_and(|role| role != current)
}

/// `PUT /api/admin/users/{userid}` — mutate role / name / email / bio / etc.
pub async fn update_user(
    State(st): State<SharedState>,
    AdminUser(caller): AdminUser,
    Path(userid): Path<Uuid>,
    Json(model): Json<AdminUserInfoModel>,
) -> AppResult<MessageResponse> {
    let txn = crate::controllers::account::locked_registration_transaction(&st).await?;
    let target = user::Entity::find_by_id(userid)
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::not_found("User not found"))?;
    let revoke_shared = model.role == Some(Role::Banned) && target.role != Role::Banned;
    let rotate_stamp = role_change_requires_stamp_rotation(target.role, model.role);

    // Admin-war protection: an admin may edit their own profile, but may not
    // mutate a *fellow* admin (ban / demote / rename).
    if target.role == Role::Admin && caller.id != target.id {
        return Err(AppError::bad_request("Cannot modify another administrator"));
    }

    if target.role == Role::Admin
        && model.role.is_some_and(|role| role != Role::Admin)
        && user::Entity::find()
            .filter(user::Column::Role.eq(Role::Admin))
            .count(&txn)
            .await?
            <= 1
    {
        return Err(AppError::bad_request(
            "Cannot demote or ban the last administrator",
        ));
    }

    let mut am: user::ActiveModel = target.into();

    if let Some(name) = model.user_name {
        let name = name.trim().to_string();
        if !name.is_empty() {
            let norm = name.to_uppercase();
            if user::Entity::find()
                .filter(user::Column::NormalizedUserName.eq(norm.clone()))
                .filter(user::Column::Id.ne(userid))
                .one(&txn)
                .await?
                .is_some()
            {
                return Err(AppError::conflict("Username already taken"));
            }
            am.normalized_user_name = Set(Some(norm));
            am.user_name = Set(Some(name));
        }
    }
    if let Some(email) = model.email {
        let email = email.trim().to_lowercase();
        if !email.is_empty() {
            let norm = email.to_uppercase();
            if user::Entity::find()
                .filter(user::Column::NormalizedEmail.eq(norm.clone()))
                .filter(user::Column::Id.ne(userid))
                .one(&txn)
                .await?
                .is_some()
            {
                return Err(AppError::conflict("Email already registered"));
            }
            am.normalized_email = Set(Some(norm));
            am.email = Set(Some(email));
        }
    }
    if let Some(bio) = model.bio {
        am.bio = Set(bio);
    }
    if let Some(phone) = model.phone {
        am.phone_number = Set(Some(phone));
    }
    if let Some(real_name) = model.real_name {
        am.real_name = Set(real_name);
    }
    if let Some(std_number) = model.std_number {
        am.std_number = Set(std_number);
    }
    if let Some(email_confirmed) = model.email_confirmed {
        am.email_confirmed = Set(email_confirmed);
    }
    if let Some(role) = model.role {
        am.role = Set(role);
    }
    if rotate_stamp {
        am.security_stamp = Set(Some(Uuid::new_v4().to_string()));
    }

    am.update(&txn).await?;
    txn.commit().await?;
    if revoke_shared {
        for team_id in affected_team_ids(&st, userid).await? {
            let key = format!("team-roster:{team_id}");
            let _local = crate::utils::single_flight::coalesce(&key).await;
            let distributed =
                crate::utils::single_flight::PgAdvisoryLock::acquire(st.pg(), &key).await?;
            crate::controllers::team::revoke_team_shared_capabilities(&st, team_id).await?;
            distributed.release().await?;
        }
    }
    Ok(MessageResponse::ok(""))
}

/// `DELETE /api/admin/users/{userid}` — remove a user (with guard rails).
/// Returns the deleted user id as a string (RSCTF `string` success).
pub async fn delete_user(
    State(st): State<SharedState>,
    AdminUser(caller): AdminUser,
    Path(userid): Path<Uuid>,
) -> AppResult<RequestResponse<String>> {
    if caller.id == userid {
        return Err(AppError::bad_request("Cannot delete yourself"));
    }

    let target = load_user(&st, userid).await?;

    if target.role == Role::Admin {
        return Err(AppError::bad_request("Cannot delete another administrator"));
    }

    let is_captain = team::Entity::find()
        .filter(team::Column::CaptainId.eq(userid))
        .one(&st.db)
        .await?
        .is_some();
    if is_captain {
        return Err(AppError::bad_request(
            "Cannot delete a user who is a team captain",
        ));
    }

    let team_ids = affected_team_ids(&st, userid).await?;

    // ApiToken.Creator is ON DELETE RESTRICT — clear the user's tokens first.
    api_token::Entity::delete_many()
        .filter(api_token::Column::CreatorId.eq(userid))
        .exec(&st.db)
        .await?;

    for team_id in team_ids {
        let key = format!("team-roster:{team_id}");
        let _local = crate::utils::single_flight::coalesce(&key).await;
        let distributed =
            crate::utils::single_flight::PgAdvisoryLock::acquire(st.pg(), &key).await?;
        crate::controllers::team::revoke_team_shared_capabilities(&st, team_id).await?;
        team_member::Entity::delete_many()
            .filter(team_member::Column::TeamId.eq(team_id))
            .filter(team_member::Column::UserId.eq(userid))
            .exec(&st.db)
            .await?;
        user_participation::Entity::delete_many()
            .filter(user_participation::Column::TeamId.eq(team_id))
            .filter(user_participation::Column::UserId.eq(userid))
            .exec(&st.db)
            .await?;
        distributed.release().await?;
    }

    // Defensive cleanup for malformed legacy links without a resolvable team.
    team_member::Entity::delete_many()
        .filter(team_member::Column::UserId.eq(userid))
        .exec(&st.db)
        .await?;
    user_participation::Entity::delete_many()
        .filter(user_participation::Column::UserId.eq(userid))
        .exec(&st.db)
        .await?;

    user::Entity::delete_by_id(userid).exec(&st.db).await?;
    Ok(RequestResponse::ok(userid.to_string()))
}

async fn affected_team_ids(st: &SharedState, user_id: Uuid) -> AppResult<Vec<i32>> {
    let mut ids: std::collections::BTreeSet<i32> = team_member::Entity::find()
        .filter(team_member::Column::UserId.eq(user_id))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|member| member.team_id)
        .collect();
    ids.extend(
        user_participation::Entity::find()
            .filter(user_participation::Column::UserId.eq(user_id))
            .all(&st.db)
            .await?
            .into_iter()
            .map(|link| link.team_id),
    );
    Ok(ids.into_iter().collect())
}

/// `DELETE /api/admin/users/{userid}/password` — reset the user's password to a
/// freshly generated value and return the plaintext (RSCTF `string` success).
pub async fn reset_password(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(userid): Path<Uuid>,
) -> AppResult<RequestResponse<String>> {
    let password = generate_password();
    let hash = hash_password(&password)?;

    let txn = crate::controllers::account::locked_registration_transaction(&st).await?;
    let target = user::Entity::find_by_id(userid)
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::not_found("User not found"))?;
    if target.role == Role::Admin {
        return Err(AppError::bad_request(
            "Administrator passwords must be changed from the account security flow",
        ));
    }

    let mut am: user::ActiveModel = target.into();
    am.password_hash = Set(Some(hash));
    am.security_stamp = Set(Some(Uuid::new_v4().to_string()));
    am.update(&txn).await?;
    txn.commit().await?;

    Ok(RequestResponse::ok(password))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_role_transition_rotates_the_session_stamp() {
        assert!(role_change_requires_stamp_rotation(
            Role::User,
            Some(Role::Admin)
        ));
        assert!(role_change_requires_stamp_rotation(
            Role::Banned,
            Some(Role::User)
        ));
        assert!(!role_change_requires_stamp_rotation(
            Role::User,
            Some(Role::User)
        ));
        assert!(!role_change_requires_stamp_rotation(Role::User, None));
    }
}
