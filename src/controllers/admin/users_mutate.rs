//! Admin user mutation handlers (update/delete/reset-password) — split from
//! users.rs to keep each file under the 1000-line rule.
use super::*;
use axum::extract::ConnectInfo;
use std::net::SocketAddr;

#[path = "users_reset_crypto.rs"]
mod reset_crypto;
use reset_crypto::{decrypt_admin_reset, encrypt_admin_reset};

#[path = "users_mutate_policy.rs"]
mod policy;
#[cfg(test)]
use policy::affected_team_ids;
use policy::{
    account_lifecycle_key, email_change_requires_stamp_rotation, revoke_user_shared_teams,
    role_change_requires_stamp_rotation, role_request_requires_shared_revocation,
    unban_requires_prior_shared_revocation, validate_admin_update,
};

/// Make an account fail closed before any roster snapshot or capability
/// teardown begins. Locking the account row closes the hand-off with team
/// invite acceptance: an accept either retains a share lock and commits before
/// this update (so the later snapshot sees it), or observes the banned role and
/// is rejected. The normalized email is returned from the same locked snapshot
/// so deletion can invalidate import-only plaintext without racing an email
/// mutation.
pub(crate) async fn fence_user_for_deletion(
    pool: &sqlx::PgPool,
    user_id: Uuid,
) -> AppResult<Option<String>> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(crate::controllers::account::REGISTRATION_LOCK_ID)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    // This is deliberately a separate statement from the team-link lookup. A
    // PostgreSQL statement keeps the snapshot it took before waiting for this
    // row lock; combining the EXISTS with FOR UPDATE could therefore miss a
    // roster link committed by the transaction that just released the row.
    let account: Option<(i16, Option<String>)> = sqlx::query_as(
        r#"SELECT role, normalized_email
             FROM "AspNetUsers"
            WHERE id = $1
            FOR UPDATE"#,
    )
    .bind(user_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((role, normalized_email)) = account else {
        return Err(AppError::not_found("User not found"));
    };
    if role == Role::Admin as i16 {
        return Err(AppError::bad_request("Cannot delete another administrator"));
    }
    // A new statement gets a fresh READ COMMITTED snapshot after the account
    // lock is held. Team creation/transfer and invite acceptance must take that
    // same account lock, so a roster link can neither be hidden by the stale
    // snapshot from before the wait nor appear after this check. Physical
    // deletion is deliberately limited to unteamed accounts: Ban is the safe
    // emergency revocation path, while deleting a live or historical roster row
    // would change competition evidence and can partially tear down a team.
    let association: Option<String> = sqlx::query_scalar(
        r#"SELECT CASE
               WHEN EXISTS(SELECT 1 FROM "Teams" WHERE captain_id = $1)
                 THEN 'captain'
               WHEN EXISTS(SELECT 1 FROM "TeamMembers" WHERE user_id = $1)
                 OR EXISTS(SELECT 1 FROM "UserParticipations" WHERE user_id = $1)
                 THEN 'member'
               ELSE NULL
           END"#,
    )
    .bind(user_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if association.as_deref() == Some("captain") {
        return Err(AppError::bad_request(
            "Cannot delete a user who is a team captain",
        ));
    }
    if association.is_some() {
        return Err(AppError::bad_request(
            "Cannot delete a user who belongs to a team",
        ));
    }

    sqlx::query(
        r#"UPDATE "AspNetUsers"
              SET role = $1, security_stamp = $2
            WHERE id = $3"#,
    )
    .bind(Role::Banned as i16)
    .bind(Uuid::new_v4().to_string())
    .bind(user_id)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(normalized_email)
}

async fn fence_user_identity_for_deletion(
    pool: &sqlx::PgPool,
    cache: &dyn crate::services::cache::Cache,
    user_id: Uuid,
) -> AppResult<()> {
    let normalized_email = fence_user_for_deletion(pool, user_id).await?;
    if let Some(email) = normalized_email {
        // The durable ban and stamp rotation now make the cached password
        // ineligible for delivery. Compare by immutable user id so an
        // overlapping account replacement can never lose its newer secret.
        super::users_credentials::invalidate_import_credential(cache, user_id, &email).await;
    }
    Ok(())
}

/// `PUT /api/admin/users/{userid}` — mutate role / name / email / bio / etc.
pub async fn update_user(
    State(st): State<SharedState>,
    AdminUser(caller): AdminUser,
    Path(userid): Path<Uuid>,
    Json(model): Json<AdminUserInfoModel>,
) -> AppResult<MessageResponse> {
    // Lock ordering is account lifecycle lease → registration transaction →
    // roster leases. Deletion uses the same order and retains this session
    // lease across external teardown, so an unban cannot reopen a fenced
    // account before deletion finishes.
    let account_lifecycle =
        crate::utils::single_flight::PgSessionAdvisoryLock::acquire_account_lifecycle(
            st.pg(),
            &account_lifecycle_key(userid),
        )
        .await?;
    let mut txn = crate::controllers::account::locked_registration_transaction(&st).await?;
    let mut target = user::Entity::find_by_id(userid)
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::not_found("User not found"))?;
    validate_admin_update(&txn, &target, caller.id, model.role).await?;

    // Keep the durable role Banned while retrying every partially completed
    // team-wide revocation. Release the registration transaction first: the
    // teardown acquires roster transactions and performs nested DB/external
    // work, so retaining this connection could starve a small pool. Reacquire
    // the cross-replica registration lock and revalidate all mutable state
    // before applying the requested unban or profile edits.
    if unban_requires_prior_shared_revocation(target.role, model.role) {
        txn.rollback().await?;
        revoke_user_shared_teams(&st, userid).await?;
        txn = crate::controllers::account::locked_registration_transaction(&st).await?;
        target = user::Entity::find_by_id(userid)
            .one(&txn)
            .await?
            .ok_or_else(|| AppError::not_found("User not found"))?;
        validate_admin_update(&txn, &target, caller.id, model.role).await?;
    }

    // Repeating an already-banned update is the retry path when an earlier
    // external VPN/BYOC teardown failed after the role change committed.
    let revoke_shared = role_request_requires_shared_revocation(model.role);
    let original_user_name = target.user_name.clone();
    let original_normalized_email = target.normalized_email.clone();
    let rotate_stamp = role_change_requires_stamp_rotation(target.role, model.role)
        || (target.email_confirmed && model.email_confirmed == Some(false))
        || email_change_requires_stamp_rotation(
            original_normalized_email.as_deref(),
            model.email.as_deref(),
        );
    let mut credential_email_to_invalidate = None;
    let mut user_name_changed = false;

    crate::controllers::account::validate_profile_fields(
        model.bio.as_deref(),
        model.phone.as_deref(),
        model.real_name.as_deref(),
        model.std_number.as_deref(),
    )?;

    let mut am: user::ActiveModel = target.into();

    if let Some(name) = model.user_name {
        let name = name.trim().to_string();
        if !name.is_empty() {
            if name.len() > crate::controllers::account::MAX_USER_NAME_BYTES {
                return Err(AppError::bad_request("Username is too long"));
            }
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
            user_name_changed = original_user_name.as_deref() != Some(name.as_str());
            am.normalized_user_name = Set(Some(norm));
            am.user_name = Set(Some(name));
        }
    }
    if let Some(email) = model.email {
        let email = email.trim().to_lowercase();
        if !email.is_empty() {
            if email.len() > crate::controllers::account::MAX_EMAIL_BYTES {
                return Err(AppError::bad_request("Email is too long"));
            }
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
            if original_normalized_email.as_deref() != Some(norm.as_str()) {
                credential_email_to_invalidate = original_normalized_email.clone();
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
    if user_name_changed {
        if let Err(error) = crate::controllers::team::flush_scoreboard_for_user(&st, userid).await {
            tracing::warn!(%error, user_id = %userid, "post-admin-rename scoreboard invalidation deferred");
        }
    }
    if let Some(old_email) = credential_email_to_invalidate {
        super::users_credentials::invalidate_import_credential(
            st.cache.as_ref(),
            userid,
            &old_email,
        )
        .await;
    }
    if revoke_shared {
        revoke_user_shared_teams(&st, userid).await?;
    }
    account_lifecycle.release().await?;
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

    // Retain a session-level fence for the entire slow teardown. Without it, a
    // concurrent admin update could unban the durable account fence after the
    // initial registration transaction commits, then create a late membership
    // that is absent from the deletion snapshot.
    let account_lifecycle =
        crate::utils::single_flight::PgSessionAdvisoryLock::acquire_account_lifecycle(
            st.pg(),
            &account_lifecycle_key(userid),
        )
        .await?;
    // Fresh sessions fail the live role/stamp check, and team invite acceptance
    // synchronizes through a share lock on this row. The fence also rejects any
    // existing team/participation link before changing the account.
    fence_user_identity_for_deletion(st.pg(), st.cache.as_ref(), userid).await?;

    // Delete the direct avatar owner and release its logical blob reference in
    // the same short transaction. The object purge remains post-commit and
    // retryable; an account crash can no longer leave a positive-reference
    // orphan that maintenance deliberately preserves.
    let avatar_hash = delete_user_rows_and_avatar(st.pg(), userid).await?;
    account_lifecycle.release().await?;
    if let Some(hash) = avatar_hash {
        crate::controllers::assets::invalidate_asset_gate(&st, &hash).await;
        if let Err(error) =
            crate::services::blob_refs::purge_if_unreferenced(st.pg(), st.storage.as_ref(), &hash)
                .await
        {
            tracing::warn!(%error, %hash, "deleted account avatar purge deferred");
        }
    }
    Ok(RequestResponse::ok(userid.to_string()))
}

async fn delete_user_rows_and_avatar(
    pool: &sqlx::PgPool,
    user_id: Uuid,
) -> AppResult<Option<String>> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let avatar_hash = sqlx::query_scalar::<_, Option<String>>(
        r#"SELECT avatar_hash FROM "AspNetUsers" WHERE id = $1 FOR UPDATE"#,
    )
    .bind(user_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("User not found"))?;
    if let Some(hash) = avatar_hash.as_deref() {
        crate::services::blob_refs::lock_direct_hashes_locked(
            &mut transaction,
            std::iter::once(hash),
        )
        .await?;
    }
    sqlx::query(r#"DELETE FROM "ApiTokens" WHERE creator_id = $1"#)
        .bind(user_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let removed = sqlx::query(r#"DELETE FROM "AspNetUsers" WHERE id = $1"#)
        .bind(user_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if removed.rows_affected() != 1 {
        return Err(AppError::conflict("Account deletion fence changed"));
    }
    if let Some(hash) = avatar_hash.as_deref() {
        crate::services::blob_refs::release_direct_hash_locked(&mut transaction, hash).await?;
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(avatar_hash)
}

/// `DELETE /api/admin/users/{userid}/password` — reset the user's password to a
/// freshly generated value and return the plaintext (RSCTF `string` success).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminPasswordResetQuery {
    pub operation_id: Uuid,
}

pub async fn reset_password(
    State(st): State<SharedState>,
    AdminUser(admin): AdminUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(userid): Path<Uuid>,
    Query(query): Query<AdminPasswordResetQuery>,
) -> AppResult<Response> {
    if query.operation_id.is_nil() {
        return Err(AppError::bad_request(
            "A valid password-reset operation ID is required",
        ));
    }
    let existing: Option<(Uuid, Uuid, i16, Option<Vec<u8>>, Option<Vec<u8>>, bool)> =
        sqlx::query_as(
            r#"SELECT user_id, requested_by, status, result_ciphertext, result_nonce,
                      result_expires_at_utc > clock_timestamp()
                 FROM "AdminPasswordResetOperations" WHERE operation_id = $1"#,
        )
        .bind(query.operation_id)
        .fetch_optional(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if let Some((bound_user, bound_admin, status, ciphertext, nonce, live)) = existing {
        if bound_user != userid || bound_admin != admin.id {
            return Err(AppError::conflict(
                "Password reset operation ID is bound to another request",
            ));
        }
        if status == 1 {
            if !live {
                return Err(AppError::not_found("Password reset result has expired"));
            }
            let password = decrypt_admin_reset(
                &st.config.jwt_secret,
                query.operation_id,
                userid,
                ciphertext.as_deref().unwrap_or_default(),
                nonce.as_deref().unwrap_or_default(),
            )?;
            return Ok(super::users_credentials::private_no_store(
                RequestResponse::ok(password),
            ));
        }
        if status == 2 {
            return Err(AppError::not_found("Password reset result has expired"));
        }
    }
    let source = crate::services::anti_cheat::client_ip(&headers, Some(peer.ip()))
        .unwrap_or_else(|| peer.ip().to_string());
    let source_scope = format!("credential-source:{source}");
    let mut credential_work = crate::services::credential_admission::try_acquire_scopes(
        st.pg(),
        crate::services::credential_admission::CredentialWorkClass::Interactive,
        &[&source_scope],
    )
    .await?;
    let target = user::Entity::find_by_id(userid)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("User not found"))?;
    if target.role == Role::Admin {
        return Err(AppError::bad_request(
            "Administrator passwords must be changed from the account security flow",
        ));
    }
    let target_stamp = target
        .security_stamp
        .clone()
        .ok_or_else(|| AppError::conflict("User credential state is unavailable"))?;
    let mut learned_scopes = vec![format!("credential-account:{userid}:{target_stamp}")];
    if let Some(normalized_email) = target.normalized_email.as_deref() {
        learned_scopes.push(format!("credential-email:{normalized_email}"));
    }
    let learned_scope_refs = learned_scopes
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    credential_work.try_add_scopes(&learned_scope_refs).await?;
    let lease_token = Uuid::new_v4();
    sqlx::query(
        r#"UPDATE "AdminPasswordResetOperations"
              SET status = 2
            WHERE user_id = $1 AND operation_id <> $2 AND status = 0
              AND lease_expires_at_utc <= clock_timestamp()"#,
    )
    .bind(userid)
    .bind(query.operation_id)
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let reserved = sqlx::query(
        r#"INSERT INTO "AdminPasswordResetOperations"
               (operation_id, user_id, requested_by, lease_token, lease_expires_at_utc)
           VALUES ($1, $2, $3, $4, clock_timestamp() + INTERVAL '45 seconds')
           ON CONFLICT (operation_id) DO UPDATE
             SET lease_token = EXCLUDED.lease_token,
                 lease_expires_at_utc = EXCLUDED.lease_expires_at_utc,
                 result_expires_at_utc = clock_timestamp() + INTERVAL '15 minutes'
           WHERE "AdminPasswordResetOperations".user_id = EXCLUDED.user_id
             AND "AdminPasswordResetOperations".requested_by = EXCLUDED.requested_by
             AND "AdminPasswordResetOperations".status = 0
             AND "AdminPasswordResetOperations".lease_expires_at_utc <= clock_timestamp()"#,
    )
    .bind(query.operation_id)
    .bind(userid)
    .bind(admin.id)
    .bind(lease_token)
    .execute(st.pg())
    .await;
    match reserved {
        Ok(result) if result.rows_affected() == 1 => {}
        Ok(_) => return Err(AppError::too_many_requests(1)),
        Err(error) if crate::utils::error::is_unique_violation(&error) => {
            return Err(AppError::too_many_requests(1));
        }
        Err(error) => return Err(AppError::internal(error.to_string())),
    }
    let password = generate_password();
    let hash = hash_password_async(password.clone()).await?;
    credential_work.ensure_owned().await?;
    let credential_email_to_invalidate = target.normalized_email.clone();
    let (ciphertext, nonce) =
        encrypt_admin_reset(&st.config.jwt_secret, query.operation_id, userid, &password)?;
    let mut transaction = st
        .pg()
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let updated = sqlx::query(
        r#"UPDATE "AspNetUsers"
              SET password_hash = $2, security_stamp = $3
            WHERE id = $1 AND security_stamp = $4 AND role <> $5"#,
    )
    .bind(userid)
    .bind(hash)
    .bind(Uuid::new_v4().to_string())
    .bind(&target_stamp)
    .bind(Role::Admin as i16)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    if updated != 1 {
        transaction
            .rollback()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Err(AppError::conflict("User credential changed concurrently"));
    }
    let completed = sqlx::query(
        r#"UPDATE "AdminPasswordResetOperations"
              SET status = 1, result_ciphertext = $3, result_nonce = $4,
                  completed_at_utc = clock_timestamp(),
                  result_expires_at_utc = clock_timestamp() + INTERVAL '15 minutes'
            WHERE operation_id = $1 AND lease_token = $2 AND status = 0"#,
    )
    .bind(query.operation_id)
    .bind(lease_token)
    .bind(ciphertext)
    .bind(nonce.as_slice())
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    if completed != 1 {
        transaction
            .rollback()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Err(AppError::conflict(
            "Password reset operation lost its lease",
        ));
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if let Some(email) = credential_email_to_invalidate {
        super::users_credentials::invalidate_import_credential(st.cache.as_ref(), userid, &email)
            .await;
    }

    Ok(super::users_credentials::private_no_store(
        RequestResponse::ok(password),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::cache::{Cache, InMemoryCache};
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

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

    #[test]
    fn repeated_ban_request_retries_shared_credential_revocation() {
        assert!(role_request_requires_shared_revocation(Some(Role::Banned)));
        assert!(!role_request_requires_shared_revocation(Some(Role::User)));
        assert!(!role_request_requires_shared_revocation(None));
    }

    #[test]
    fn effective_email_reassignment_rotates_the_session_stamp() {
        assert!(email_change_requires_stamp_rotation(
            Some("OLD@EXAMPLE.TEST"),
            Some("new@example.test")
        ));
        assert!(!email_change_requires_stamp_rotation(
            Some("SAME@EXAMPLE.TEST"),
            Some(" same@example.test ")
        ));
        assert!(!email_change_requires_stamp_rotation(
            Some("OLD@EXAMPLE.TEST"),
            Some("  ")
        ));
    }

    #[test]
    fn unban_cannot_precede_a_successful_revocation_retry() {
        assert!(unban_requires_prior_shared_revocation(
            Role::Banned,
            Some(Role::User)
        ));
        assert!(unban_requires_prior_shared_revocation(
            Role::Banned,
            Some(Role::Monitor)
        ));
        assert!(!unban_requires_prior_shared_revocation(
            Role::Banned,
            Some(Role::Banned)
        ));
        assert!(!unban_requires_prior_shared_revocation(
            Role::User,
            Some(Role::Monitor)
        ));
    }

    #[test]
    fn account_lifecycle_locks_are_scoped_to_immutable_user_ids() {
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        assert_eq!(
            account_lifecycle_key(first),
            format!("account-lifecycle:{first}")
        );
        assert_ne!(account_lifecycle_key(first), account_lifecycle_key(second));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn account_update_waits_for_a_cross_replica_deletion_lease() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url)
            .await
            .unwrap();
        let key = account_lifecycle_key(Uuid::new_v4());
        let deletion =
            crate::utils::single_flight::PgSessionAdvisoryLock::acquire_account_lifecycle(
                &pool, &key,
            )
            .await
            .unwrap();
        let mut update = tokio::spawn({
            let pool = pool.clone();
            let key = key.clone();
            async move {
                crate::utils::single_flight::PgSessionAdvisoryLock::acquire_account_lifecycle(
                    &pool, &key,
                )
                .await
            }
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut update)
                .await
                .is_err(),
            "admin update passed a live account-deletion lease"
        );

        deletion.release().await.unwrap();
        let acquired = tokio::time::timeout(std::time::Duration::from_secs(2), update)
            .await
            .expect("admin update remained blocked after deletion released its lease")
            .expect("account update task failed")
            .expect("account update could not acquire the released lease");
        acquired.release().await.unwrap();
        pool.close().await;
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn roster_mutation_waits_for_a_preexisting_replica_issuer() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to PostgreSQL");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url)
            .await
            .unwrap();
        let random = uuid::Uuid::new_v4();
        let team_id = i32::from_be_bytes(random.as_bytes()[..4].try_into().unwrap());
        let key = format!("team-roster:{team_id}");

        // No local gate: this lock represents an issuer on another replica.
        let issuer = crate::utils::single_flight::PgAdvisoryLock::acquire(&pool, &key)
            .await
            .unwrap();
        let mut fence = tokio::spawn({
            let pool = pool.clone();
            async move { crate::controllers::team::acquire_roster_mutation(&pool, team_id).await }
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut fence)
                .await
                .is_err(),
            "fence passed a live credential issuer"
        );

        issuer.release().await.unwrap();
        let acquired = tokio::time::timeout(std::time::Duration::from_secs(2), fence)
            .await
            .expect("fence remained blocked after issuer release")
            .expect("fence task failed")
            .expect("fence returned an application error");
        acquired.release().await.unwrap();
        pool.close().await;
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn delete_fence_bans_and_rotates_before_teardown() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("rsctf_user_delete_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin_pool)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "AspNetUsers" (
              id UUID PRIMARY KEY,
              role SMALLINT NOT NULL,
              security_stamp TEXT,
              normalized_email TEXT,
              avatar_hash TEXT
            );
            CREATE TABLE "Files" (
              id SERIAL PRIMARY KEY,
              hash TEXT NOT NULL UNIQUE,
              reference_count BIGINT NOT NULL
            );
            CREATE TABLE "Attachments" (
              id INTEGER PRIMARY KEY,
              local_file_id INTEGER
            );
            CREATE TABLE "Participations" (
              id INTEGER PRIMARY KEY,
              writeup_id INTEGER
            );
            CREATE TABLE "AdServiceSnapshots" (
              id BIGSERIAL PRIMARY KEY,
              local_file_id INTEGER NOT NULL
            );
            CREATE TABLE "ApiTokens" (
              id UUID PRIMARY KEY,
              creator_id UUID
            );
            CREATE TABLE "Teams" (
              id INTEGER PRIMARY KEY,
              captain_id UUID NOT NULL,
              avatar_hash TEXT
            );
            CREATE TABLE "Games" (
              id INTEGER PRIMARY KEY,
              poster_hash TEXT
            );
            CREATE TABLE "Configs" (
              config_key TEXT PRIMARY KEY,
              value TEXT
            );
            CREATE TABLE "GameChallenges" (
              id INTEGER PRIMARY KEY,
              original_archive_blob_path TEXT
            );
            CREATE TABLE "TeamMembers" (
              team_id INTEGER NOT NULL,
              user_id UUID NOT NULL
            );
            CREATE TABLE "UserParticipations" (
              team_id INTEGER NOT NULL,
              user_id UUID NOT NULL
            );
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let ordinary = Uuid::new_v4();
        let administrator = Uuid::new_v4();
        let captain = Uuid::new_v4();
        let roster_member = Uuid::new_v4();
        let participant = Uuid::new_v4();
        let newly_linked_member = Uuid::new_v4();
        for (id, role) in [
            (ordinary, Role::User),
            (administrator, Role::Admin),
            (captain, Role::User),
            (roster_member, Role::User),
            (participant, Role::User),
            (newly_linked_member, Role::User),
        ] {
            let normalized_email = (id == ordinary).then_some("ORDINARY@EXAMPLE.TEST");
            sqlx::query(
                r#"INSERT INTO "AspNetUsers" (id, role, security_stamp, normalized_email)
                   VALUES ($1, $2, 'old-stamp', $3)"#,
            )
            .bind(id)
            .bind(role as i16)
            .bind(normalized_email)
            .execute(&pool)
            .await
            .unwrap();
        }
        let avatar_hash = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        sqlx::query(r#"UPDATE "AspNetUsers" SET avatar_hash = $2 WHERE id = $1"#)
            .bind(ordinary)
            .bind(avatar_hash)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "Files" (hash, reference_count) VALUES ($1, 1)"#)
            .bind(avatar_hash)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "ApiTokens" (id, creator_id) VALUES ($1, $2)"#)
            .bind(Uuid::new_v4())
            .bind(ordinary)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "Teams" (id, captain_id) VALUES (1, $1)"#)
            .bind(captain)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO "TeamMembers" (team_id, user_id)
               VALUES (8, $1)"#,
        )
        .bind(roster_member)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "UserParticipations" (team_id, user_id)
               VALUES (9, $1)"#,
        )
        .bind(participant)
        .execute(&pool)
        .await
        .unwrap();

        let credential_cache = InMemoryCache::new();
        super::super::users_credentials::cache_import_credential(
            &credential_cache,
            ordinary,
            "ordinary@example.test",
            "ordinary",
            "old-stamp",
            "temporary-secret",
        )
        .await
        .unwrap();
        let credential_key =
            super::super::users_credentials::credential_cache_key("ordinary@example.test");
        assert!(credential_cache.get(&credential_key).await.is_some());

        fence_user_identity_for_deletion(&pool, &credential_cache, ordinary)
            .await
            .unwrap();
        assert!(
            credential_cache.get(&credential_key).await.is_none(),
            "deletion left the imported plaintext credential cached"
        );
        let fenced: (i16, Option<String>) =
            sqlx::query_as(r#"SELECT role, security_stamp FROM "AspNetUsers" WHERE id = $1"#)
                .bind(ordinary)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(fenced.0, Role::Banned as i16);
        assert_ne!(fenced.1.as_deref(), Some("old-stamp"));
        assert!(affected_team_ids(&pool, ordinary).await.unwrap().is_empty());
        assert_eq!(
            affected_team_ids(&pool, roster_member).await.unwrap(),
            vec![8]
        );
        assert_eq!(
            affected_team_ids(&pool, participant).await.unwrap(),
            vec![9]
        );
        assert_eq!(affected_team_ids(&pool, captain).await.unwrap(), vec![1]);

        let error = fence_user_for_deletion(&pool, administrator)
            .await
            .expect_err("administrator deletion must remain forbidden");
        assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
        let error = fence_user_for_deletion(&pool, captain)
            .await
            .expect_err("captain deletion must remain forbidden");
        assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
        for linked_user in [roster_member, participant] {
            let error = fence_user_for_deletion(&pool, linked_user)
                .await
                .expect_err("team-linked user deletion must remain forbidden");
            assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
            assert_eq!(
                error.to_string(),
                "Cannot delete a user who belongs to a team"
            );
            let unchanged: (i16, Option<String>) =
                sqlx::query_as(r#"SELECT role, security_stamp FROM "AspNetUsers" WHERE id = $1"#)
                    .bind(linked_user)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(unchanged.0, Role::User as i16);
            assert_eq!(unchanged.1.as_deref(), Some("old-stamp"));
        }

        // Reproduce the READ COMMITTED stale-snapshot window: the fence starts
        // its account-lock statement while another transaction owns the row,
        // then that owner links the target to a roster before releasing it. The
        // association query must run as a fresh statement after lock acquisition.
        let mut roster_assignment = pool.begin().await.unwrap();
        sqlx::query(r#"SELECT role FROM "AspNetUsers" WHERE id = $1 FOR UPDATE"#)
            .bind(newly_linked_member)
            .execute(&mut *roster_assignment)
            .await
            .unwrap();
        let mut racing_fence = tokio::spawn({
            let pool = pool.clone();
            async move { fence_user_for_deletion(&pool, newly_linked_member).await }
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut racing_fence)
                .await
                .is_err(),
            "deletion fence did not wait for the preexisting account lock"
        );
        sqlx::query(r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES (2, $1)"#)
            .bind(newly_linked_member)
            .execute(&mut *roster_assignment)
            .await
            .unwrap();
        roster_assignment.commit().await.unwrap();
        let error = tokio::time::timeout(std::time::Duration::from_secs(2), racing_fence)
            .await
            .expect("deletion fence remained blocked")
            .expect("deletion fence task failed")
            .expect_err("fresh membership was hidden by a stale statement snapshot");
        assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(
            sqlx::query_scalar::<_, i16>(r#"SELECT role FROM "AspNetUsers" WHERE id = $1"#)
                .bind(newly_linked_member)
                .fetch_one(&pool)
                .await
                .unwrap(),
            Role::User as i16
        );

        assert_eq!(
            delete_user_rows_and_avatar(&pool, ordinary)
                .await
                .unwrap()
                .as_deref(),
            Some(avatar_hash)
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(r#"SELECT reference_count FROM "Files""#)
                .fetch_one(&pool)
                .await
                .unwrap(),
            0,
            "account deletion leaked its avatar reference"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(r#"SELECT COUNT(*) FROM "ApiTokens""#)
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin_pool)
            .await
            .unwrap();
    }
}
