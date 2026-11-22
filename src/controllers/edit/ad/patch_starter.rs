//! Register the source teams should start from, independently of the private build archive.
use super::{
    manager_or_admin, AppError, AppResult, ChallengeType, CurrentUser, Path, SharedState, State,
};
use crate::middlewares::rate_limiter::{limited, Policy};
use crate::services::ad::patches::ssh_sources::SshSource;
use crate::services::ad::patches::{PatchManifest, Starter};
use crate::utils::upload::{read_patch_zip, SOURCE_ARCHIVE_BLOB_BYTES};
use axum::extract::{DefaultBodyLimit, Request};
use axum::http::HeaderMap;
use axum::{routing, Json, Router};
use sqlx::PgPool;

#[cfg(test)]
mod tests;

#[derive(Debug, serde::Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RegisteredStarter {
    #[serde(flatten)]
    #[sqlx(flatten)]
    source: Starter,
    runtime_image_ref: Option<String>,
    awaiting_image_build: bool,
    #[sqlx(json)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    ssh_sources: Vec<SshSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data_mount: Option<String>,
}

struct Registration {
    tree_id: String,
    ssh_sources: Vec<SshSource>,
    data_mount: Option<String>,
}

impl Registration {
    fn from_zip(archive: &[u8]) -> Result<Self, String> {
        let manifest = PatchManifest::from_zip(archive)?;
        Ok(Self {
            tree_id: manifest.tree_id(),
            ssh_sources: manifest.ssh_sources(archive)?,
            data_mount: Self::data_mount(archive)?,
        })
    }

    fn data_mount(archive: &[u8]) -> Result<Option<String>, String> {
        use std::io::{Cursor, Read};
        let mut zip = zip::ZipArchive::new(Cursor::new(archive)).map_err(|e| e.to_string())?;
        let file = match zip.by_name(".rsctf-data-dir") {
            Ok(file) => file,
            Err(zip::result::ZipError::FileNotFound) => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
        let mut text = String::new();
        file.take(257)
            .read_to_string(&mut text)
            .map_err(|_| "Invalid .rsctf-data-dir")?;
        let path = text
            .strip_suffix("\r\n")
            .or_else(|| text.strip_suffix('\n'))
            .unwrap_or(&text);
        if !crate::services::container::application_data_path(path) {
            return Err(
                ".rsctf-data-dir must contain /data or one directory under /var/lib".into(),
            );
        }
        Ok(Some(path.to_owned()))
    }
}

pub(crate) fn router() -> Router<SharedState> {
    Router::new().route(
        "/api/edit/games/{id}/challenges/{cId}/patch-starter",
        routing::get(get).merge(limited(
            Policy::Query,
            routing::put(put).layer(DefaultBodyLimit::max(SOURCE_ARCHIVE_BLOB_BYTES)),
        )),
    )
}

fn db_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

pub async fn get(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game_id, challenge_id)): Path<(i32, i32)>,
) -> AppResult<Json<Option<RegisteredStarter>>> {
    manager_or_admin(&st, &user, game_id).await?;
    let (tree, size, runtime_image_ref, awaiting_image_build, ssh_sources, data_mount) =
        sqlx::query_as::<_, (Option<String>, Option<i32>, Option<String>, bool, sqlx::types::Json<Vec<SshSource>>, Option<String>)>(
            r#"SELECT starter.tree_id, starter.archive_bytes, starter.runtime_image_ref,
                  COALESCE(starter.awaiting_image_build,FALSE), COALESCE(starter.ssh_sources,'[]'::jsonb), starter.data_mount
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
        RegisteredStarter {
            source: Starter {
                tree_id,
                archive_bytes,
            },
            runtime_image_ref,
            awaiting_image_build,
            ssh_sources: ssh_sources.0,
            data_mount,
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
) -> AppResult<Json<RegisteredStarter>> {
    manager_or_admin(&st, &user, game_id).await?;
    let expected = expected_tree(request.headers())?;
    let (archive, permit) = read_patch_zip(request, 0).await?;
    let (archive, registration, _permit) = tokio::task::spawn_blocking(move || {
        let registration = Registration::from_zip(&archive)?;
        Ok::<_, String>((archive, registration, permit))
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
            &registration,
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
    registration: &Registration,
    expected: Option<&str>,
) -> AppResult<RegisteredStarter> {
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
    let current = sqlx::query_as::<_, RegisteredStarter>(
        r#"SELECT tree_id, archive_bytes, runtime_image_ref, awaiting_image_build, ssh_sources, data_mount FROM "AdPatchStarters" WHERE challenge_id=$1"#,
    )
    .bind(challenge_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(db_error)?;
    if let Some(current) = current {
        // A lost-response retry (including a repacked archive) retains the original bytes.
        if current.source.tree_id == registration.tree_id
            && current.ssh_sources == registration.ssh_sources
            && current.data_mount == registration.data_mount
        {
            return Ok(current);
        }
        if expected != Some(&current.source.tree_id) {
            return Err(AppError::conflict(
                "Starter changed. Reload its tree ID before replacing it.",
            ));
        }
    } else if expected.is_some() {
        return Err(AppError::conflict("There is no saved starter to replace"));
    }
    let starter = sqlx::query_as::<_, RegisteredStarter>(
        r#"INSERT INTO "AdPatchStarters" (challenge_id,tree_id,archive,ssh_sources,data_mount)
           SELECT $1,$2,$3,$5::jsonb,$6::text WHERE (($5::jsonb='[]'::jsonb AND $6 IS NULL)
             OR EXISTS (SELECT 1 FROM "Games" WHERE id=$4 AND start_time_utc > clock_timestamp()))
           AND ($6 IS NULL OR EXISTS(SELECT 1 FROM "AdPatchStarters" WHERE challenge_id=$1 AND data_mount=$6)
                OR NOT EXISTS(SELECT 1 FROM "AdTeamServices" WHERE challenge_id=$1))
           ON CONFLICT (challenge_id) DO UPDATE SET tree_id=EXCLUDED.tree_id, archive=EXCLUDED.archive,
               ssh_sources=EXCLUDED.ssh_sources, data_mount=EXCLUDED.data_mount
           WHERE EXISTS (SELECT 1 FROM "Games" WHERE id=$4 AND start_time_utc > clock_timestamp())
             AND ("AdPatchStarters".data_mount IS NOT DISTINCT FROM EXCLUDED.data_mount
                  OR (NOT "AdPatchStarters".data_mount_claimed AND NOT EXISTS(SELECT 1 FROM "AdTeamServices" WHERE challenge_id=$1)))
             AND NOT EXISTS (SELECT 1 FROM "AdTeamServices" s JOIN "AdPatchSourceUsage" u ON u.service_id=s.id
                             WHERE s.game_id=$4 AND s.challenge_id=$1)
           RETURNING tree_id, archive_bytes, runtime_image_ref, awaiting_image_build, ssh_sources, data_mount"#,
    )
    .bind(challenge_id)
    .bind(&registration.tree_id)
    .bind(archive)
    .bind(game_id)
    .bind(sqlx::types::Json(&registration.ssh_sources))
    .bind(registration.data_mount.as_deref())
    .fetch_optional(&mut *tx)
    .await
    .map_err(db_error)?
    .ok_or_else(|| AppError::conflict("The starter is pinned after kickoff or source use. A provisioned team's data directory is also pinned."))?;
    tx.commit().await.map_err(db_error)?;
    Ok(starter)
}
