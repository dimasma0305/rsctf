use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bollard::container::InspectContainerOptions;
use futures_util::StreamExt;
use rsctf_worker_protocol::{CommandErrorCode, ReplicaStatus, TcpProxyRequest, WorkloadFence};
use tokio::net::TcpStream;
use uuid::Uuid;

use super::{
    docker_error, label, network_name, replica_status, DockerRuntime, RuntimeError,
    LABEL_ASSIGNMENT, LABEL_GENERATION, LABEL_PORT_PREFIX, LABEL_REPLICA, LABEL_SERVICE,
};

const READINESS_CONNECT_TIMEOUT: Duration = Duration::from_millis(750);
const MAX_CONCURRENT_PORT_PROBES: usize = 8;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct EndpointCacheKey {
    pub(super) workload_id: Uuid,
    assignment_id: Uuid,
    generation: u64,
    service: String,
    port: String,
}

#[derive(Clone, Debug)]
pub(super) struct EndpointTarget {
    pub(super) container_id: String,
    replica: u16,
    pub(super) address: SocketAddr,
}

impl DockerRuntime {
    pub(super) async fn matching_container_ids(
        &self,
        fence: WorkloadFence,
        service_name: &str,
    ) -> Result<Vec<(u16, String)>, RuntimeError> {
        let containers = self.existing_containers(fence.workload_id).await?;
        let mut result = Vec::new();
        for container in containers {
            let labels = container.labels.as_ref();
            if label(labels, LABEL_ASSIGNMENT) != Some(fence.assignment_id.to_string()).as_deref()
                || label(labels, LABEL_GENERATION) != Some(fence.generation.to_string()).as_deref()
                || label(labels, LABEL_SERVICE) != Some(service_name)
                || container.state.as_deref() != Some("running")
            {
                continue;
            }
            let replica = label(labels, LABEL_REPLICA)
                .and_then(|value| value.parse::<u16>().ok())
                .ok_or_else(|| {
                    RuntimeError::new(CommandErrorCode::Internal, "invalid replica label")
                })?;
            let id = container.id.ok_or_else(|| {
                RuntimeError::new(CommandErrorCode::Internal, "Docker omitted container ID")
            })?;
            if !self.ready_containers.contains(&id) {
                continue;
            }
            result.push((replica, id));
        }
        result.sort_by_key(|(replica, _)| *replica);
        Ok(result)
    }

    async fn mapped_address(
        &self,
        container_id: &str,
        fence: WorkloadFence,
        port_name: &str,
    ) -> Result<SocketAddr, RuntimeError> {
        let inspect = self
            .docker
            .inspect_container(container_id, None::<InspectContainerOptions>)
            .await
            .map_err(|error| docker_error("inspect workload endpoint", error))?;
        let labels = inspect
            .config
            .and_then(|config| config.labels)
            .unwrap_or_default();
        let port = labels
            .get(&format!("io.rsctf.port.{port_name}"))
            .and_then(|value| value.parse::<u16>().ok())
            .ok_or_else(|| {
                RuntimeError::new(
                    CommandErrorCode::NotFound,
                    "named container port is missing",
                )
            })?;
        let networks = inspect
            .network_settings
            .and_then(|settings| settings.networks)
            .ok_or_else(|| {
                RuntimeError::new(
                    CommandErrorCode::NotFound,
                    "container has no workload network",
                )
            })?;
        let network = networks.get(&network_name(fence)).ok_or_else(|| {
            RuntimeError::new(
                CommandErrorCode::NotFound,
                "container is not attached to its fenced workload network",
            )
        })?;
        let ip = network
            .ip_address
            .as_deref()
            .filter(|value| !value.is_empty())
            .or_else(|| {
                network
                    .global_ipv6_address
                    .as_deref()
                    .filter(|value| !value.is_empty())
            })
            .ok_or_else(|| {
                RuntimeError::new(
                    CommandErrorCode::NotFound,
                    "Docker omitted container address",
                )
            })?
            .parse::<IpAddr>()
            .map_err(|_| {
                RuntimeError::new(
                    CommandErrorCode::Internal,
                    "Docker returned an invalid container address",
                )
            })?;
        Ok(SocketAddr::new(ip, port))
    }

    pub(super) fn endpoint_key(request: &TcpProxyRequest) -> EndpointCacheKey {
        EndpointCacheKey {
            workload_id: request.fence.workload_id,
            assignment_id: request.fence.assignment_id,
            generation: request.fence.generation,
            service: request.service.clone(),
            port: request.port.clone(),
        }
    }

    pub(super) fn invalidate_workload_endpoints(&self, workload_id: Uuid) {
        self.endpoint_cache
            .retain(|key, _| key.workload_id != workload_id);
        self.round_robin
            .retain(|key, _| key.workload_id != workload_id);
    }

    pub(super) async fn resolve_endpoints(
        &self,
        request: &TcpProxyRequest,
        refresh: bool,
    ) -> Result<Vec<EndpointTarget>, RuntimeError> {
        let key = Self::endpoint_key(request);
        if !refresh {
            if let Some(cached) = self.endpoint_cache.get(&key) {
                return Ok(cached.value().clone());
            }
        }
        let containers = self
            .matching_container_ids(request.fence, &request.service)
            .await?;
        let mut targets = Vec::with_capacity(containers.len());
        for (replica, container_id) in containers {
            targets.push(EndpointTarget {
                container_id: container_id.clone(),
                replica,
                address: self
                    .mapped_address(&container_id, request.fence, &request.port)
                    .await?,
            });
        }
        if !targets.is_empty() {
            self.endpoint_cache.insert(key, targets.clone());
        }
        Ok(targets)
    }

    pub(super) fn select_endpoint(
        &self,
        request: &TcpProxyRequest,
        targets: &[EndpointTarget],
    ) -> Result<EndpointTarget, RuntimeError> {
        if targets.is_empty() {
            return Err(RuntimeError::new(
                CommandErrorCode::NotFound,
                "no ready replicas match the data request",
            ));
        }
        let selected = if let Some(replica) = request.replica {
            targets
                .iter()
                .find(|candidate| candidate.replica == replica)
        } else {
            let counter = self
                .round_robin
                .entry(Self::endpoint_key(request))
                .or_insert_with(|| AtomicU64::new(0));
            let index = counter.fetch_add(1, Ordering::Relaxed) as usize % targets.len();
            targets.get(index)
        }
        .ok_or_else(|| {
            RuntimeError::new(CommandErrorCode::NotFound, "requested replica is not ready")
        })?;
        Ok(selected.clone())
    }

    pub(super) async fn replica_status_with_readiness(
        &self,
        container: &bollard::models::ContainerSummary,
        fence: WorkloadFence,
    ) -> ReplicaStatus {
        let mut status = replica_status(container);
        let Some(container_id) = container.id.as_deref() else {
            status.ready = false;
            status.detail = Some("Docker omitted the container identity".to_string());
            return status;
        };
        if !status.ready {
            self.ready_containers.remove(container_id);
            return status;
        }
        if self.ready_containers.contains(container_id) {
            return status;
        }

        match declared_endpoint_addresses(container, fence) {
            Ok(addresses) => {
                if endpoints_accept_connections(addresses).await {
                    self.ready_containers.insert(container_id.to_string());
                } else {
                    status.ready = false;
                    status.detail =
                        Some("declared TCP endpoint is not accepting connections".to_string());
                }
            }
            Err(error) => {
                tracing::debug!(
                    workload_id = %fence.workload_id,
                    assignment_id = %fence.assignment_id,
                    generation = fence.generation,
                    container_id,
                    code = ?error.code,
                    error = %error,
                    "workload replica readiness metadata is invalid"
                );
                status.ready = false;
                status.detail = Some("declared TCP endpoint is unavailable".to_string());
            }
        }
        status
    }

    pub(super) async fn dial_endpoint(address: SocketAddr) -> Result<TcpStream, RuntimeError> {
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            TcpStream::connect(address),
        )
        .await
        .map_err(|_| {
            RuntimeError::new(
                CommandErrorCode::Timeout,
                "container endpoint dial timed out",
            )
        })?
        .map_err(|error| {
            RuntimeError::new(
                CommandErrorCode::RuntimeUnavailable,
                format!("container endpoint dial failed: {error}"),
            )
        })
    }
}

fn declared_endpoint_addresses(
    container: &bollard::models::ContainerSummary,
    fence: WorkloadFence,
) -> Result<Vec<SocketAddr>, RuntimeError> {
    let labels = container.labels.as_ref().ok_or_else(|| {
        RuntimeError::new(
            CommandErrorCode::Internal,
            "workload container has no labels",
        )
    })?;
    let mut ports = labels
        .iter()
        .filter_map(|(key, value)| {
            key.strip_prefix(LABEL_PORT_PREFIX)
                .map(|name| (name, value))
        })
        .map(|(name, value)| {
            if name.is_empty() {
                return Err(RuntimeError::new(
                    CommandErrorCode::Internal,
                    "workload container has an invalid port label",
                ));
            }
            value.parse::<u16>().map_err(|_| {
                RuntimeError::new(
                    CommandErrorCode::Internal,
                    "workload container has an invalid port label",
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if ports.is_empty() {
        return Err(RuntimeError::new(
            CommandErrorCode::Internal,
            "workload container has no declared TCP ports",
        ));
    }
    ports.sort_unstable();
    ports.dedup();

    let networks = container
        .network_settings
        .as_ref()
        .and_then(|settings| settings.networks.as_ref())
        .ok_or_else(|| {
            RuntimeError::new(
                CommandErrorCode::NotFound,
                "container has no workload network",
            )
        })?;
    let network = networks.get(&network_name(fence)).ok_or_else(|| {
        RuntimeError::new(
            CommandErrorCode::NotFound,
            "container is not attached to its fenced workload network",
        )
    })?;
    let ip = network
        .ip_address
        .as_deref()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            network
                .global_ipv6_address
                .as_deref()
                .filter(|value| !value.is_empty())
        })
        .ok_or_else(|| {
            RuntimeError::new(
                CommandErrorCode::NotFound,
                "Docker omitted container address",
            )
        })?
        .parse::<IpAddr>()
        .map_err(|_| {
            RuntimeError::new(
                CommandErrorCode::Internal,
                "Docker returned an invalid container address",
            )
        })?;
    Ok(ports
        .into_iter()
        .map(|port| SocketAddr::new(ip, port))
        .collect())
}

async fn endpoints_accept_connections(addresses: Vec<SocketAddr>) -> bool {
    futures_util::stream::iter(addresses)
        .map(|address| async move {
            matches!(
                tokio::time::timeout(READINESS_CONNECT_TIMEOUT, TcpStream::connect(address),).await,
                Ok(Ok(_))
            )
        })
        .buffer_unordered(MAX_CONCURRENT_PORT_PROBES)
        .all(|ready| async move { ready })
        .await
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use bollard::models::{ContainerSummary, ContainerSummaryNetworkSettings, EndpointSettings};
    use tokio::net::TcpListener;

    use super::*;

    fn fence() -> WorkloadFence {
        WorkloadFence {
            workload_id: Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap(),
            assignment_id: Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap(),
            generation: 3,
        }
    }

    fn summary(labels: HashMap<String, String>) -> ContainerSummary {
        ContainerSummary {
            labels: Some(labels),
            network_settings: Some(ContainerSummaryNetworkSettings {
                networks: Some(HashMap::from([(
                    network_name(fence()),
                    EndpointSettings {
                        ip_address: Some("127.0.0.1".to_string()),
                        ..Default::default()
                    },
                )])),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn derives_every_declared_port_from_the_fenced_network() {
        let container = summary(HashMap::from([
            (format!("{LABEL_PORT_PREFIX}web"), "8080".to_string()),
            (format!("{LABEL_PORT_PREFIX}admin"), "9090".to_string()),
        ]));

        assert_eq!(
            declared_endpoint_addresses(&container, fence()).unwrap(),
            vec![
                "127.0.0.1:8080".parse().unwrap(),
                "127.0.0.1:9090".parse().unwrap(),
            ]
        );
    }

    #[test]
    fn rejects_missing_and_invalid_declared_ports() {
        assert!(declared_endpoint_addresses(&summary(HashMap::new()), fence()).is_err());
        assert!(declared_endpoint_addresses(
            &summary(HashMap::from([(
                format!("{LABEL_PORT_PREFIX}web"),
                "invalid".to_string(),
            )])),
            fence(),
        )
        .is_err());
    }

    #[tokio::test]
    async fn readiness_requires_every_declared_endpoint_to_accept() {
        let listening = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let listening_address = listening.local_addr().unwrap();
        assert!(endpoints_accept_connections(vec![listening_address]).await);

        let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let closed_address = closed.local_addr().unwrap();
        drop(closed);
        assert!(!endpoints_accept_connections(vec![listening_address, closed_address,]).await);
    }
}
