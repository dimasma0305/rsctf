//! Database publication and teardown for stable BYOC relay endpoints.

use std::sync::Arc;

use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

use super::flag::FlagRetention;
use super::{live_tunnel_authorized, AuthorizationGeneration, RelayEndpoint, TunnelHandle};
use crate::app_state::SharedState;
use crate::models::data::ad_team_service;
use crate::utils::error::{AppError, AppResult};

const LATEST_DURABLE_FLAG_SQL: &str = r#"
    SELECT flag.round_id, flag.flag
      FROM "AdTeamServices" service
      JOIN "AdFlags" flag ON flag.team_service_id = service.id
      JOIN "AdRounds" round
        ON round.id = flag.round_id AND round.game_id = service.game_id
     WHERE service.game_id = $1
       AND service.participation_id = $2
       AND service.challenge_id = $3
     ORDER BY round.number DESC, round.id DESC
     LIMIT 1
"#;

async fn load_latest_durable_flag(
    pool: &sqlx::PgPool,
    game_id: i32,
    pid: i32,
    cid: i32,
) -> AppResult<Option<(u64, String)>> {
    let row: Option<(i32, String)> = sqlx::query_as(LATEST_DURABLE_FLAG_SQL)
        .bind(game_id)
        .bind(pid)
        .bind(cid)
        .fetch_optional(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    row.map(|(round_id, flag)| {
        u64::try_from(round_id)
            .ok()
            .filter(|sequence| *sequence > 0)
            .map(|sequence| (sequence, flag))
            .ok_or_else(|| AppError::internal("durable BYOC flag has an invalid round identity"))
    })
    .transpose()
}

async fn hydrate_durable_flag(
    st: &SharedState,
    game_id: i32,
    pid: i32,
    cid: i32,
    endpoint: &RelayEndpoint,
) -> AppResult<()> {
    let durable = load_latest_durable_flag(st.pg(), game_id, pid, cid).await?;
    retain_loaded_durable_flag(endpoint, durable).await
}

async fn retain_loaded_durable_flag(
    endpoint: &RelayEndpoint,
    durable: Option<(u64, String)>,
) -> AppResult<()> {
    let Some((sequence, flag)) = durable else {
        return Ok(());
    };
    match endpoint.retain_flag(sequence, &flag).await {
        Ok(FlagRetention::Accepted(_) | FlagRetention::Stale(_)) => Ok(()),
        Err(error) => Err(AppError::internal(format!(
            "durable BYOC flag could not be retained safely: {error:?}"
        ))),
    }
}

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
    let distributed = crate::services::ad::service_lifecycle::acquire_publication_lock(
        st.pg(),
        game_id,
        pid,
        cid,
    )
    .await?;
    // Revalidate after taking the same lock used by credential rotation and
    // service teardown. This closes the authorize-then-publish race for pending
    // WebSockets during participation rejection, team deletion, or token rotation.
    if !live_tunnel_authorized(st, game_id, pid, cid, token).await {
        distributed.release().await?;
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
            return Err(AppError::Forbidden);
        }
    };
    if !live_tunnel_authorized(st, game_id, pid, cid, token).await {
        distributed.release().await?;
        return Err(AppError::Forbidden);
    }
    if !st.byoc.contains_endpoint(pid, cid, endpoint.id()).await {
        distributed.release().await?;
        return Err(AppError::Forbidden);
    }
    // Hydrate before attachment. A fresh process must not expose service or exec
    // forwarding until the agent has acknowledged the latest durable round flag.
    // Monotonic endpoint retention fences a concurrent newer live push.
    hydrate_durable_flag(st, game_id, pid, cid, &endpoint).await?;
    register_service(st, pid, cid, endpoint.host(), endpoint.port()).await?;
    if endpoint.attach(handle.clone()).await.is_err() {
        let cleanup = offline_service(st, pid, cid, endpoint.host(), endpoint.port()).await;
        if let Err(error) = distributed.release().await {
            tracing::warn!(pid, cid, %error, "byoc: relay lock release failed after rejected attach");
        }
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
        // A replacement may still be waiting for its retained-flag ACK and is
        // intentionally unavailable to service forwarding. Its raw attachment
        // nevertheless owns publication cleanup, so the old generation must
        // not blank the shared endpoint row underneath it.
        if endpoint.raw_current().await.is_some() {
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sqlx::postgres::PgPoolOptions;

    use super::*;

    #[test]
    fn hydration_query_is_exact_and_independent_of_delivery_receipts() {
        for predicate in [
            "service.game_id = $1",
            "service.participation_id = $2",
            "service.challenge_id = $3",
            "round.game_id = service.game_id",
        ] {
            assert!(LATEST_DURABLE_FLAG_SQL.contains(predicate));
        }
        assert!(!LATEST_DURABLE_FLAG_SQL.contains("AdFlagDeliveryResults"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn restart_hydration_loads_only_the_latest_exact_durable_flag() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TEMP TABLE "AdTeamServices" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              participation_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL
            );
            CREATE TEMP TABLE "AdRounds" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, number INTEGER NOT NULL
            );
            CREATE TEMP TABLE "AdFlags" (
              round_id INTEGER NOT NULL, team_service_id INTEGER NOT NULL, flag TEXT NOT NULL
            );
            INSERT INTO "AdTeamServices" VALUES
              (1, 7, 11, 21), (2, 7, 12, 21), (3, 8, 11, 21);
            INSERT INTO "AdRounds" VALUES
              (70, 7, 4), (91, 7, 5), (92, 8, 99);
            INSERT INTO "AdFlags" VALUES
              (70, 1, 'old-exact'), (91, 1, 'current-exact'),
              (91, 2, 'wrong-participation'), (92, 3, 'wrong-game');
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let durable = load_latest_durable_flag(&pool, 7, 11, 21).await.unwrap();
        assert_eq!(durable, Some((91, "current-exact".to_string())));
        assert_eq!(
            load_latest_durable_flag(&pool, 7, 11, 22).await.unwrap(),
            None
        );

        let endpoint = RelayEndpoint::bind("127.0.0.1".to_string()).await.unwrap();
        retain_loaded_durable_flag(&endpoint, durable)
            .await
            .unwrap();
        let (open, _requests) = tokio::sync::mpsc::channel(1);
        let (_closed_tx, closed) = tokio::sync::watch::channel(false);
        let handle = TunnelHandle {
            id: 1,
            open,
            shutdown: Arc::new(tokio::sync::Notify::new()),
            closed,
            active: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        };
        assert!(endpoint.attach(handle).await.is_ok());
        assert!(endpoint.current().await.is_none());
        let retained = endpoint.retained_flag().await.unwrap();
        assert_eq!(retained.sequence(), 91);
        assert!(endpoint.mark_flag_ready(1, &retained).await);
        assert_eq!(endpoint.current().await.unwrap().id, 1);
        assert!(endpoint.revoke().await.is_some());
        endpoint.wait_closed().await;
    }
}
