use super::*;
use crate::services::{ad::maintenance::database_error, ad::service_lifecycle::service_lock_key};

impl WorkerContainerManager {
    /// Keep the durable assignment, private listener and storage allocation
    /// while the worker stages the requested image. An operation receipt in
    /// the definition distinguishes an explicit restart of the same image.
    pub(crate) async fn revise_team_service(
        &self,
        current: WorkerHandle,
        participant: i32,
        challenge: i32,
        image: &str,
        operation: &str,
    ) -> AppResult<ContainerInfo> {
        let (workload, spec) = self
            .current_rollout_workload(current)
            .await?
            .ok_or_else(|| AppError::conflict("The team worker assignment changed"))?;
        if workload.owner_kind != "ad-team-service"
            || workload.owner_key != service_lock_key(participant, challenge)
            || spec.game_kind != GameKind::AttackDefense
            || spec.services.len() != 1
        {
            return Err(AppError::conflict(
                "The assignment does not belong to this team service",
            ));
        }
        let (image, pinned) = image_identity(image)?;
        if pinned.is_some_and(|id| id != workload.worker_id) {
            return Err(AppError::conflict(
                "The revision image belongs to a different worker",
            ));
        }
        let mut spec = spec.into_inner();
        let service = &mut spec.services[0];
        service.image = image;
        service
            .environment
            .insert("RSCTF_PATCH_OPERATION".into(), operation.to_string());
        let spec = ValidatedWorkloadSpec::try_from(spec)
            .map_err(|e| AppError::bad_request(e.to_string()))?;
        let (definition, _, _) = workload_definition(&spec)?;
        let update = self
            .store
            .update_workload_definition(&UpdateWorkload {
                id: current.workload_id,
                assignment_id: current.assignment_id,
                expected_generation: current.generation,
                definition,
            })
            .await
            .map_err(store_error)?;
        let generation = match update {
            DefinitionUpdateOutcome::Updated { generation }
            | DefinitionUpdateOutcome::AlreadyCurrent { generation } => generation,
            DefinitionUpdateOutcome::Stale => {
                return Err(AppError::conflict("The team worker revision changed"))
            }
            _ => {
                return Err(AppError::unavailable(
                    "The worker cannot accept this revision",
                ))
            }
        };
        let next = WorkerHandle {
            generation,
            ..current
        };
        self.wait_until_ready(next).await?;
        self.team_endpoint(next, participant, challenge).await
    }

    /// Called under the normal team publication lock. Only organizer image
    /// configuration selects placement and only a registered team mount is kept.
    pub(crate) async fn place_team_service(
        &self,
        spec: &ContainerSpec,
        participant: i32,
        challenge: i32,
    ) -> AppResult<ContainerInfo> {
        validate_container_spec(spec)?;
        if spec.game_kind != GameKind::AttackDefense
            || spec.allow_egress
            || spec.ad_network.is_none()
        {
            return Err(AppError::bad_request(
                "A hosted team worker requires the isolated A&D network",
            ));
        }
        let (image, worker) = image_identity(&spec.image)?;
        let (platform, _) = self.platform_for(worker).await?;
        if platform.operating_system != OperatingSystem::Linux {
            return Err(AppError::bad_request("A hosted team worker requires Linux"));
        }
        let mount = sqlx::query_scalar::<_, String>(
            r#"UPDATE "AdPatchStarters" SET data_mount_claimed=TRUE
            WHERE challenge_id=$1 AND data_mount IS NOT NULL RETURNING data_mount"#,
        )
        .bind(challenge)
        .fetch_optional(self.store.pool())
        .await
        .map_err(database_error)?;
        let workload = ValidatedWorkloadSpec::try_from(WorkloadSpec {
            game_kind: GameKind::AttackDefense,
            platform,
            services: vec![ServiceSpec {
                name: "challenge".into(),
                image,
                replicas: 1,
                stateless: false,
                resources: ResourceLimits {
                    cpu_millis: (spec.cpu_count as u32)
                        .checked_mul(1000)
                        .ok_or_else(|| AppError::bad_request("Invalid team service CPU count"))?,
                    memory_bytes: (spec.memory_limit as u64) << 20,
                    storage_bytes: (spec.storage_limit as u64) << 20,
                },
                environment: spec.env.iter().cloned().collect(),
                ports: vec![ServicePort {
                    name: "service".into(),
                    container_port: u16::try_from(spec.expose_port)
                        .map_err(|_| AppError::bad_request("Invalid team service port"))?,
                    protocol: PortProtocol::Tcp,
                }],
            }],
            primary_endpoint: EndpointRef {
                service: "challenge".into(),
                port: "service".into(),
            },
            flag_target: Some(FlagTarget {
                service: "challenge".into(),
                path: "/flag".into(),
            }),
            team_data_mount: mount,
        })
        .map_err(|e| AppError::bad_request(e.to_string()))?;
        let workload =
            crate::services::challenge_workloads::with_flag(workload, spec.flag.clone())?;
        let handle = self
            .place_owned(
                &workload,
                "ad-team-service",
                service_lock_key(participant, challenge),
            )
            .await?;
        self.team_endpoint(handle, participant, challenge).await
    }

    async fn team_endpoint(
        &self,
        handle: WorkerHandle,
        participant: i32,
        challenge: i32,
    ) -> AppResult<ContainerInfo> {
        let (ip, port) = crate::services::worker::team_relay::address(
            self.store.pool(),
            handle,
            participant,
            challenge,
        )
        .await?;
        Ok(ContainerInfo {
            id: handle.encode(),
            ip,
            port,
            status: "running".into(),
        })
    }
}
