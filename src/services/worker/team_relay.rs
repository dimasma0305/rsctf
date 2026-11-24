//! Private TCP sockets on the VPN owner. One socket follows an assignment
//! across image revisions. The workload's public proxy never handles these.
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    time::Duration,
};

use rsctf_worker_protocol::{DataStreamRequest, EndpointRef, TcpProxyRequest, WorkloadFence};
use sqlx::{types::Json, PgPool};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::watch,
    task::{JoinHandle, JoinSet},
};
use uuid::Uuid;

use super::{WorkerHandle, WorkerService};
use crate::{
    app_state::SharedState,
    services::{ad::maintenance::database_error, ad_vpn},
    utils::error::{AppError, AppResult},
};

const REFRESH: Duration = Duration::from_secs(3);

pub(super) async fn address(
    pool: &PgPool,
    handle: WorkerHandle,
    participant: i32,
    challenge: i32,
) -> AppResult<(String, i32)> {
    let rows = sqlx::query(r#"INSERT INTO "AdWorkerRelays" (workload_id,participation_id,challenge_id)
        SELECT id,$4,$5 FROM "WorkerWorkloads" WHERE id=$1 AND assignment_id=$2 AND generation=$3
        AND desired_state='Present' AND owner_kind='ad-team-service'
        AND owner_key=concat('ad-service:',$4::integer,':',$5::integer)
        ON CONFLICT (workload_id) DO UPDATE SET participation_id=EXCLUDED.participation_id
        WHERE "AdWorkerRelays".participation_id=EXCLUDED.participation_id AND "AdWorkerRelays".challenge_id=EXCLUDED.challenge_id"#)
        .bind(handle.workload_id).bind(handle.assignment_id).bind(handle.generation).bind(participant).bind(challenge)
        .execute(pool).await.map_err(database_error)?.rows_affected();
    if rows != 1 {
        return Err(AppError::conflict("The worker assignment changed"));
    }
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let address = sqlx::query_as(
                r#"SELECT host,port FROM "AdWorkerRelays"
                WHERE workload_id=$1 AND ready_until>clock_timestamp()"#,
            )
            .bind(handle.workload_id)
            .fetch_optional(pool)
            .await
            .map_err(database_error)?;
            if let Some(address) = address {
                return Ok(address);
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .map_err(|_| AppError::unavailable("The private worker endpoint is not ready"))?
}

// Read only the current published generation and exact socket. An unpublished
// candidate, a withdrawn endpoint or a reassigned socket cannot admit a stream.
async fn route(
    pool: &PgPool,
    id: Uuid,
    addr: SocketAddr,
) -> Result<Option<(Uuid, TcpProxyRequest)>, sqlx::Error> {
    let row = sqlx::query_as::<_, (Uuid, Uuid, i64, Json<EndpointRef>)>(
        r#"
        SELECT w.worker_id,w.assignment_id,w.generation,w.spec->'primaryEndpoint'
        FROM "AdWorkerRelays" e JOIN "WorkerWorkloads" w ON w.id=e.workload_id
        JOIN "AdTeamServices" s USING(participation_id,challenge_id)
        WHERE w.id=$1 AND e.host=$2 AND e.port=$3 AND s.host=e.host AND s.port=e.port
        AND e.ready_until>clock_timestamp() AND w.owner_kind='ad-team-service'
        AND w.owner_key=concat('ad-service:',e.participation_id,':',e.challenge_id)
        AND w.desired_state='Present' AND w.observed_state='Ready'
        AND s.container_id=concat('rsctf-worker:',w.id,':',w.assignment_id,':',w.generation)
        AND s.published_image_backend=s.container_id"#,
    )
    .bind(id)
    .bind(addr.ip().to_string())
    .bind(i32::from(addr.port()))
    .fetch_optional(pool)
    .await?;
    Ok(
        row.and_then(|(worker, assignment_id, generation, endpoint)| {
            Some((
                worker,
                TcpProxyRequest {
                    fence: WorkloadFence {
                        workload_id: id,
                        assignment_id,
                        generation: generation.try_into().ok()?,
                    },
                    service: endpoint.0.service,
                    port: endpoint.0.port,
                    replica: None,
                },
            ))
        }),
    )
}

async fn forward(
    mut socket: TcpStream,
    pool: PgPool,
    workers: WorkerService,
    id: Uuid,
    addr: SocketAddr,
) {
    let Ok(Some((worker, request))) = route(&pool, id, addr).await else {
        return;
    };
    let fence = request.fence;
    let Ok(mut stream) = workers
        .open_data_stream(worker, DataStreamRequest::TcpProxy(request))
        .await
    else {
        return;
    };
    let watch = async {
        loop {
            tokio::time::sleep(REFRESH).await;
            if !ad_vpn::owns_instance_lease()
                || !matches!(tokio::time::timeout(REFRESH,route(&pool,id,addr)).await, Ok(Ok(Some((_, next)))) if next.fence==fence)
            {
                break;
            }
        }
    };
    tokio::select! {
        _ = tokio::io::copy_bidirectional(&mut socket,&mut stream) => {},
        _ = watch => {}
    }
}

async fn listen(
    listener: TcpListener,
    pool: PgPool,
    workers: WorkerService,
    id: Uuid,
    addr: SocketAddr,
) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            result=listener.accept() => {
                let Ok((socket,_))=result else {break};
                if ad_vpn::owns_instance_lease() && connections.len()<64 {
                    connections.spawn(forward(socket,pool.clone(),workers.clone(),id,addr));
                }
            }
            _=connections.join_next(), if !connections.is_empty() => {}
        }
    }
}

struct ListenerTask(JoinHandle<()>);
impl Drop for ListenerTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn refresh(
    state: &SharedState,
    workers: &WorkerService,
    listeners: &mut HashMap<Uuid, ListenerTask>,
) -> AppResult<()> {
    let mut wanted = HashSet::new();
    let mut after = Uuid::nil();
    loop {
        // Release the read connection before reserving sockets. The lease
        // monitor and workload reconciliation also use this pool.
        let rows=sqlx::query_as::<_,(Uuid,Option<String>,Option<i32>)>(r#"
        SELECT e.workload_id,e.host,e.port FROM "AdWorkerRelays" e
        JOIN "WorkerWorkloads" w ON w.id=e.workload_id AND w.desired_state='Present'
        JOIN "Participations" p ON p.id=e.participation_id AND p.status=1
        JOIN "Teams" t ON t.id=p.team_id AND NOT t.deletion_pending
        JOIN "Games" g ON g.id=p.game_id AND NOT g.deletion_pending AND g.end_time_utc>clock_timestamp()
        JOIN "GameChallenges" c ON c.id=e.challenge_id AND c.game_id=g.id AND c.is_enabled
            AND c.review_status=0 AND c."Type"=4 AND NOT c.ad_self_hosted AND NOT c.deletion_pending
        WHERE w.owner_kind='ad-team-service' AND w.owner_key=concat('ad-service:',p.id,':',c.id)
        AND e.workload_id>$1 ORDER BY e.workload_id LIMIT 128"#)
            .bind(after).fetch_all(state.pg()).await.map_err(database_error)?;
        let Some(last) = rows.last() else { break };
        after = last.0;
        let full_page = rows.len() == 128;
        for (id, old_host, old_port) in rows {
            wanted.insert(id);
            if listeners.get(&id).is_some_and(|task| !task.0.is_finished()) {
                continue;
            }
            let host =
                crate::services::byoc_tunnel::services_ip().map_err(AppError::unavailable)?;
            if old_host.as_ref().is_some_and(|old| old != &host) {
                continue;
            }
            let port = u16::try_from(old_port.unwrap_or(0))
                .map_err(|_| AppError::internal("Invalid relay port"))?;
            let Ok(listener) = TcpListener::bind((host.as_str(), port)).await else {
                continue;
            };
            let addr = listener
                .local_addr()
                .map_err(|e| AppError::internal(e.to_string()))?;
            let updated = sqlx::query(
                r#"UPDATE "AdWorkerRelays" SET host=$2,port=$3
            WHERE workload_id=$1 AND (host IS NULL OR (host=$2 AND port=$3))"#,
            )
            .bind(id)
            .bind(&host)
            .bind(i32::from(addr.port()))
            .execute(state.pg())
            .await;
            match updated {
                Ok(r) if r.rows_affected() == 1 => {}
                Err(sqlx::Error::Database(e)) if e.is_unique_violation() => continue,
                Err(e) => return Err(database_error(e)),
                _ => continue,
            }
            listeners.insert(
                id,
                ListenerTask(tokio::spawn(listen(
                    listener,
                    state.pg().clone(),
                    workers.clone(),
                    id,
                    addr,
                ))),
            );
        }
        if !full_page {
            break;
        }
    }
    listeners.retain(|id, task| wanted.contains(id) && !task.0.is_finished());
    if !listeners.is_empty() {
        let ids: Vec<_> = listeners.keys().copied().collect();
        sqlx::query(r#"UPDATE "AdWorkerRelays" SET ready_until=clock_timestamp()+interval '10 seconds' WHERE workload_id=ANY($1) AND (ready_until IS NULL OR ready_until < clock_timestamp()+interval '6 seconds')"#)
            .bind(ids).execute(state.pg()).await.map_err(database_error)?;
    }
    Ok(())
}

pub fn start(
    state: SharedState,
    workers: WorkerService,
    mut shutdown: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut listeners = HashMap::new();
        let mut timer = tokio::time::interval(REFRESH);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _=shutdown.changed()=>break,
                _=timer.tick()=>{
                    if !ad_vpn::owns_instance_lease() {listeners.clear();continue;}
                    if let Err(error)=refresh(&state,&workers,&mut listeners).await {
                        listeners.clear();
                        tracing::warn!(%error,"private worker endpoint reconciliation failed");
                    }
                }
            }
        }
    })
}
