use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::MAX_TEAMS_ALLOWED;
use crate::services::anti_cheat;
use crate::utils::codec::random_hex;
use crate::utils::enums::Role;
use crate::utils::error::{AppError, AppResult};

/// Atomically revalidate the authenticated captain, enforce the captain limit,
/// and create both ownership rows. The exact JWT stamp binds this durable
/// mutation to the principal that passed authentication before any lock wait.
#[cfg(test)]
pub(crate) async fn create_team_rows(
    pool: &sqlx::PgPool,
    creator_id: Uuid,
    expected_security_stamp: &str,
    name: &str,
    bio: Option<&str>,
) -> AppResult<i32> {
    create_team_rows_replay(pool, creator_id, expected_security_stamp, name, bio, None).await
}

pub(crate) async fn create_team_rows_replay(
    pool: &sqlx::PgPool,
    creator_id: Uuid,
    expected_security_stamp: &str,
    name: &str,
    bio: Option<&str>,
    operation: Option<(Uuid, &str)>,
) -> AppResult<i32> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    // Creating a team links its already-authenticated captain without an
    // invite admission. Mark this trusted roster insert explicitly so the
    // rolling-upgrade TeamMembers fence rejects only legacy public joins.
    anti_cheat::mark_identity_neutral_insert(&mut transaction).await?;
    lock_acting_account(&mut transaction, creator_id, expected_security_stamp).await?;

    if let Some((operation_id, digest)) = operation {
        if let Some(result_id) = crate::services::create_operations::claim(
            &mut transaction,
            creator_id,
            "team",
            0,
            operation_id,
            digest,
        )
        .await?
        {
            let team_id = result_id
                .parse::<i32>()
                .map_err(|_| AppError::internal("invalid retained team create result"))?;
            transaction.commit().await.map_err(database_error)?;
            return Ok(team_id);
        }
    }

    let captained: i64 =
        sqlx::query_scalar(r#"SELECT COUNT(*)::bigint FROM "Teams" WHERE captain_id = $1"#)
            .bind(creator_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(database_error)?;
    if captained >= MAX_TEAMS_ALLOWED as i64 {
        return Err(AppError::bad_request("Exceeded team creation limit"));
    }

    let team_id: i32 = sqlx::query_scalar(
        r#"INSERT INTO "Teams"
             (name, bio, avatar_hash, locked, invite_token, captain_id)
           VALUES ($1, $2, NULL, FALSE, $3, $4)
        RETURNING id"#,
    )
    .bind(name)
    .bind(bio)
    .bind(random_hex(16))
    .bind(creator_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(database_error)?;
    sqlx::query(r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES ($1, $2)"#)
        .bind(team_id)
        .bind(creator_id)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
    if let Some((operation_id, _)) = operation {
        crate::services::create_operations::complete(
            &mut transaction,
            creator_id,
            "team",
            0,
            operation_id,
            &team_id.to_string(),
        )
        .await?;
    }
    transaction.commit().await.map_err(database_error)?;
    Ok(team_id)
}

/// Transfer captaincy while binding the acting captain to the exact live
/// account state represented by the request JWT. Both accounts are locked in
/// UUID order so concurrent cross-team transfers cannot invert row-lock order.
pub(crate) async fn transfer_captain_locked(
    transaction: &mut Transaction<'_, Postgres>,
    team_id: i32,
    current_captain_id: Uuid,
    expected_security_stamp: &str,
    new_captain_id: Uuid,
) -> AppResult<()> {
    let team: Option<(Uuid, bool, bool)> = sqlx::query_as(
        r#"SELECT captain_id, locked, deletion_pending
              FROM "Teams" WHERE id = $1"#,
    )
    .bind(team_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?;
    let Some((captain_id, locked, deletion_pending)) = team else {
        return Err(AppError::not_found("Team not found"));
    };
    if captain_id != current_captain_id {
        return Err(AppError::Forbidden);
    }
    if deletion_pending {
        return Err(AppError::conflict("Team is being deleted"));
    }
    if locked {
        let active_game: bool = sqlx::query_scalar(
            r#"SELECT EXISTS(
                   SELECT 1
                     FROM "Participations" participation
                     JOIN "Games" game ON game.id = participation.game_id
                    WHERE participation.team_id = $1
                      AND game.end_time_utc > clock_timestamp()
               )"#,
        )
        .bind(team_id)
        .fetch_one(&mut **transaction)
        .await
        .map_err(database_error)?;
        if active_game {
            return Err(AppError::bad_request("Team is locked by an active game"));
        }
    }
    let target_is_member: bool = new_captain_id == captain_id
        || sqlx::query_scalar(
            r#"SELECT EXISTS(
                   SELECT 1 FROM "TeamMembers"
                    WHERE team_id = $1 AND user_id = $2
               )"#,
        )
        .bind(team_id)
        .bind(new_captain_id)
        .fetch_one(&mut **transaction)
        .await
        .map_err(database_error)?;
    if !target_is_member {
        return Err(AppError::bad_request(
            "New captain must already be a team member",
        ));
    }

    let mut account_ids = vec![current_captain_id, new_captain_id];
    account_ids.sort_unstable();
    account_ids.dedup();
    let accounts = sqlx::query_as::<_, (Uuid, bool, i16, Option<String>)>(
        r#"SELECT id, email_confirmed, role, security_stamp
              FROM "AspNetUsers"
             WHERE id = ANY($1)
             ORDER BY id
               FOR UPDATE"#,
    )
    .bind(&account_ids)
    .fetch_all(&mut **transaction)
    .await
    .map_err(database_error)?;
    let acting = accounts
        .iter()
        .find(|account| account.0 == current_captain_id);
    if acting.is_none_or(|(_, confirmed, role, stamp)| {
        !*confirmed
            || *role == Role::Banned as i16
            || stamp.as_deref() != Some(expected_security_stamp)
    }) {
        return Err(AppError::Forbidden);
    }
    let target = accounts.iter().find(|account| account.0 == new_captain_id);
    if target.is_none_or(|(_, confirmed, role, _)| !*confirmed || *role == Role::Banned as i16) {
        return Err(AppError::bad_request("New captain not found"));
    }

    let captained: i64 =
        sqlx::query_scalar(r#"SELECT COUNT(*)::bigint FROM "Teams" WHERE captain_id = $1"#)
            .bind(new_captain_id)
            .fetch_one(&mut **transaction)
            .await
            .map_err(database_error)?;
    if captained >= MAX_TEAMS_ALLOWED as i64 {
        return Err(AppError::bad_request(
            "New captain already captains too many teams",
        ));
    }
    sqlx::query(r#"UPDATE "Teams" SET captain_id = $1 WHERE id = $2"#)
        .bind(new_captain_id)
        .bind(team_id)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
    Ok(())
}

async fn lock_acting_account(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    expected_security_stamp: &str,
) -> AppResult<()> {
    let account = sqlx::query_as::<_, (bool, i16, Option<String>)>(
        r#"SELECT email_confirmed, role, security_stamp
              FROM "AspNetUsers"
             WHERE id = $1
               FOR UPDATE"#,
    )
    .bind(user_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?;
    if account.is_none_or(|(confirmed, role, stamp)| {
        !confirmed
            || role == Role::Banned as i16
            || stamp.as_deref() != Some(expected_security_stamp)
    }) {
        return Err(AppError::Forbidden);
    }
    Ok(())
}

fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}
