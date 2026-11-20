//! A team can inspect the organizer-selected starter for its own hosted service.
use super::{
    resolve_participation, AppError, AppResult, ChallengeType, CurrentUser, Path, SharedState,
    State,
};
use crate::services::ad::patches::{PatchChange, PatchManifest, Starter};
use crate::services::{
    bulk_export,
    live_roster::{self, LiveParticipationIdentity},
};
use crate::utils::upload::read_patch_zip;
use axum::{
    extract::Request,
    http::header,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use sqlx::PgPool;
use std::sync::Arc;

pub(super) mod revisions;
pub(super) mod source;
#[cfg(test)]
mod tests;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Preview {
    starter_tree: String,
    candidate_tree: String,
    base_revision: i32,
    changes: Vec<PatchChange>,
}

async fn caller<'a>(
    st: &SharedState,
    user: &'a CurrentUser,
    game_id: i32,
) -> AppResult<LiveParticipationIdentity<'a>> {
    let part = resolve_participation(st, user, game_id).await?;
    Ok(LiveParticipationIdentity {
        user_id: user.id,
        expected_security_stamp: &user.security_stamp,
        game_id,
        team_id: part.team_id,
        participation_id: part.id,
    })
}

/// Hold the existing roster, game, division and challenge fences only while
/// deciding what may be returned. A slow upload never owns a pool connection.
async fn authorized_starter(
    pool: &PgPool,
    who: LiveParticipationIdentity<'_>,
    challenge: i32,
) -> AppResult<Option<Starter>> {
    let (roster, _, starter) = lock_source(pool, who, challenge).await?;
    roster.release().await?;
    Ok(starter)
}

async fn lock_source(
    pool: &PgPool,
    who: LiveParticipationIdentity<'_>,
    challenge: i32,
) -> AppResult<(
    crate::utils::single_flight::PgAdvisoryLock,
    i32,
    Option<Starter>,
)> {
    let mut roster = live_roster::try_acquire_participation_fence(
        pool,
        who.user_id,
        who.expected_security_stamp,
        who.game_id,
        who.team_id,
        who.participation_id,
        true,
    )
    .await?
    .ok_or(AppError::Forbidden)?;
    let db = &mut **roster.transaction_mut();
    super::super::play::lock_visible_challenge_on(
        db,
        who.game_id,
        who.team_id,
        who.participation_id,
        challenge,
    )
    .await?;
    let service = sqlx::query_scalar::<_, i32>(
        r#"SELECT s.id FROM "AdTeamServices" s JOIN "GameChallenges" c
             ON c.id = s.challenge_id AND c.game_id = s.game_id
            WHERE s.game_id = $1 AND s.participation_id = $2 AND c.id = $3
              AND c."Type" = $4 AND NOT c.ad_self_hosted
            FOR SHARE OF s"#,
    )
    .bind(who.game_id)
    .bind(who.participation_id)
    .bind(challenge)
    .bind(ChallengeType::AttackDefense as i16)
    .fetch_optional(&mut *db)
    .await
    .map_err(|e| AppError::internal(e.to_string()))?
    .ok_or_else(|| AppError::not_found("Hosted team service not found"))?;
    // Read the clock after taking the locks. Practice and archived access do
    // not extend the competition's source-sharing window.
    let (tree, size) = sqlx::query_as::<_, (Option<String>, Option<i32>)>(
        r#"SELECT starter.tree_id, starter.archive_bytes FROM "Games" g
             LEFT JOIN "AdPatchStarters" starter ON starter.challenge_id = $2
            WHERE g.id = $1 AND g.start_time_utc <= clock_timestamp() AND clock_timestamp() < g.end_time_utc"#,
    ).bind(who.game_id).bind(challenge).fetch_optional(&mut *db).await.map_err(|e| AppError::internal(e.to_string()))?
        .ok_or_else(|| AppError::not_found("Patch source is available during the game"))?;
    Ok((
        roster,
        service,
        tree.zip(size).map(|(tree_id, archive_bytes)| Starter {
            tree_id,
            archive_bytes,
        }),
    ))
}

fn missing() -> AppError {
    AppError::not_found("No starter ZIP is registered for this challenge")
}

async fn load(pool: &PgPool, challenge: i32, starter: &Starter) -> AppResult<Vec<u8>> {
    // No client-supplied hash and no fallback to the private build archive.
    sqlx::query_scalar(
        r#"SELECT archive FROM "AdPatchStarters"
        WHERE challenge_id = $1 AND tree_id = $2 AND archive_bytes = $3"#,
    )
    .bind(challenge)
    .bind(&starter.tree_id)
    .bind(starter.archive_bytes)
    .fetch_optional(pool)
    .await
    .map_err(|e| AppError::internal(e.to_string()))?
    .ok_or_else(missing)
}

async fn recheck(
    pool: &PgPool,
    who: LiveParticipationIdentity<'_>,
    challenge: i32,
    starter: &Starter,
) -> AppResult<()> {
    if authorized_starter(pool, who, challenge).await?.as_ref() != Some(starter) {
        return Err(AppError::conflict("Starter changed. Reload the challenge."));
    }
    Ok(())
}

pub async fn info(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game, challenge)): Path<(i32, i32)>,
) -> AppResult<Response> {
    let who = caller(&st, &user, game).await?;
    Ok((
        [(header::CACHE_CONTROL, "private, no-store")],
        Json(authorized_starter(st.pg(), who, challenge).await?),
    )
        .into_response())
}

pub async fn download(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game, challenge)): Path<(i32, i32)>,
) -> AppResult<Response> {
    let who = caller(&st, &user, game).await?;
    let starter = authorized_starter(st.pg(), who, challenge)
        .await?
        .ok_or_else(missing)?;
    let permit = match st
        .bulk_export_admission
        .try_acquire(Arc::clone(&st.cache), starter.archive_bytes as usize * 2)
        .await
    {
        Ok(permit) => Arc::new(permit),
        Err(_) => return Ok(bulk_export::overload_response()),
    };
    let bytes = load(st.pg(), challenge, &starter).await?;
    recheck(st.pg(), who, challenge, &starter).await?;
    Ok((
        [
            (header::CACHE_CONTROL, "private, no-store"),
            (header::CONTENT_TYPE, "application/zip"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"starter.zip\"",
            ),
        ],
        bulk_export::permitted_bytes_body(bytes, permit),
    )
        .into_response())
}

pub async fn preview(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game, challenge)): Path<(i32, i32)>,
    request: Request,
) -> AppResult<Response> {
    let who = caller(&st, &user, game).await?;
    let (roster, service, starter) = lock_source(st.pg(), who, challenge).await?;
    let starter = starter.ok_or_else(missing)?;
    roster.release().await?;
    let (bytes, permit) = read_patch_zip(request, starter.archive_bytes as usize).await?;
    let (candidate, permit) = tokio::task::spawn_blocking(move || {
        PatchManifest::from_zip(&bytes).map(|manifest| (manifest, permit))
    })
    .await
    .map_err(|e| AppError::internal(e.to_string()))?
    .map_err(AppError::bad_request)?;
    let original = load(st.pg(), challenge, &starter).await?;
    let (candidate_tree, changes) = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let baseline = PatchManifest::from_zip(&original)?;
        Ok::<_, String>((candidate.tree_id(), candidate.changes_from(&baseline)))
    })
    .await
    .map_err(|e| AppError::internal(e.to_string()))?
    .map_err(AppError::internal)?;
    let (mut roster, current_service, current_starter) =
        lock_source(st.pg(), who, challenge).await?;
    if service != current_service || current_starter.as_ref() != Some(&starter) {
        return Err(AppError::conflict(
            "Service or starter changed. Reopen the challenge.",
        ));
    }
    let base_revision = sqlx::query_scalar::<_, i32>(
        r#"SELECT revision FROM "AdPatchRevisions"
        WHERE service_id=$1 ORDER BY revision DESC LIMIT 1"#,
    )
    .bind(service)
    .fetch_optional(&mut **roster.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .unwrap_or(0);
    roster.release().await?;
    Ok((
        [(header::CACHE_CONTROL, "private, no-store")],
        Json(Preview {
            starter_tree: starter.tree_id,
            candidate_tree,
            base_revision,
            changes,
        }),
    )
        .into_response())
}
