use std::net::Ipv4Addr;

use uuid::Uuid;

use crate::app_state::SharedState;
#[cfg(test)]
use crate::middlewares::privilege_authentication::CurrentUser;
use crate::utils::enums::Role;
use crate::utils::error::{AppError, AppResult};

use super::{load_authorized_target_on, AssetFinalGrant, AuthorizedAsset, DownloadEventTarget};

fn download_event_lock_key(target: &DownloadEventTarget) -> String {
    format!(
        "asset-download-event:{}:{}:{}",
        target.game_id, target.team_id, target.challenge_id
    )
}

async fn insert_download_event_on(
    connection: &mut sqlx::PgConnection,
    target: &DownloadEventTarget,
    token: Option<&str>,
) -> AppResult<Option<i32>> {
    let challenge_id = target.challenge_id.to_string();
    let event_id = sqlx::query_scalar(
        r#"INSERT INTO "GameEvents"
               (game_id, "Type", "values", publish_time_utc, user_id, team_id)
           SELECT $1, $2, jsonb_build_array($3::text, $4::text, $5::text),
                  $6, $7, $8
            WHERE NOT EXISTS (
                  SELECT 1
                    FROM "GameEvents" existing
                   WHERE existing.game_id = $1
                     AND existing.team_id = $8
                     AND existing."Type" = $2
                     AND existing."values" ->> 0 = $3
            )
           RETURNING id"#,
    )
    .bind(target.game_id)
    .bind(crate::utils::enums::EventType::Download as i16)
    .bind(challenge_id)
    .bind(&target.challenge_title)
    .bind(token.unwrap_or(""))
    .bind(target.observed_at_utc)
    .bind(target.user_id)
    .bind(target.team_id)
    .fetch_optional(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(event_id)
}

pub(super) const PUBLIC_ASSET_FINAL_SQL: &str = r#"
WITH public_user AS MATERIALIZED (
    SELECT id FROM "AspNetUsers" WHERE avatar_hash = $1 FOR SHARE
), public_team AS MATERIALIZED (
    SELECT id FROM "Teams" WHERE avatar_hash = $1 FOR SHARE
), public_game AS MATERIALIZED (
    SELECT id FROM "Games" WHERE poster_hash = $1 FOR SHARE
), public_config AS MATERIALIZED (
    SELECT config_key
      FROM "Configs"
     WHERE config_key IN ('GlobalConfig:LogoHash', 'GlobalConfig:FaviconHash')
       AND value = $1
     FOR SHARE
)
SELECT EXISTS (SELECT 1 FROM public_user)
    OR EXISTS (SELECT 1 FROM public_team)
    OR EXISTS (SELECT 1 FROM public_game)
    OR EXISTS (SELECT 1 FROM public_config)
"#;

async fn finalize_public_asset(pool: &sqlx::PgPool, content_hash: &str) -> AppResult<()> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let still_public = sqlx::query_scalar::<_, bool>(PUBLIC_ASSET_FINAL_SQL)
        .bind(content_hash)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if !still_public {
        transaction
            .rollback()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Err(AppError::Forbidden);
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

async fn finalize_monitor_asset(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    expected_security_stamp: &str,
) -> AppResult<()> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let still_monitor = sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS (
               SELECT 1
                 FROM "AspNetUsers" account
                WHERE account.id = $1
                  AND account.security_stamp = $2
                  AND account.role IN ($3, $4)
                FOR SHARE OF account
           )"#,
    )
    .bind(user_id)
    .bind(expected_security_stamp)
    .bind(Role::Monitor as i16)
    .bind(Role::Admin as i16)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !still_monitor {
        transaction
            .rollback()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Err(AppError::Forbidden);
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

#[cfg(test)]
pub(in crate::controllers::assets) async fn finalize_public_grant_for_test(
    pool: &sqlx::PgPool,
    content_hash: &str,
) -> AppResult<()> {
    finalize_public_asset(pool, content_hash).await
}

#[cfg(test)]
pub(in crate::controllers::assets) async fn finalize_monitor_grant_for_test(
    pool: &sqlx::PgPool,
    user: &CurrentUser,
) -> AppResult<()> {
    finalize_monitor_asset(pool, user.id, &user.security_stamp).await
}

/// Finalize a prepared protected response. Storage work deliberately happens
/// before this call. It reacquires the exact stamped roster fence, rechecks the
/// mutable challenge/division policy, and commits the precisely attributed
/// Download event before releasing the fence. The response body is streamed
/// only after this transaction has committed, so no database guard is retained
/// for the network lifetime.
#[derive(Default)]
pub(super) struct FinalizedAssetDownload {
    vpn_gate_active: bool,
    event_id: Option<i32>,
}

pub(super) async fn finalize_asset_download_on(
    pool: &sqlx::PgPool,
    authorization: &AuthorizedAsset,
    source: Option<Ipv4Addr>,
    token: Option<&str>,
    record_download: bool,
) -> AppResult<FinalizedAssetDownload> {
    let grant = match &authorization.final_grant {
        AssetFinalGrant::None => return Ok(FinalizedAssetDownload::default()),
        AssetFinalGrant::Public { content_hash } => {
            finalize_public_asset(pool, content_hash).await?;
            return Ok(FinalizedAssetDownload::default());
        }
        AssetFinalGrant::Monitor {
            user_id,
            expected_security_stamp,
        } => {
            finalize_monitor_asset(pool, *user_id, expected_security_stamp).await?;
            return Ok(FinalizedAssetDownload::default());
        }
        AssetFinalGrant::Protected(grant) => grant,
    };
    let Some(mut roster) = crate::services::live_roster::try_acquire_participation_fence(
        pool,
        grant.user_id,
        &grant.expected_security_stamp,
        grant.game_id,
        grant.team_id,
        grant.participation_id,
        true,
    )
    .await?
    else {
        return Err(AppError::Forbidden);
    };

    let vpn_gate_active = crate::services::event_security::require_event_vpn_source_on(
        roster.transaction_mut().as_mut(),
        grant.game_id,
        grant.user_id,
        grant.participation_id,
        source,
    )
    .await?;

    let row = load_authorized_target_on(
        roster.transaction_mut(),
        grant.user_id,
        grant.game_id,
        grant.participation_id,
        grant.team_id,
        &grant.target,
        Some(&grant.content_hash),
    )
    .await?;
    let Some(row) = row else {
        roster
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Err(AppError::Forbidden);
    };

    let mut event_id = None;
    if record_download {
        if let Some((challenge_id, challenge_title)) =
            grant.target.challenge_id.zip(row.challenge_title)
        {
            let event = DownloadEventTarget {
                game_id: grant.game_id,
                team_id: grant.team_id,
                challenge_id,
                challenge_title,
                user_id: grant.user_id,
                observed_at_utc: row.observed_at_utc,
            };
            roster
                .acquire_additional(&download_event_lock_key(&event))
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            event_id = insert_download_event_on(roster.transaction_mut(), &event, token).await?;
        }
    }

    roster
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(FinalizedAssetDownload {
        vpn_gate_active,
        event_id,
    })
}

pub(in crate::controllers::assets) async fn finalize_asset_download(
    st: &SharedState,
    authorization: &AuthorizedAsset,
    source: Option<Ipv4Addr>,
    token: Option<&str>,
    record_download: bool,
) -> AppResult<bool> {
    let outcome =
        finalize_asset_download_on(st.pg(), authorization, source, token, record_download).await?;
    if let Some(event_id) = outcome.event_id {
        if let Err(error) =
            crate::services::game_event_feed::publish_committed(st, &[event_id]).await
        {
            tracing::warn!(event_id, %error, "asset download event publish failed");
        }
    }
    Ok(outcome.vpn_gate_active)
}
