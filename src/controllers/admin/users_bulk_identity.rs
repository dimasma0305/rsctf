//! Atomic identity and roster writes for the two admin bulk-user endpoints.
//!
//! Account selection, re-credentialing/creation, and optional team assignment
//! share one PostgreSQL transaction. The global registration advisory lock
//! makes the identity decision linearizable with public/OAuth registration and
//! the other admin identity writers on every replica.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::utils::enums::Role;
use crate::utils::error::{is_unique_violation, AppError, AppResult};

pub(super) struct ExplicitUserWrite<'a> {
    pub user_name: &'a str,
    pub normalized_user_name: &'a str,
    pub email: &'a str,
    pub normalized_email: &'a str,
    pub password_hash: &'a str,
    pub phone: Option<&'a str>,
    pub create_real_name: &'a str,
    pub create_std_number: &'a str,
    pub update_real_name: Option<&'a str>,
    pub update_std_number: Option<&'a str>,
    pub update_phone: Option<&'a str>,
    pub now: DateTime<Utc>,
}

pub(super) struct ImportUserWrite<'a> {
    pub email: &'a str,
    pub normalized_email: &'a str,
    pub base_user_name: &'a str,
    pub password_hash: &'a str,
    pub email_confirmed: bool,
    pub create_real_name: &'a str,
    pub create_std_number: &'a str,
    pub create_phone: Option<&'a str>,
    pub update_real_name: Option<&'a str>,
    pub update_std_number: Option<&'a str>,
    pub update_phone: Option<&'a str>,
    pub now: DateTime<Utc>,
}

pub(super) struct ImportCredentialWrite<'a> {
    pub cache: &'a dyn crate::services::cache::Cache,
    pub password: &'a str,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct ProvisionedUser {
    pub id: Uuid,
    pub user_name: String,
    pub security_stamp: String,
    pub team_id: Option<i32>,
    pub created: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ImportProvision {
    Provisioned(ProvisionedUser),
    Skipped(&'static str),
}

enum TeamTarget {
    Existing { id: i32, captain_id: Uuid },
    New(String),
}

fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

fn identity_write_error(error: sqlx::Error) -> AppError {
    if is_unique_violation(&error) {
        AppError::conflict("Username already taken")
    } else {
        database_error(error)
    }
}

pub(super) async fn registration_transaction(
    pool: &sqlx::PgPool,
) -> AppResult<sqlx::Transaction<'static, sqlx::Postgres>> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(crate::controllers::account::REGISTRATION_LOCK_ID)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
    Ok(transaction)
}

async fn prepare_team_target(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    team_name: Option<&str>,
    cached_team_id: Option<i32>,
) -> AppResult<Option<TeamTarget>> {
    let Some(team_name) = team_name else {
        return Ok(None);
    };
    let team_id = match cached_team_id {
        Some(id) => Some(id),
        None => sqlx::query_scalar(r#"SELECT id FROM "Teams" WHERE name = $1 ORDER BY id LIMIT 1"#)
            .bind(team_name)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(database_error)?,
    };
    let Some(team_id) = team_id else {
        return Ok(Some(TeamTarget::New(team_name.to_string())));
    };

    crate::utils::single_flight::acquire_transaction_advisory_lock(
        transaction,
        &format!("team-roster:{team_id}"),
    )
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let row: Option<(Uuid, bool)> = sqlx::query_as(
        r#"SELECT captain_id, deletion_pending FROM "Teams" WHERE id = $1 FOR UPDATE"#,
    )
    .bind(team_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?;
    let Some((captain_id, deletion_pending)) = row else {
        return Err(AppError::conflict("Team no longer exists"));
    };
    if deletion_pending {
        return Err(AppError::conflict("Team is being deleted"));
    }
    Ok(Some(TeamTarget::Existing {
        id: team_id,
        captain_id,
    }))
}

async fn assign_team(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    target: Option<TeamTarget>,
    user_id: Uuid,
) -> AppResult<Option<i32>> {
    let Some(target) = target else {
        return Ok(None);
    };
    match target {
        TeamTarget::Existing { id, captain_id } => {
            let already_member: bool = captain_id == user_id
                || sqlx::query_scalar(
                    r#"SELECT EXISTS(SELECT 1 FROM "TeamMembers"
                                      WHERE team_id = $1 AND user_id = $2)"#,
                )
                .bind(id)
                .bind(user_id)
                .fetch_one(&mut **transaction)
                .await
                .map_err(database_error)?;
            if !already_member {
                let member_count: i64 = sqlx::query_scalar(
                    r#"SELECT COUNT(*)::bigint FROM (
                           SELECT captain_id AS user_id FROM "Teams" WHERE id = $1
                           UNION
                           SELECT user_id FROM "TeamMembers" WHERE team_id = $1
                       ) roster"#,
                )
                .bind(id)
                .fetch_one(&mut **transaction)
                .await
                .map_err(database_error)?;
                if member_count >= crate::controllers::team::MAX_TEAM_MEMBERS as i64 {
                    return Err(AppError::bad_request("Team is full"));
                }
                sqlx::query(
                    r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES ($1, $2)
                       ON CONFLICT (team_id, user_id) DO NOTHING"#,
                )
                .bind(id)
                .bind(user_id)
                .execute(&mut **transaction)
                .await
                .map_err(database_error)?;
            }
            Ok(Some(id))
        }
        TeamTarget::New(name) => {
            let captained: i64 =
                sqlx::query_scalar(r#"SELECT COUNT(*)::bigint FROM "Teams" WHERE captain_id = $1"#)
                    .bind(user_id)
                    .fetch_one(&mut **transaction)
                    .await
                    .map_err(database_error)?;
            if captained >= crate::controllers::team::MAX_TEAMS_ALLOWED as i64 {
                return Err(AppError::bad_request("Exceeded team creation limit"));
            }
            let id: i32 = sqlx::query_scalar(
                r#"INSERT INTO "Teams"
                     (name, bio, avatar_hash, locked, deletion_pending, invite_token, captain_id)
                   VALUES ($1, NULL, NULL, FALSE, FALSE, $2, $3)
                RETURNING id"#,
            )
            .bind(name)
            .bind(crate::utils::codec::random_hex(16))
            .bind(user_id)
            .fetch_one(&mut **transaction)
            .await
            .map_err(database_error)?;
            sqlx::query(r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES ($1, $2)"#)
                .bind(id)
                .bind(user_id)
                .execute(&mut **transaction)
                .await
                .map_err(database_error)?;
            Ok(Some(id))
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn insert_user(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: Uuid,
    user_name: &str,
    normalized_user_name: &str,
    email: &str,
    normalized_email: &str,
    email_confirmed: bool,
    password_hash: &str,
    security_stamp: &str,
    phone: Option<&str>,
    real_name: &str,
    std_number: &str,
    now: DateTime<Utc>,
) -> AppResult<()> {
    sqlx::query(
        r#"INSERT INTO "AspNetUsers"
             (id, user_name, normalized_user_name, email, normalized_email,
              email_confirmed, password_hash, security_stamp, concurrency_stamp,
              phone_number, phone_number_confirmed, two_factor_enabled, lockout_end,
              lockout_enabled, access_failed_count, role, ip, browser_fingerprint,
              last_signed_in_utc, last_visited_utc, register_time_utc, bio,
              real_name, std_number, exercise_visible, avatar_hash)
           VALUES
             ($1, $2, $3, $4, $5, $6, $7, $8, $9,
              $10, FALSE, FALSE, NULL, FALSE, 0, $11, '0.0.0.0', NULL,
              $12, $12, $12, '', $13, $14, TRUE, NULL)"#,
    )
    .bind(id)
    .bind(user_name)
    .bind(normalized_user_name)
    .bind(email)
    .bind(normalized_email)
    .bind(email_confirmed)
    .bind(password_hash)
    .bind(security_stamp)
    .bind(Uuid::new_v4().to_string())
    .bind(phone)
    .bind(Role::User as i16)
    .bind(now)
    .bind(real_name)
    .bind(std_number)
    .execute(&mut **transaction)
    .await
    .map_err(identity_write_error)?;
    Ok(())
}

pub(super) async fn provision_explicit_user(
    pool: &sqlx::PgPool,
    write: ExplicitUserWrite<'_>,
    team_name: Option<&str>,
    cached_team_id: Option<i32>,
) -> AppResult<ProvisionedUser> {
    let mut transaction = registration_transaction(pool).await?;
    // Team/roster lock precedes the account row lock, matching ordinary joins.
    let team_target = prepare_team_target(&mut transaction, team_name, cached_team_id).await?;
    let matches: Vec<(Uuid, i16)> = sqlx::query_as(
        r#"SELECT id, role FROM "AspNetUsers"
            WHERE normalized_user_name = $1 OR normalized_email = $2
            ORDER BY id
            FOR UPDATE"#,
    )
    .bind(write.normalized_user_name)
    .bind(write.normalized_email)
    .fetch_all(&mut *transaction)
    .await
    .map_err(database_error)?;
    if matches.len() > 1 {
        return Err(AppError::conflict(
            "Username and email belong to different users",
        ));
    }

    let security_stamp = Uuid::new_v4().to_string();
    let (id, created) = match matches.first().copied() {
        Some((id, role)) => {
            if role == Role::Admin as i16 || role == Role::Banned as i16 {
                return Err(AppError::bad_request(
                    "Administrator or banned accounts cannot be updated by batch import",
                ));
            }
            sqlx::query(
                r#"UPDATE "AspNetUsers"
                      SET user_name = $1,
                          normalized_user_name = $2,
                          email = $3,
                          normalized_email = $4,
                          password_hash = $5,
                          security_stamp = $6,
                          real_name = COALESCE($7, real_name),
                          std_number = COALESCE($8, std_number),
                          phone_number = COALESCE($9, phone_number)
                    WHERE id = $10"#,
            )
            .bind(write.user_name)
            .bind(write.normalized_user_name)
            .bind(write.email)
            .bind(write.normalized_email)
            .bind(write.password_hash)
            .bind(&security_stamp)
            .bind(write.update_real_name)
            .bind(write.update_std_number)
            .bind(write.update_phone)
            .bind(id)
            .execute(&mut *transaction)
            .await
            .map_err(identity_write_error)?;
            (id, false)
        }
        None => {
            let id = Uuid::now_v7();
            insert_user(
                &mut transaction,
                id,
                write.user_name,
                write.normalized_user_name,
                write.email,
                write.normalized_email,
                true,
                write.password_hash,
                &security_stamp,
                write.phone,
                write.create_real_name,
                write.create_std_number,
                write.now,
            )
            .await?;
            (id, true)
        }
    };
    let team_id = assign_team(&mut transaction, team_target, id).await?;
    transaction.commit().await.map_err(database_error)?;
    Ok(ProvisionedUser {
        id,
        user_name: write.user_name.to_string(),
        security_stamp,
        team_id,
        created,
    })
}

async fn unique_user_name(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    base: &str,
) -> AppResult<String> {
    let mut suffix = 1_u64;
    loop {
        let candidate = if suffix == 1 {
            base.to_string()
        } else {
            format!("{base}.{suffix}")
        };
        let occupied: bool = sqlx::query_scalar(
            r#"SELECT EXISTS(SELECT 1 FROM "AspNetUsers" WHERE normalized_user_name = $1)"#,
        )
        .bind(candidate.to_uppercase())
        .fetch_one(&mut **transaction)
        .await
        .map_err(database_error)?;
        if !occupied {
            return Ok(candidate);
        }
        suffix = suffix.saturating_add(1);
    }
}

pub(super) async fn provision_import_user(
    pool: &sqlx::PgPool,
    write: ImportUserWrite<'_>,
    credential: ImportCredentialWrite<'_>,
    team_name: Option<&str>,
    cached_team_id: Option<i32>,
) -> AppResult<ImportProvision> {
    let mut transaction = registration_transaction(pool).await?;
    let team_target = prepare_team_target(&mut transaction, team_name, cached_team_id).await?;
    let matches: Vec<(Uuid, i16, Option<String>)> = sqlx::query_as(
        r#"SELECT id, role, user_name FROM "AspNetUsers"
            WHERE normalized_email = $1
            ORDER BY id
            FOR UPDATE"#,
    )
    .bind(write.normalized_email)
    .fetch_all(&mut *transaction)
    .await
    .map_err(database_error)?;
    if matches.len() > 1 {
        transaction.rollback().await.map_err(database_error)?;
        return Ok(ImportProvision::Skipped(
            "email belongs to multiple existing accounts",
        ));
    }

    let security_stamp = Uuid::new_v4().to_string();
    let (id, user_name, created) = match matches.into_iter().next() {
        Some((id, role, user_name)) => {
            if role == Role::Admin as i16 || role == Role::Banned as i16 {
                transaction.rollback().await.map_err(database_error)?;
                return Ok(ImportProvision::Skipped(
                    "administrator or banned accounts cannot be updated by import",
                ));
            }
            sqlx::query(
                r#"UPDATE "AspNetUsers"
                      SET password_hash = $1,
                          security_stamp = $2,
                          real_name = COALESCE($3, real_name),
                          std_number = COALESCE($4, std_number),
                          phone_number = COALESCE($5, phone_number)
                    WHERE id = $6"#,
            )
            .bind(write.password_hash)
            .bind(&security_stamp)
            .bind(write.update_real_name)
            .bind(write.update_std_number)
            .bind(write.update_phone)
            .bind(id)
            .execute(&mut *transaction)
            .await
            .map_err(identity_write_error)?;
            (id, user_name.unwrap_or_default(), false)
        }
        None => {
            let id = Uuid::now_v7();
            let user_name = unique_user_name(&mut transaction, write.base_user_name).await?;
            insert_user(
                &mut transaction,
                id,
                &user_name,
                &user_name.to_uppercase(),
                write.email,
                write.normalized_email,
                write.email_confirmed,
                write.password_hash,
                &security_stamp,
                write.create_phone,
                write.create_real_name,
                write.create_std_number,
                write.now,
            )
            .await?;
            (id, user_name, true)
        }
    };
    let team_id = assign_team(&mut transaction, team_target, id).await?;
    // Publish the plaintext before the identity transaction commits, while the
    // global registration lock and this user's row lock still serialize every
    // competing import/email mutation. Delivery revalidates id + email + stamp,
    // so a process crash that rolls the transaction back leaves only a
    // harmless stale cache value that fails closed.
    let publication = super::users_credentials::cache_import_credential(
        credential.cache,
        id,
        write.email,
        &user_name,
        &security_stamp,
        credential.password,
    )
    .await?;
    if let Err(error) = transaction.commit().await {
        super::users_credentials::rollback_import_credential_publication(
            credential.cache,
            &publication,
        )
        .await;
        return Err(database_error(error));
    }
    Ok(ImportProvision::Provisioned(ProvisionedUser {
        id,
        user_name,
        security_stamp,
        team_id,
        created,
    }))
}

#[cfg(test)]
#[path = "users_bulk_tests.rs"]
mod tests;
