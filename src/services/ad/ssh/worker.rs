use super::*;
use crate::services::worker::WorkerHandle;
use rsctf_worker_protocol::{DataStreamRequest, InteractiveExecRequest, WorkloadFence};

/// Bind the key's participant and challenge to the same published workload
/// used by the data lane. A backend ID copied to a different service cannot
/// grant that service's captain a terminal on the original assignment.
pub(crate) async fn current_target(
    pool: &sqlx::PgPool,
    participant: i32,
    challenge: i32,
    handle: WorkerHandle,
) -> Result<Option<(uuid::Uuid, String)>, sqlx::Error> {
    sqlx::query_as(r#"SELECT w.worker_id,w.spec->'primaryEndpoint'->>'service'
        FROM "WorkerWorkloads" w JOIN "AdTeamServices" s ON s.participation_id=$4 AND s.challenge_id=$5
        WHERE w.id=$1 AND w.assignment_id=$2 AND w.generation=$3 AND w.desired_state='Present'
        AND w.observed_state='Ready' AND w.owner_kind='ad-team-service'
        AND w.owner_key=concat('ad-service:',s.participation_id,':',s.challenge_id)
        AND s.container_id=$6 AND s.published_image_backend=s.container_id
        AND s.host<>'' AND s.port BETWEEN 1 AND 65535"#)
        .bind(handle.workload_id).bind(handle.assignment_id).bind(handle.generation)
        .bind(participant).bind(challenge).bind(handle.encode()).fetch_optional(pool).await
}

impl BastionHandler {
    pub(super) async fn open_worker_shell(
        &mut self,
        key: SshLeaseKey,
        target: WorkerHandle,
        channel: ChannelId,
        handle: russh::server::Handle,
    ) -> Result<(), String> {
        let pid = key.participation_id;
        let cid = key.challenge_id;
        let (worker, service) = current_target(self.st.pg(), pid, cid, target)
            .await
            .map_err(|_| "database error")?
            .ok_or("your team's service is no longer available")?;
        let workers = self
            .st
            .workers
            .as_ref()
            .ok_or("worker connection unavailable")?;
        let stream = workers
            .open_data_stream(
                worker,
                DataStreamRequest::InteractiveExec(InteractiveExecRequest {
                    fence: WorkloadFence {
                        workload_id: target.workload_id,
                        assignment_id: target.assignment_id,
                        generation: target
                            .generation
                            .try_into()
                            .map_err(|_| "invalid worker generation")?,
                    },
                    service,
                    replica: 0,
                    columns: self.cols.clamp(1, 512) as u16,
                    rows: self.rows.clamp(1, 256) as u16,
                }),
            )
            .await
            .map_err(|error| {
                tracing::warn!(pid,cid,%error,"could not attach to team worker terminal");
                "the assigned worker could not open a team terminal"
            })?;
        let st = self.st.clone();
        self.attach_remote_shell(stream,channel,handle,async move {
            let assignment_changed=async {
                loop {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    if !matches!(tokio::time::timeout(Duration::from_secs(3),current_target(st.pg(),pid,cid,target)).await,
                        Ok(Ok(Some((id,_)))) if id==worker) {return;}
                }
            };
            tokio::select! {
                _=wait_for_ssh_revocation(st.clone(),key.key_id,pid,cid,key.fingerprint)=>{},
                _=assignment_changed=>{},
            }
        });
        Ok(())
    }
}
