//! Database publication and teardown for stable BYOC relay endpoints.

use std::sync::Arc;

use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

use super::{live_tunnel_authorized, AuthorizationGeneration, RelayEndpoint, TunnelHandle};
use crate::app_state::SharedState;
use crate::models::data::ad_team_service;
use crate::utils::error::{AppError, AppResult};

/// Point the BYOC service row at the live tunnel listener (host:port) so the
/// checker probes it. Upserts if the row is missing.
async fn register_service(
    st: &SharedState,
    pid: i32,
    cid: i32,
    host: &str,
    port: i32,
) -> AppResult<()> {
    let game_id = match ad_team_service::Entity::find()
        .filter(ad_team_service::Column::ParticipationId.eq(pid))
        .filter(ad_team_service::Column::ChallengeId.eq(cid))
        .one(&st.db)
        .await
    {
        Ok(Some(row)) => {
            let mut model: ad_team_service::ActiveModel = row.into();
            model.host = Set(host.to_string());
            model.port = Set(port);
            model.container_id = Set(None);
            model.update(&st.db).await?;
            return Ok(());
        }
        Ok(None) => participation_game(st, pid).await,
        Err(error) => return Err(error.into()),
    };
    let game_id = game_id.ok_or_else(|| AppError::not_found("Participation not found"))?;
    ad_team_service::ActiveModel {
        game_id: Set(game_id),
        participation_id: Set(pid),
        challenge_id: Set(cid),
        host: Set(host.to_string()),
        port: Set(port),
        status: Set(crate::utils::enums::AdCheckStatus::Offline as i16),
        container_id: Set(None),
        last_reset_at: Set(None),
        ..Default::default()
    }
    .insert(&st.db)
    .await?;
    Ok(())
}

/// On tunnel loss, blank the endpoint + mark Offline so the checker stops probing.
async fn offline_service(
    st: &SharedState,
    pid: i32,
    cid: i32,
    expected_host: &str,
    expected_port: i32,
) -> AppResult<u64> {
    let result = sqlx::query(
        r#"UPDATE "AdTeamServices"
              SET host = '', port = 0, status = 2
            WHERE participation_id = $1
              AND challenge_id = $2
              AND container_id IS NULL
              AND host = $3
              AND port = $4"#,
    )
    .bind(pid)
    .bind(cid)
    .bind(expected_host)
    .bind(expected_port)
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(result.rows_affected())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn activate_tunnel(
    st: &SharedState,
    game_id: i32,
    pid: i32,
    cid: i32,
    token: &str,
    authorization_generation: AuthorizationGeneration,
    endpoint: Arc<RelayEndpoint>,
    handle: TunnelHandle,
) -> AppResult<()> {
    let lock_key = format!("ad-service:{pid}:{cid}");
    let local = crate::utils::single_flight::coalesce(&lock_key).await;
    let distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(st.pg(), &lock_key)
            .await?;
    // Revalidate after taking the same lock used by credential rotation and
    // service teardown. This closes the authorize-then-publish race for pending
    // WebSockets during participation rejection, team deletion, or token rotation.
    if !live_tunnel_authorized(st, game_id, pid, cid, token).await {
        distributed.release().await?;
        drop(local);
        return Err(AppError::Forbidden);
    }
    // Hold this shared gate through publication. Disconnect takes the write
    // side before it advances a generation and returns, which orders an
    // in-flight sync strictly before that revocation.
    let publication_guard = match st
        .byoc
        .publication_guard(pid, cid, authorization_generation)
        .await
    {
        Some(guard) => guard,
        None => {
            distributed.release().await?;
            drop(local);
            return Err(AppError::Forbidden);
        }
    };
    if !live_tunnel_authorized(st, game_id, pid, cid, token).await {
        distributed.release().await?;
        drop(local);
        return Err(AppError::Forbidden);
    }
    if !st.byoc.contains_endpoint(pid, cid, endpoint.id()).await {
        distributed.release().await?;
        drop(local);
        return Err(AppError::Forbidden);
    }
    register_service(st, pid, cid, endpoint.host(), endpoint.port()).await?;
    if endpoint.attach(handle.clone()).await.is_err() {
        let cleanup = offline_service(st, pid, cid, endpoint.host(), endpoint.port()).await;
        if let Err(error) = distributed.release().await {
            tracing::warn!(pid, cid, %error, "byoc: relay lock release failed after rejected attach");
        }
        drop(local);
        drop(publication_guard);
        cleanup?;
        crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await?;
        return Err(AppError::Forbidden);
    }
    // The session attach precedes the final authorization read deliberately:
    // a concurrent token rotation either becomes visible here or sees this
    // handle in its disconnect scan. There is no authorize/publish gap.
    if !live_tunnel_authorized(st, game_id, pid, cid, token).await {
        if let Err(error) = distributed.release().await {
            tracing::warn!(pid, cid, %error, "byoc: relay lock release failed after revocation");
        }
        drop(local);
        drop(publication_guard);
        if let Some(epoch) = deactivate_tunnel(st, pid, cid, handle.id, &endpoint).await {
            st.byoc
                .retire_idle_endpoint(pid, cid, endpoint, epoch)
                .await;
        }
        return Err(AppError::Forbidden);
    }
    if let Err(error) = distributed.release().await {
        tracing::warn!(pid, cid, %error, "byoc: relay lock release failed after publication");
    }
    drop(local);
    if let Err(error) = crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await {
        drop(publication_guard);
        if let Some(epoch) = deactivate_tunnel(st, pid, cid, handle.id, &endpoint).await {
            st.byoc
                .schedule_failed_activation_release(pid, cid, endpoint, epoch);
        }
        return Err(error);
    }
    // Credential state can change while firewall reconciliation is running but
    // before the mutation handler reaches `disconnect_*`. Do not publish that
    // now-invalid session: remove it and reconcile before activation returns.
    if !live_tunnel_authorized(st, game_id, pid, cid, token).await {
        drop(publication_guard);
        if let Some(epoch) = deactivate_tunnel(st, pid, cid, handle.id, &endpoint).await {
            st.byoc
                .retire_idle_endpoint(pid, cid, endpoint, epoch)
                .await;
        }
        return Err(AppError::Forbidden);
    }
    drop(publication_guard);
    Ok(())
}

/// Detach and unpublish only if `id` still owns the endpoint. The idle epoch
/// also fences retries after a same-address replacement attaches.
pub(super) async fn deactivate_tunnel(
    st: &SharedState,
    pid: i32,
    cid: i32,
    id: u64,
    endpoint: &Arc<RelayEndpoint>,
) -> Option<u64> {
    let lock_key = format!("ad-service:{pid}:{cid}");
    let mut idle_epoch = None;
    loop {
        let local = crate::utils::single_flight::coalesce(&lock_key).await;
        let distributed = match crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(
            st.pg(),
            &lock_key,
        )
        .await
        {
            Ok(lock) => lock,
            Err(error) => {
                drop(local);
                tracing::warn!(pid, cid, %error, "byoc: relay cleanup lock unavailable; retrying");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }
        };
        // Explicit revocation owns database cleanup after removing this exact
        // endpoint. Stop an older retry before a future listener can reuse the
        // same OS port and be mistaken for the stale session (ABA).
        if !st.byoc.contains_endpoint(pid, cid, endpoint.id()).await {
            let _ = distributed.release().await;
            drop(local);
            return None;
        }
        if idle_epoch.is_none() {
            idle_epoch = endpoint.detach_if(id).await;
            if idle_epoch.is_none() {
                let _ = distributed.release().await;
                drop(local);
                return None;
            }
        }
        let epoch = idle_epoch?;
        // A pending reconnect claim must fence only the idle reaper, not this
        // already-owned DB cleanup. Stop retrying only after a replacement
        // session is actually attached; that session republishes under this
        // same service lock.
        if endpoint.current().await.is_some() {
            let _ = distributed.release().await;
            drop(local);
            return None;
        }
        if let Err(error) = offline_service(st, pid, cid, endpoint.host(), endpoint.port()).await {
            let _ = distributed.release().await;
            drop(local);
            tracing::warn!(pid, cid, %error, "byoc: relay endpoint cleanup failed; retrying");
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            continue;
        }
        if let Err(error) = distributed.release().await {
            tracing::warn!(pid, cid, %error, "byoc: could not release relay cleanup lock");
        }
        drop(local);
        loop {
            match crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await {
                Ok(()) => break,
                Err(error) => {
                    tracing::warn!(pid, cid, %error, "byoc: VPN relay revocation failed; retrying");
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        }
        return Some(epoch);
    }
}

async fn participation_game(st: &SharedState, pid: i32) -> Option<i32> {
    use crate::models::data::participation;
    participation::Entity::find_by_id(pid)
        .one(&st.db)
        .await
        .ok()
        .flatten()
        .map(|participation| participation.game_id)
}
