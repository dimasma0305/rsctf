//! Register the source teams should start from, independently of the private build archive.
use super::{
    manager_or_admin, AppError, AppResult, ChallengeType, CurrentUser, Path, SharedState, State,
};
use crate::services::ad::patches::{PatchManifest, Starter};
use crate::utils::upload::read_patch_zip;
use axum::extract::Request;
use axum::http::HeaderMap;
use axum::Json;
use sqlx::PgPool;

#[cfg(test)]
mod tests;

fn db_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

pub async fn get(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game_id, challenge_id)): Path<(i32, i32)>,
) -> AppResult<Json<Option<Starter>>> {
    manager_or_admin(&st, &user, game_id).await?;
    let (tree, size) = sqlx::query_as::<_, (Option<String>, Option<i32>)>(
        r#"SELECT starter.tree_id, starter.archive_bytes
             FROM "GameChallenges" c JOIN "Games" g ON g.id = c.game_id
             LEFT JOIN "AdPatchStarters" starter ON starter.challenge_id = c.id
            WHERE c.id = $1 AND c.game_id = $2 AND c."Type" = $3
              AND NOT c.ad_self_hosted AND NOT c.deletion_pending AND NOT g.deletion_pending"#,
    )
    .bind(challenge_id)
    .bind(game_id)
    .bind(ChallengeType::AttackDefense as i16)
    .fetch_optional(st.pg())
    .await
    .map_err(db_error)?
    .ok_or_else(|| AppError::not_found("Hosted A&D challenge not found"))?;
    Ok(Json(tree.zip(size).map(|(tree_id, archive_bytes)| {
        Starter {
            tree_id,
            archive_bytes,
        }
    })))
}

fn expected_tree(headers: &HeaderMap) -> AppResult<Option<String>> {
    let Some(header) = headers.get("x-rsctf-expected-tree") else {
        return Ok(None);
    };
    let value = header
        .to_str()
        .ok()
        .filter(|s| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        .ok_or_else(|| {
            AppError::bad_request("Expected tree must be a lowercase SHA-256 tree ID")
        })?;
    Ok(Some(value.to_owned()))
}

pub async fn put(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game_id, challenge_id)): Path<(i32, i32)>,
    request: Request,
) -> AppResult<Json<Starter>> {
    manager_or_admin(&st, &user, game_id).await?;
    let expected = expected_tree(request.headers())?;
    let (archive, permit) = read_patch_zip(request, 0).await?;
    let (archive, tree_id, _permit) = tokio::task::spawn_blocking(move || {
        let tree = PatchManifest::from_zip(&archive)?.tree_id();
        Ok::<_, String>((archive, tree, permit))
    })
    .await
    .map_err(|e| AppError::internal(e.to_string()))?
    .map_err(AppError::bad_request)?;
    Ok(Json(
        register(
            st.pg(),
            &user,
            game_id,
            challenge_id,
            &archive,
            &tree_id,
            expected.as_deref(),
        )
        .await?,
    ))
}

async fn register(
    pool: &PgPool,
    user: &CurrentUser,
    game_id: i32,
    challenge_id: i32,
    archive: &[u8],
    tree_id: &str,
    expected: Option<&str>,
) -> AppResult<Starter> {
    let mut tx = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(db_error)?;
    if !user.is_admin() {
        let manager = sqlx::query_scalar::<_, i32>(
            r#"SELECT game_id FROM "GameManagers" WHERE game_id=$1 AND user_id=$2 FOR SHARE"#,
        )
        .bind(game_id)
        .bind(user.id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?;
        if manager.is_none() {
            return Err(AppError::Forbidden);
        }
    }
    // Serialize competing starters, challenge edits, and deletion. Never hold
    // these locks over upload, decompression, or hashing.
    sqlx::query_scalar::<_, i32>(
        r#"SELECT c.id
             FROM "Games" g JOIN "GameChallenges" c ON c.game_id = g.id
            WHERE g.id = $1 AND c.id = $2 AND c."Type" = $3
              AND NOT c.ad_self_hosted AND NOT c.deletion_pending AND NOT g.deletion_pending
              FOR SHARE OF g FOR UPDATE OF c"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(ChallengeType::AttackDefense as i16)
    .fetch_optional(&mut *tx)
    .await
    .map_err(db_error)?
    .ok_or_else(|| AppError::not_found("Hosted A&D challenge not found"))?;
    let current = sqlx::query_as::<_, Starter>(
        r#"SELECT tree_id, archive_bytes FROM "AdPatchStarters" WHERE challenge_id=$1"#,
    )
    .bind(challenge_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(db_error)?;
    if let Some(current) = current {
        // A lost-response retry (including a repacked archive) retains the original bytes.
        if current.tree_id == tree_id {
            return Ok(current);
        }
        if expected != Some(&current.tree_id) {
            return Err(AppError::conflict(
                "Starter changed. Reload its tree ID before replacing it.",
            ));
        }
    } else if expected.is_some() {
        return Err(AppError::conflict("There is no saved starter to replace"));
    }
    let starter = sqlx::query_as::<_, Starter>(
        r#"INSERT INTO "AdPatchStarters" (challenge_id, tree_id, archive) VALUES ($1,$2,$3)
           ON CONFLICT (challenge_id) DO UPDATE SET tree_id=EXCLUDED.tree_id, archive=EXCLUDED.archive
           WHERE EXISTS (SELECT 1 FROM "Games" WHERE id=$4 AND start_time_utc > clock_timestamp())
           RETURNING tree_id, archive_bytes"#,
    )
    .bind(challenge_id)
    .bind(tree_id)
    .bind(archive)
    .bind(game_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(db_error)?
    .ok_or_else(|| AppError::conflict("The original starter is pinned after the game starts"))?;
    tx.commit().await.map_err(db_error)?;
    Ok(starter)
}
