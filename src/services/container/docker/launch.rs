use super::super::*;

impl DockerContainerManager {
    pub(in crate::services::container) async fn create_with_donor(
        &self,
        spec: ContainerSpec,
        donor: Option<&str>,
    ) -> AppResult<ContainerInfo> {
        validate_docker_container_spec(&spec)?;
        let docker = self.client()?;
        let storage_quota_enforced = self.detect_storage_quota_enforcement().await?;
        docker::require_storage_quota(storage_quota_enforced, self.allow_unbounded_storage)?;
        let storage_opt = writable_layer_storage_option(storage_quota_enforced, spec.storage_limit);
        let launch_fingerprint = launch_spec_fingerprint(&spec);

        // 1. Pull an absent repository digest without changing identity. A
        // vanished daemon-local ID must have been repaired from its trusted
        // archive before this boundary; surface a retryable infrastructure
        // response if a prune races the final create instead of leaking a 500.
        let inspected_image = match self.inspect_image(&spec.image).await? {
            Ok(image) => image,
            Err(_) if crate::services::challenge_images::is_repository_digest(&spec.image) => {
                if let Some(error) = self.pull_repository_digest(&spec.image).await? {
                    tracing::warn!(image = %spec.image, %error, "immutable image pull failed");
                }
                self.inspect_image(&spec.image).await?.map_err(|error| {
                    tracing::error!(image = %spec.image, %error, "immutable repository image remains unavailable after pull");
                    AppError::unavailable(
                        "The challenge image could not be pulled by the container host. Retry later or ask an administrator to rebuild it.",
                    )
                })?
            }
            Err(error) => {
                tracing::error!(image = %spec.image, %error, "immutable challenge image is unavailable at container create");
                return Err(AppError::unavailable(
                    "The challenge image is unavailable on this container host. Retry later or ask an administrator to rebuild it.",
                ));
            }
        };
        let restricted_profile = image_requests_restricted_profile(&inspected_image);

        // 2. Environment: caller-supplied vars plus the dynamic flag contract.
        let mut env: Vec<String> = spec.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        if let Some(flag) = spec.flag.as_deref() {
            if !flag.is_empty() {
                env.push(format!("{FLAG_ENV}={flag}"));
                env.push(format!("{FLAG_FILE_ENV}={FLAG_FILE_PATH}"));
            }
        }

        // 3. Port publishing: expose the challenge port. For a normal container we
        // bind it to host port "0" so the daemon picks a free ephemeral port. For
        // an A&D-over-VPN container (`ad_network`) we publish NO host port — the
        // service is reachable only via its in-VPN IP over the tunnel.
        let port_key = format!("{}/tcp", spec.expose_port);
        let exposed_ports = spec
            .publish_port
            .then(|| HashMap::from([(port_key.clone(), HashMap::new())]));
        let port_bindings: Option<HashMap<String, Option<Vec<PortBinding>>>> =
            docker::published_bind_ip(&spec, self.proxy_bind)?.map(|host_ip| {
                HashMap::from([(
                    port_key.clone(),
                    Some(vec![PortBinding {
                        host_ip: Some(host_ip),
                        host_port: Some("0".to_string()),
                    }]),
                )])
            });

        // 4. Resource limits: memory (MB → bytes), CPU quota (whole cores →
        // nano-cpus), a pids cap to blunt fork bombs, and an init process so a
        // forking PID 1 cannot accumulate zombies.
        let mut host_config =
            docker::challenge_host_config(&spec, restricted_profile, storage_opt, port_bindings);

        if let Some(id) = donor {
            host_config.volumes_from = Some(vec![format!("{id}:rw")]);
        }

        let mut labels = scoped_managed_labels(&self.scope);
        labels.insert(LAUNCH_SPEC_LABEL.to_string(), launch_fingerprint.clone());
        stamp_restricted_profile(&mut labels, restricted_profile);
        stamp_storage_quota_policy(&mut labels, storage_quota_enforced);
        if let Some(operation_id) = spec.operation_id.as_ref() {
            labels.insert(OPERATION_LABEL.to_string(), operation_id.clone());
        }

        // A&D-over-VPN always attaches only to the internal services bridge.
        // Docker allowEgress is rejected above: adding a shared external bridge
        // would permit cross-workload, private-network, and metadata access.
        let networking_config = if let Some(net) = spec.ad_network.as_ref() {
            let services_cidr = crate::services::ad_vpn::services_cidr();
            self.ensure_network(net, services_cidr.as_str()).await?;
            Some(NetworkingConfig {
                endpoints_config: HashMap::from([(net.clone(), EndpointSettings::default())]),
            })
        } else {
            None
        };

        let config = Config {
            image: Some(spec.image.clone()),
            env: Some(env),
            exposed_ports,
            labels: Some(labels),
            host_config: Some(host_config),
            networking_config,
            // Only an image that already defines a health check gets one, and
            // only to slow it down: a container never gains a probe its image
            // did not ship.
            healthcheck: docker::clamped_image_health_config(&inspected_image),
            ..Default::default()
        };

        // 5. Create with a readable unique name. Never remove a 409 holder: without
        // an ownership proof it may be another user's live challenge container.
        let scoped_operation = scoped_operation_id(&self.scope, spec.operation_id.as_deref());
        let mut name = container_name(&spec.image, &spec.env, scoped_operation.as_deref());
        let discovered = discover_operation_container(
            docker,
            &self.scope,
            &spec,
            &launch_fingerprint,
            restricted_profile,
            storage_quota_enforced,
        )
        .await?;
        let (id, adopted) = if let Some(id) = discovered {
            (id, true)
        } else {
            match docker_admission()
                .lifecycle(
                    "create_container",
                    docker.create_container(
                        Some(CreateContainerOptions::<String> {
                            name: name.clone(),
                            ..Default::default()
                        }),
                        config.clone(),
                    ),
                )
                .await?
            {
                Ok(created) => (created.id, false),
                Err(e) if is_conflict(&e) && spec.operation_id.is_some() => {
                    let existing = docker_admission()
                        .read("inspect_container", docker.inspect_container(&name, None))
                        .await?
                        .map_err(|inspect| {
                            AppError::internal(format!(
                                "container operation {name} conflicted but could not be adopted: {inspect}"
                            ))
                        })?;
                    let id = adopt_operation_container(
                        docker,
                        &existing,
                        &self.scope,
                        &spec,
                        &launch_fingerprint,
                        restricted_profile,
                        storage_quota_enforced,
                    )
                    .await?;
                    (id, true)
                }
                Err(e) if is_conflict(&e) => {
                    name = container_name(&spec.image, &spec.env, None);
                    let created = docker_admission()
                        .lifecycle(
                            "create_container",
                            docker.create_container(
                                Some(CreateContainerOptions::<String> {
                                    name,
                                    ..Default::default()
                                }),
                                config,
                            ),
                        )
                        .await?
                        .map_err(|e| {
                            AppError::internal(format!("failed to create container: {e}"))
                        })?;
                    (created.id, false)
                }
                Err(e) => {
                    return Err(AppError::internal(format!(
                        "failed to create container: {e}"
                    )));
                }
            }
        };
        // 6. Start, reconciling an adopter that won the concurrent start race.
        self.start_or_reconcile_container(docker, &id, spec.operation_id.is_some(), adopted)
            .await?;

        // 7. Inspect to read back state + the published host port.
        let info = docker_admission()
            .read("inspect_container", docker.inspect_container(&id, None))
            .await?
            .map_err(|e| AppError::internal(format!("failed to inspect container: {e}")))?;

        let status = map_status(info.state.as_ref().and_then(|s| s.status));

        // A&D-over-VPN: the endpoint is the container's in-VPN IP + internal port,
        // not a published host port. Read the Docker-assigned IP back from the net.
        if let Some(net) = &spec.ad_network {
            let ip = info
                .network_settings
                .as_ref()
                .and_then(|ns| ns.networks.as_ref())
                .and_then(|nets| nets.get(net))
                .and_then(|ep| ep.ip_address.clone())
                .filter(|s| !s.is_empty())
                .unwrap_or_default();
            return Ok(ContainerInfo {
                id,
                ip,
                port: spec.expose_port,
                status: status.to_string(),
            });
        }

        // Published host port for the challenge's exposed port.
        let binding = info
            .network_settings
            .as_ref()
            .and_then(|ns| ns.ports.as_ref())
            .and_then(|ports| ports.get(&port_key))
            .and_then(|v| v.as_ref())
            .and_then(|v| v.first());

        let published_port = binding
            .and_then(|b| b.host_port.as_deref())
            .and_then(|p| p.parse::<i32>().ok());
        if spec.proxy_only && published_port.is_none() {
            return Err(AppError::unavailable(
                "Docker did not allocate the private PlatformProxy port",
            ));
        }
        let port = published_port.unwrap_or(spec.expose_port);

        // Routable IP: prefer the configured public entry, then the binding's
        // host IP (unless it's the wildcard), then loopback. We deliberately do
        // NOT surface the container's *internal* network IP as the primary
        // endpoint — with published ports the reachable address is host-side.
        let ip = docker::advertised_endpoint_ip(
            &spec,
            self.public_entry.as_deref(),
            binding.and_then(|binding| binding.host_ip.as_deref()),
            self.proxy_bind,
        )?;

        Ok(ContainerInfo {
            id,
            ip,
            port,
            status: status.to_string(),
        })
    }
}
