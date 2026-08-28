//! Revisioned team-profile writes and bounded scoreboard invalidation.

use sqlx::{PgConnection, Postgres, Transaction};
use uuid::Uuid;

use super::{flush_scoreboards_for_games, TeamInfoModel, TeamUpdateModel, TeamUserInfoModel};
use crate::app_state::SharedState;
use crate::utils::error::{is_unique_violation, AppError, AppResult};

const PROFILE_MUTATIONS_PER_MINUTE: i64 = 8;
const INVALIDATION_GAME_PAGE: i64 = 32;

#[derive(sqlx::FromRow)]
struct TeamProfileRow {
    id: i32,
    name: String,
    bio: Option<String>,
    avatar_hash: Option<String>,
    locked: bool,
    captain_id: Uuid,
    profile_revision: i64,
}

#[derive(sqlx::FromRow)]
struct TeamMemberRow {
    id: Uuid,
    user_name: Option<String>,
    bio: String,
    avatar_hash: Option<String>,
    captain: bool,
    real_name: String,
    student_number: String,
}

fn avatar_url(hash: Option<String>) -> Option<String> {
    hash.map(|hash| format!("/assets/{hash}/avatar"))
}

async fn load_profile(
    connection: &mut PgConnection,
    team_id: i32,
    lock: bool,
) -> AppResult<TeamProfileRow> {
    let sql = if lock {
        r#"SELECT id, name, bio, avatar_hash, locked, captain_id, profile_revision
             FROM "Teams" WHERE id = $1 FOR UPDATE"#
    } else {
        r#"SELECT id, name, bio, avatar_hash, locked, captain_id, profile_revision
             FROM "Teams" WHERE id = $1"#
    };
    sqlx::query_as(sql)
        .bind(team_id)
        .fetch_optional(connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .ok_or_else(|| AppError::not_found("Team not found"))
}

async fn load_info(
    connection: &mut PgConnection,
    profile: TeamProfileRow,
) -> AppResult<TeamInfoModel> {
    let members = sqlx::query_as::<_, TeamMemberRow>(
        r#"WITH member_ids AS (
               SELECT captain_id AS user_id FROM "Teams" WHERE id = $1
               UNION
               SELECT user_id FROM "TeamMembers" WHERE team_id = $1
           )
           SELECT users.id, users.user_name, users.bio, users.avatar_hash,
                  users.id = teams.captain_id AS captain,
                  users.real_name, users.std_number AS student_number
             FROM member_ids
             JOIN "AspNetUsers" users ON users.id = member_ids.user_id
             JOIN "Teams" teams ON teams.id = $1
            ORDER BY captain DESC, users.user_name, users.id"#,
    )
    .bind(profile.id)
    .fetch_all(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .into_iter()
    .map(|member| TeamUserInfoModel {
        id: member.id,
        user_name: member.user_name,
        bio: Some(member.bio),
        avatar: avatar_url(member.avatar_hash),
        captain: member.captain,
        real_name: member.real_name,
        student_number: member.student_number,
    })
    .collect();
    Ok(TeamInfoModel {
        id: profile.id,
        name: profile.name,
        bio: profile.bio,
        avatar: avatar_url(profile.avatar_hash),
        locked: profile.locked,
        profile_revision: profile.profile_revision,
        members: Some(members),
    })
}

fn request_digest(model: &TeamUpdateModel) -> AppResult<String> {
    let bytes = serde_json::to_vec(model)
        .map_err(|error| AppError::internal(format!("could not encode team update: {error}")))?;
    Ok(crate::utils::codec::sha256_hex(&bytes))
}

async fn replay_operation(
    connection: &mut PgConnection,
    operation_id: Uuid,
    team_id: i32,
    actor_user_id: Uuid,
    digest: &str,
) -> AppResult<Option<TeamInfoModel>> {
    let row = sqlx::query_as::<_, (i32, Uuid, String, sqlx::types::Json<TeamInfoModel>)>(
        r#"SELECT team_id, actor_user_id, request_digest, result
             FROM "TeamProfileOperations" WHERE operation_id = $1"#,
    )
    .bind(operation_id)
    .fetch_optional(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((stored_team, stored_actor, stored_digest, result)) = row else {
        return Ok(None);
    };
    if stored_team != team_id || stored_actor != actor_user_id || stored_digest != digest {
        return Err(AppError::conflict(
            "The team operation ID was already used for a different request",
        ));
    }
    Ok(Some(result.0))
}

async fn store_operation(
    connection: &mut PgConnection,
    operation_id: Uuid,
    team_id: i32,
    actor_user_id: Uuid,
    digest: &str,
    expected_revision: i64,
    result: &TeamInfoModel,
) -> AppResult<()> {
    let inserted = sqlx::query(
        r#"INSERT INTO "TeamProfileOperations"
               (operation_id, team_id, actor_user_id, request_digest,
                expected_revision, result_revision, result)
           VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
    )
    .bind(operation_id)
    .bind(team_id)
    .bind(actor_user_id)
    .bind(digest)
    .bind(expected_revision)
    .bind(result.profile_revision)
    .bind(sqlx::types::Json(result))
    .execute(connection)
    .await;
    match inserted {
        Ok(_) => Ok(()),
        Err(error) if is_unique_violation(&error) => Err(AppError::conflict(
            "The team operation ID was already used for a different request",
        )),
        Err(error) => Err(AppError::internal(error.to_string())),
    }
}

pub(super) async fn enforce_mutation_budget(
    connection: &mut PgConnection,
    team_id: i32,
    actor_user_id: Uuid,
) -> AppResult<()> {
    let recent = sqlx::query_scalar::<_, i64>(
        r#"SELECT COUNT(*) FROM "TeamProfileOperations"
            WHERE team_id = $1 AND actor_user_id = $2
              AND created_at_utc >= CURRENT_TIMESTAMP - INTERVAL '1 minute'"#,
    )
    .bind(team_id)
    .bind(actor_user_id)
    .fetch_one(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if recent >= PROFILE_MUTATIONS_PER_MINUTE {
        return Err(AppError::TooManyRequests);
    }
    Ok(())
}

pub(super) async fn replay_avatar_operation(
    connection: &mut PgConnection,
    operation_id: Uuid,
    team_id: i32,
    actor_user_id: Uuid,
    digest: &str,
) -> AppResult<Option<String>> {
    let row = sqlx::query_as::<_, (i32, Uuid, String, serde_json::Value)>(
        r#"SELECT team_id, actor_user_id, request_digest, result
             FROM "TeamProfileOperations" WHERE operation_id = $1"#,
    )
    .bind(operation_id)
    .fetch_optional(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((stored_team, stored_actor, stored_digest, result)) = row else {
        return Ok(None);
    };
    if stored_team != team_id || stored_actor != actor_user_id || stored_digest != digest {
        return Err(AppError::conflict(
            "The team operation ID was already used for a different request",
        ));
    }
    result
        .as_str()
        .map(|value| Some(value.to_string()))
        .ok_or_else(|| AppError::internal("avatar operation result has an invalid shape"))
}

pub(super) async fn store_avatar_operation(
    connection: &mut PgConnection,
    operation_id: Uuid,
    team_id: i32,
    actor_user_id: Uuid,
    digest: &str,
    expected_revision: i64,
    result_revision: i64,
    result: &str,
) -> AppResult<()> {
    let inserted = sqlx::query(
        r#"INSERT INTO "TeamProfileOperations"
               (operation_id, team_id, actor_user_id, request_digest,
                expected_revision, result_revision, result)
           VALUES ($1, $2, $3, $4, $5, $6, to_jsonb($7::TEXT))"#,
    )
    .bind(operation_id)
    .bind(team_id)
    .bind(actor_user_id)
    .bind(digest)
    .bind(expected_revision)
    .bind(result_revision)
    .bind(result)
    .execute(connection)
    .await;
    match inserted {
        Ok(_) => Ok(()),
        Err(error) if is_unique_violation(&error) => Err(AppError::conflict(
            "The team operation ID was already used for a different request",
        )),
        Err(error) => Err(AppError::internal(error.to_string())),
    }
}

pub(super) async fn enqueue_invalidation(
    connection: &mut PgConnection,
    team_id: i32,
    profile_revision: i64,
) -> AppResult<()> {
    sqlx::query(
        r#"INSERT INTO "TeamProfileInvalidations"
               (team_id, profile_revision, after_game_id)
           VALUES ($1, $2, 0)
           ON CONFLICT (team_id) DO UPDATE
             SET profile_revision = GREATEST(
                     "TeamProfileInvalidations".profile_revision,
                     EXCLUDED.profile_revision),
                 after_game_id = 0,
                 claim_id = NULL,
                 claim_expires_at_utc = NULL,
                 updated_at_utc = CURRENT_TIMESTAMP"#,
    )
    .bind(team_id)
    .bind(profile_revision)
    .execute(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

/// Apply one captain profile edit while retaining the caller's roster transaction.
pub(super) async fn update_locked(
    transaction: &mut Transaction<'_, Postgres>,
    team_id: i32,
    actor_user_id: Uuid,
    mut model: TeamUpdateModel,
) -> AppResult<TeamInfoModel> {
    let operation_id = model
        .operation_id
        .ok_or_else(|| AppError::bad_request("operationId is required"))?;
    if model.profile_revision < 0 {
        return Err(AppError::bad_request(
            "profileRevision must not be negative",
        ));
    }
    model.name = model.name.map(|name| name.trim().to_string());
    super::validate_team_profile(model.name.as_deref(), model.bio.as_deref())?;
    let digest = request_digest(&model)?;

    let preflight = load_profile(&mut **transaction, team_id, false).await?;
    if preflight.captain_id != actor_user_id {
        return Err(AppError::Forbidden);
    }
    if let Some(result) = replay_operation(
        &mut **transaction,
        operation_id,
        team_id,
        actor_user_id,
        &digest,
    )
    .await?
    {
        return Ok(result);
    }

    super::ensure_roster_change_allowed(transaction, team_id).await?;
    let current = load_profile(&mut **transaction, team_id, true).await?;
    if current.captain_id != actor_user_id {
        return Err(AppError::Forbidden);
    }
    if current.profile_revision != model.profile_revision {
        return Err(AppError::conflict(
            "Team profile changed in another request; reload and try again",
        ));
    }
    enforce_mutation_budget(&mut **transaction, team_id, actor_user_id).await?;

    let requested_name = model.name.unwrap_or_else(|| current.name.clone());
    let requested_bio = model.bio.or_else(|| current.bio.clone());
    if requested_name == current.name && requested_bio == current.bio {
        let result = load_info(&mut **transaction, current).await?;
        store_operation(
            &mut **transaction,
            operation_id,
            team_id,
            actor_user_id,
            &digest,
            model.profile_revision,
            &result,
        )
        .await?;
        return Ok(result);
    }

    let updated = sqlx::query_as::<_, TeamProfileRow>(
        r#"UPDATE "Teams"
              SET name = $2, bio = $3, profile_revision = profile_revision + 1
            WHERE id = $1 AND profile_revision = $4
        RETURNING id, name, bio, avatar_hash, locked, captain_id, profile_revision"#,
    )
    .bind(team_id)
    .bind(requested_name)
    .bind(requested_bio)
    .bind(model.profile_revision)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::conflict("Team profile changed; reload and try again"))?;
    let result = load_info(&mut **transaction, updated).await?;
    store_operation(
        &mut **transaction,
        operation_id,
        team_id,
        actor_user_id,
        &digest,
        model.profile_revision,
        &result,
    )
    .await?;
    enqueue_invalidation(&mut **transaction, team_id, result.profile_revision).await?;
    Ok(result)
}

#[derive(sqlx::FromRow)]
struct PendingInvalidation {
    team_id: i32,
    profile_revision: i64,
    after_game_id: i32,
}

/// Apply a bounded page of durable profile invalidations.
pub(crate) async fn process_profile_invalidations(state: &SharedState) -> AppResult<u64> {
    let claim_id = Uuid::new_v4();
    let mut transaction = crate::utils::database::begin_sqlx_transaction(state.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let effects = sqlx::query_as::<_, PendingInvalidation>(
        r#"WITH candidates AS (
               SELECT team_id FROM "TeamProfileInvalidations"
                WHERE claim_expires_at_utc IS NULL
                   OR claim_expires_at_utc <= CURRENT_TIMESTAMP
                ORDER BY updated_at_utc, team_id
                FOR UPDATE SKIP LOCKED LIMIT 32
           )
           UPDATE "TeamProfileInvalidations" effect
              SET claim_id = $1,
                  claim_expires_at_utc = CURRENT_TIMESTAMP + INTERVAL '2 minutes'
             FROM candidates
            WHERE effect.team_id = candidates.team_id
        RETURNING effect.team_id, effect.profile_revision, effect.after_game_id"#,
    )
    .bind(claim_id)
    .fetch_all(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let mut processed = 0;
    for effect in effects {
        let game_ids = sqlx::query_scalar::<_, i32>(
            r#"SELECT DISTINCT game_id FROM "Participations"
                WHERE team_id = $1 AND game_id > $2
                ORDER BY game_id LIMIT $3"#,
        )
        .bind(effect.team_id)
        .bind(effect.after_game_id)
        .bind(INVALIDATION_GAME_PAGE)
        .fetch_all(state.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        flush_scoreboards_for_games(state, &game_ids).await;

        if game_ids.len() < INVALIDATION_GAME_PAGE as usize {
            let deleted = sqlx::query(
                r#"DELETE FROM "TeamProfileInvalidations"
                    WHERE team_id = $1 AND profile_revision = $2 AND claim_id = $3"#,
            )
            .bind(effect.team_id)
            .bind(effect.profile_revision)
            .bind(claim_id)
            .execute(state.pg())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
            processed += deleted.rows_affected();
        } else if let Some(last_game_id) = game_ids.last() {
            sqlx::query(
                r#"UPDATE "TeamProfileInvalidations"
                      SET after_game_id = $3, claim_id = NULL, claim_expires_at_utc = NULL
                    WHERE team_id = $1 AND profile_revision = $2 AND claim_id = $4"#,
            )
            .bind(effect.team_id)
            .bind(effect.profile_revision)
            .bind(last_game_id)
            .bind(claim_id)
            .execute(state.pg())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        }
    }
    sqlx::query(
        r#"DELETE FROM "TeamProfileOperations" WHERE operation_id IN (
               SELECT operation_id FROM "TeamProfileOperations"
                WHERE created_at_utc < CURRENT_TIMESTAMP - INTERVAL '7 days'
                ORDER BY created_at_utc LIMIT 256
           )"#,
    )
    .execute(state.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(processed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_digest_ignores_operation_identity_but_keeps_revision() {
        let first = TeamUpdateModel {
            name: Some("team".into()),
            bio: Some("bio".into()),
            profile_revision: 4,
            operation_id: Some(Uuid::new_v4()),
        };
        let second = TeamUpdateModel {
            name: Some("team".into()),
            bio: Some("bio".into()),
            profile_revision: 4,
            operation_id: Some(Uuid::new_v4()),
        };
        let third = TeamUpdateModel {
            name: Some("team".into()),
            bio: Some("bio".into()),
            profile_revision: 5,
            operation_id: second.operation_id,
        };
        assert_eq!(
            request_digest(&first).unwrap(),
            request_digest(&second).unwrap()
        );
        assert_ne!(
            request_digest(&second).unwrap(),
            request_digest(&third).unwrap()
        );
    }
}
