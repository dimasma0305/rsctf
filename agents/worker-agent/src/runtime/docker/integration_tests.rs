//! Opt-in Docker smoke test. Set 'RSCTF_WORKER_TEST_IMAGE_ID' to a full
//! 'sha256:...' image ID whose default command listens on TCP port 8080.
//! The test requires an otherwise-unclaimed disposable Docker daemon.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::Duration;

use bollard::container::{ListContainersOptions, RemoveContainerOptions};
use bollard::network::ListNetworksOptions;
use bollard::volume::RemoveVolumeOptions;
use bollard::Docker;
use rsctf_worker_protocol::{
    EndpointRef, EnsureAbsent, EnsureWorkload, FlagTarget, GameKind, ImageIdentity,
    ObservedWorkloadState, OperatingSystem, Platform, PortProtocol, ResourceLimits, ServicePort,
    ServiceSpec, TcpProxyRequest, ValidatedWorkloadSpec, WorkloadFence, WorkloadSpec, WriteFlag,
};
use uuid::Uuid;

use super::{is_not_found, DockerRuntime, LABEL_MANAGED, LABEL_WORKER};
use crate::runtime::{RuntimeOptions, WorkerRuntime};

const DAEMON_OWNER_VOLUME: &str = "rsctf-worker-owner";
const LABEL_DAEMON_OWNER: &str = "io.rsctf.worker.daemon-owner";
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(30);
const READY_TIMEOUT: Duration = Duration::from_secs(30);

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult<T = ()> = Result<T, TestError>;

struct DockerTestScope {
    docker: Docker,
    worker_id: Uuid,
    state_dir: PathBuf,
    cleaned: bool,
}

impl DockerTestScope {
    async fn prepare(worker_id: Uuid, state_dir: PathBuf) -> TestResult<Self> {
        let docker = Docker::connect_with_local_defaults()?
            .negotiate_version()
            .await?;
        match docker.inspect_volume(DAEMON_OWNER_VOLUME).await {
            Ok(volume) => {
                let owner = volume
                    .labels
                    .get(LABEL_DAEMON_OWNER)
                    .map(String::as_str)
                    .unwrap_or("unknown");
                return Err(test_error(format!(
                    "Docker smoke test requires an unclaimed disposable daemon; {DAEMON_OWNER_VOLUME} is owned by {owner}"
                )));
            }
            Err(error) if is_not_found(&error) => {}
            Err(error) => return Err(Box::new(error)),
        }
        Ok(Self {
            docker,
            worker_id,
            state_dir,
            cleaned: false,
        })
    }

    async fn cleanup(&mut self) -> Result<(), String> {
        let result =
            cleanup_owned_resources(self.docker.clone(), self.worker_id, self.state_dir.clone())
                .await;
        self.cleaned = result.is_ok();
        result
    }
}

impl Drop for DockerTestScope {
    fn drop(&mut self) {
        if self.cleaned {
            return;
        }
        let docker = self.docker.clone();
        let worker_id = self.worker_id;
        let state_dir = self.state_dir.clone();
        let cleanup = std::thread::Builder::new()
            .name("rsctf-worker-test-cleanup".to_string())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| format!("create cleanup runtime: {error}"))?;
                runtime.block_on(async move {
                    tokio::time::timeout(
                        CLEANUP_TIMEOUT,
                        cleanup_owned_resources(docker, worker_id, state_dir),
                    )
                    .await
                    .map_err(|_| "Docker test cleanup timed out".to_string())?
                })
            });
        let outcome = match cleanup {
            Ok(thread) => thread
                .join()
                .unwrap_or_else(|_| Err("Docker test cleanup thread panicked".to_string())),
            Err(error) => Err(format!("spawn Docker test cleanup thread: {error}")),
        };
        if let Err(error) = outcome {
            eprintln!("Docker smoke-test cleanup failed: {error}");
        }
    }
}

async fn cleanup_owned_resources(
    docker: Docker,
    worker_id: Uuid,
    state_dir: PathBuf,
) -> Result<(), String> {
    let mut errors = Vec::new();
    let worker = worker_id.to_string();
    let mut filters = HashMap::new();
    filters.insert(
        "label".to_string(),
        vec![
            format!("{LABEL_MANAGED}=true"),
            format!("{LABEL_WORKER}={worker}"),
        ],
    );
    match docker
        .list_containers(Some(ListContainersOptions {
            all: true,
            filters: filters.clone(),
            ..Default::default()
        }))
        .await
    {
        Ok(containers) => {
            for container in containers {
                let Some(id) = container.id else {
                    errors.push("Docker omitted an owned test container ID".to_string());
                    continue;
                };
                if let Err(error) = docker
                    .remove_container(
                        &id,
                        Some(RemoveContainerOptions {
                            force: true,
                            v: true,
                            ..Default::default()
                        }),
                    )
                    .await
                {
                    if !is_not_found(&error) {
                        errors.push(format!("remove test container {id}: {error}"));
                    }
                }
            }
        }
        Err(error) => errors.push(format!("list owned test containers: {error}")),
    }

    match docker
        .list_networks(Some(ListNetworksOptions { filters }))
        .await
    {
        Ok(networks) => {
            for network in networks {
                let Some(id) = network.id else {
                    errors.push("Docker omitted an owned test network ID".to_string());
                    continue;
                };
                if let Err(error) = docker.remove_network(&id).await {
                    if !is_not_found(&error) {
                        errors.push(format!("remove test network {id}: {error}"));
                    }
                }
            }
        }
        Err(error) => errors.push(format!("list owned test networks: {error}")),
    }

    match docker.inspect_volume(DAEMON_OWNER_VOLUME).await {
        Ok(volume)
            if volume.labels.get(LABEL_DAEMON_OWNER).map(String::as_str)
                == Some(worker.as_str()) =>
        {
            if let Err(error) = docker
                .remove_volume(
                    DAEMON_OWNER_VOLUME,
                    Some(RemoveVolumeOptions { force: false }),
                )
                .await
            {
                if !is_not_found(&error) {
                    errors.push(format!("remove test daemon sentinel: {error}"));
                }
            }
        }
        Ok(_) => errors
            .push("refused to remove a Docker daemon sentinel owned by another worker".to_string()),
        Err(error) if is_not_found(&error) => {}
        Err(error) => errors.push(format!("inspect test daemon sentinel: {error}")),
    }

    if let Err(error) = tokio::fs::remove_dir_all(&state_dir).await {
        if error.kind() != std::io::ErrorKind::NotFound {
            errors.push(format!("remove test state directory: {error}"));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn test_error(message: impl Into<String>) -> TestError {
    Box::new(std::io::Error::other(message.into()))
}

fn require(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(test_error(message))
    }
}

async fn exercise_lifecycle(
    runtime: &DockerRuntime,
    worker_id: Uuid,
    image_id: String,
    fence: WorkloadFence,
) -> TestResult {
    runtime.probe().await?;
    let image = ImageIdentity::WorkerLocal {
        worker_id,
        image_id,
    };
    let resources = ResourceLimits {
        cpu_millis: 100,
        memory_bytes: 64 * 1024 * 1024,
    };
    let service_port = || ServicePort {
        name: "service".to_string(),
        container_port: 8080,
        protocol: PortProtocol::Tcp,
    };
    let spec = ValidatedWorkloadSpec::try_from(WorkloadSpec {
        game_kind: GameKind::Jeopardy,
        platform: Platform {
            operating_system: OperatingSystem::Linux,
            architecture: runtime.platform().architecture,
            windows_build: None,
        },
        services: vec![
            ServiceSpec {
                name: "challenge".to_string(),
                image: image.clone(),
                resources,
                replicas: 2,
                stateless: true,
                environment: BTreeMap::new(),
                ports: vec![service_port()],
            },
            ServiceSpec {
                name: "sidecar".to_string(),
                image,
                resources,
                replicas: 1,
                stateless: true,
                environment: BTreeMap::new(),
                ports: vec![service_port()],
            },
        ],
        primary_endpoint: EndpointRef {
            service: "challenge".to_string(),
            port: "service".to_string(),
        },
        flag_target: Some(FlagTarget {
            service: "challenge".to_string(),
            path: "/tmp/flag".to_string(),
        }),
    })?;
    let spec_hash = spec.spec_hash()?;
    let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
    loop {
        let status = runtime
            .ensure_workload(EnsureWorkload {
                command_id: Uuid::new_v4(),
                fence,
                spec_hash: spec_hash.clone(),
                timeout_ms: READY_TIMEOUT.as_millis() as u64,
                spec: spec.clone(),
            })
            .await?;
        if status.state == ObservedWorkloadState::Ready {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(test_error(format!(
                "workload listeners did not become Ready: {:?}",
                status.replicas
            )));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let challenge_replicas = runtime.matching_container_ids(fence, "challenge").await?;
    require(
        challenge_replicas.len() == 2,
        format!(
            "expected two challenge replicas, got {}",
            challenge_replicas.len()
        ),
    )?;
    let sidecar_replicas = runtime.matching_container_ids(fence, "sidecar").await?;
    require(
        sidecar_replicas.len() == 1,
        format!(
            "expected one sidecar replica, got {}",
            sidecar_replicas.len()
        ),
    )?;

    runtime
        .open_tcp(&TcpProxyRequest {
            fence,
            service: "challenge".to_string(),
            port: "service".to_string(),
            replica: None,
        })
        .await?;
    let inventory = runtime.inventory().await?;
    require(
        inventory.len() == 1 && inventory[0].state == ObservedWorkloadState::Ready,
        format!("expected one Ready inventory item, got {inventory:?}"),
    )?;

    runtime
        .write_flag(WriteFlag {
            command_id: Uuid::new_v4(),
            fence,
            flag_sequence: 1,
            target: FlagTarget {
                service: "challenge".to_string(),
                path: "/tmp/flag".to_string(),
            },
            value: "RSCTF{worker_smoke}".to_string(),
            timeout_ms: 10_000,
        })
        .await?;

    let absent = runtime
        .ensure_absent(EnsureAbsent {
            command_id: Uuid::new_v4(),
            fence,
            spec_hash,
            timeout_ms: READY_TIMEOUT.as_millis() as u64,
        })
        .await?;
    require(
        absent.state == ObservedWorkloadState::Absent,
        format!("expected Absent after teardown, got {:?}", absent.state),
    )?;
    require(
        runtime.inventory().await?.is_empty(),
        "workload remained in inventory after teardown",
    )
}

#[tokio::test]
async fn docker_workload_lifecycle_when_image_is_configured() {
    let Ok(image_id) = std::env::var("RSCTF_WORKER_TEST_IMAGE_ID") else {
        return;
    };
    let worker_id = Uuid::new_v4();
    let state_dir = std::env::temp_dir().join(format!("rsctf-worker-test-{worker_id}"));
    let mut scope = DockerTestScope::prepare(worker_id, state_dir.clone())
        .await
        .unwrap_or_else(|error| panic!("prepare isolated Docker smoke test: {error}"));
    let result = async {
        let runtime = DockerRuntime::connect(
            worker_id,
            None,
            &state_dir,
            RuntimeOptions {
                writable_layer_bytes: 512 * 1024 * 1024,
                minimum_free_bytes: 1024 * 1024 * 1024,
                allow_unbounded_storage: true,
            },
        )
        .await?;
        let fence = WorkloadFence {
            workload_id: Uuid::new_v4(),
            assignment_id: Uuid::new_v4(),
            generation: 1,
        };
        exercise_lifecycle(&runtime, worker_id, image_id, fence).await
    }
    .await;
    let cleanup = scope.cleanup().await;
    match (result, cleanup) {
        (Ok(()), Ok(())) => {}
        (Err(error), Ok(())) => panic!("Docker smoke test failed: {error}"),
        (Ok(()), Err(error)) => panic!("Docker smoke-test cleanup failed: {error}"),
        (Err(test_error), Err(cleanup_error)) => {
            panic!("Docker smoke test failed: {test_error}; cleanup also failed: {cleanup_error}")
        }
    }
}
