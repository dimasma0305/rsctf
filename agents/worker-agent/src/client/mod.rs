mod control;
mod data;

use std::time::{Duration, Instant};

use rsctf_worker_protocol::{WorkerCapacity, WorkerHello, PROTOCOL_REVISION};
use thiserror::Error;
use uuid::Uuid;

use crate::backoff::Backoff;
use crate::config::{AgentConfig, RunArgs};
use crate::readiness::ReadinessFile;
use crate::runtime::{runtime_for, RuntimeOptions};
use crate::tls::MtlsConnector;

const SESSION_NEGOTIATION_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn run(arguments: RunArgs) -> Result<(), ClientError> {
    if arguments.runtime_concurrency == 0 {
        return Err(ClientError::Configuration(
            "runtime concurrency must be greater than zero".to_string(),
        ));
    }
    if !arguments.accept_host_network_boundary {
        return Err(ClientError::Configuration(
            "worker workloads can address the Docker host gateway; run only in a dedicated, firewalled host/VM and pass --accept-host-network-boundary"
                .to_string(),
        ));
    }
    let maximum_writable_layer_bytes = if cfg!(windows) {
        256 * 1024 * 1024 * 1024
    } else {
        16 * 1024 * 1024 * 1024
    };
    if !(64 * 1024 * 1024..=maximum_writable_layer_bytes).contains(&arguments.writable_layer_bytes)
        || arguments.minimum_free_bytes < 1024 * 1024 * 1024
    {
        return Err(ClientError::Configuration(format!(
            "writable-layer quota must be 64 MiB..{} GiB and minimum free space at least 1 GiB",
            maximum_writable_layer_bytes / (1024 * 1024 * 1024)
        )));
    }
    let state_dir = arguments
        .config
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let readiness = ReadinessFile::new(arguments.ready_file.clone())?;
    if let Some(readiness_dir) = arguments
        .ready_file
        .as_deref()
        .and_then(std::path::Path::parent)
    {
        crate::security::verify_state_dir(readiness_dir).await?;
    }
    readiness.clear().await?;
    crate::security::verify_state_dir(state_dir).await?;
    crate::security::verify_state_file(&arguments.config).await?;
    let _state_lock = crate::security::acquire_state_lock(state_dir)?;
    let config = AgentConfig::load(&arguments.config).await?;
    for path in [
        &config.certificate_path,
        &config.private_key_path,
        &config.ca_path,
    ] {
        crate::security::verify_state_file(path).await?;
    }
    let mut backoff = Backoff::new(Duration::from_secs(1), Duration::from_secs(30));
    let runtime = loop {
        match runtime_for(
            config.worker_id,
            arguments.docker_endpoint.as_deref(),
            state_dir,
            RuntimeOptions {
                writable_layer_bytes: arguments.writable_layer_bytes,
                minimum_free_bytes: arguments.minimum_free_bytes,
                allow_unbounded_storage: arguments.allow_unbounded_storage,
            },
        )
        .await
        {
            Ok(runtime) => break runtime,
            Err(error) if error.code == rsctf_worker_protocol::CommandErrorCode::Unsupported => {
                return Err(error.into())
            }
            Err(error) => {
                let delay = backoff.next_delay();
                tracing::warn!(%error, ?delay, "Docker initialization failed; retrying locally");
                tokio::time::sleep(delay).await;
            }
        }
    };
    let connector = MtlsConnector::new(config.clone());
    backoff.reset();
    let detected_capacity = loop {
        match runtime.probe().await {
            Ok(()) => match runtime.capacity().await {
                Ok(capacity) => break capacity,
                Err(error) => {
                    let delay = backoff.next_delay();
                    tracing::warn!(%error, ?delay, "Docker capacity probe failed; retrying locally");
                    tokio::time::sleep(delay).await;
                }
            },
            Err(error) => {
                let delay = backoff.next_delay();
                tracing::warn!(%error, ?delay, "Docker is unavailable; retrying locally");
                tokio::time::sleep(delay).await;
            }
        }
    };
    backoff.reset();
    let configured_capacity = config.capacity.unwrap_or(detected_capacity);
    let capacity = select_capacity(
        detected_capacity,
        configured_capacity,
        arguments.cpu_millis,
        arguments.memory_bytes,
        arguments.slots,
    )?;
    let mut labels = config.labels.clone();
    for raw_label in arguments.labels {
        let (key, value) = raw_label.split_once('=').ok_or_else(|| {
            ClientError::Configuration(format!("placement label `{raw_label}` must use key=value"))
        })?;
        if key.is_empty()
            || value.is_empty()
            || labels.len() >= 32 && !labels.contains_key(key)
            || key.len() > 63
            || value.len() > 255
        {
            return Err(ClientError::Configuration(
                "placement labels must be nonempty, bounded, and limited to 32 entries".to_string(),
            ));
        }
        labels.insert(key.to_string(), value.to_string());
    }
    let hello = WorkerHello {
        protocol_revision: PROTOCOL_REVISION,
        worker_id: config.worker_id,
        boot_id: Uuid::new_v4(),
        agent_version: env!("CARGO_PKG_VERSION").to_string(),
        platform: runtime.platform(),
        runtime: runtime.descriptor(),
        capabilities: runtime.capabilities(),
        capacity,
        labels,
    };
    let dispatcher =
        control::OperationDispatcher::new(runtime.clone(), arguments.runtime_concurrency);

    loop {
        if let Err(error) = runtime.probe().await {
            let delay = backoff.next_delay();
            tracing::warn!(%error, ?delay, "Docker became unavailable; not advertising worker capacity");
            tokio::time::sleep(delay).await;
            continue;
        }
        let connected_at = Instant::now();
        let result = control::run_session(
            &connector,
            &hello,
            runtime.clone(),
            dispatcher.clone(),
            &readiness,
        )
        .await;
        readiness.clear().await?;
        if connected_at.elapsed() >= Duration::from_secs(60) {
            backoff.reset();
        }
        let delay = backoff.next_delay();
        match result {
            Ok(()) => tracing::warn!(?delay, "worker control session closed"),
            Err(error) => tracing::warn!(%error, ?delay, "worker control session failed"),
        }
        tokio::time::sleep(delay).await;
    }
}

fn select_capacity(
    detected: WorkerCapacity,
    configured: WorkerCapacity,
    cpu_millis: Option<u64>,
    memory_bytes: Option<u64>,
    slots: Option<u32>,
) -> Result<WorkerCapacity, ClientError> {
    let selected = WorkerCapacity {
        cpu_millis: cpu_millis.unwrap_or(configured.cpu_millis),
        memory_bytes: memory_bytes.unwrap_or(configured.memory_bytes),
        slots: slots.unwrap_or(configured.slots),
    };
    if selected.cpu_millis == 0 || selected.memory_bytes == 0 || selected.slots == 0 {
        return Err(ClientError::Configuration(
            "advertised CPU, memory, and slots must all be greater than zero".to_string(),
        ));
    }
    if selected.cpu_millis > detected.cpu_millis
        || selected.memory_bytes > detected.memory_bytes
        || selected.slots > detected.slots
        || selected.slots > rsctf_worker_protocol::MAX_WORKER_SLOTS
    {
        return Err(ClientError::Configuration(format!(
            "advertised capacity must not exceed detected safe capacity (cpu={}m, memory={} bytes, slots={})",
            detected.cpu_millis, detected.memory_bytes, detected.slots
        )));
    }
    Ok(selected)
}

pub fn validate_revision(revision: u16) -> Result<(), ClientError> {
    if revision != PROTOCOL_REVISION {
        return Err(ClientError::Protocol(format!(
            "server selected protocol revision {revision}, agent supports {PROTOCOL_REVISION}"
        )));
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum ClientError {
    #[error(transparent)]
    Config(#[from] crate::config::ConfigError),
    #[error(transparent)]
    Security(#[from] crate::security::SecurityError),
    #[error(transparent)]
    Runtime(#[from] crate::runtime::RuntimeError),
    #[error(transparent)]
    Tls(#[from] crate::tls::TlsConnectorError),
    #[error(transparent)]
    Readiness(#[from] crate::readiness::ReadinessError),
    #[error(transparent)]
    Frame(#[from] rsctf_worker_protocol::FrameError),
    #[error("worker protocol error: {0}")]
    Protocol(String),
    #[error("worker configuration error: {0}")]
    Configuration(String),
    #[error("worker transport failed: {0}")]
    Transport(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    const DETECTED: WorkerCapacity = WorkerCapacity {
        cpu_millis: 8_000,
        memory_bytes: 16 * 1024 * 1024 * 1024,
        slots: 64,
    };

    #[test]
    fn capacity_overrides_can_only_reserve_detected_headroom() {
        let configured = WorkerCapacity {
            cpu_millis: 4_000,
            memory_bytes: 8 * 1024 * 1024 * 1024,
            slots: 32,
        };
        assert_eq!(
            select_capacity(DETECTED, configured, None, None, None).unwrap(),
            configured
        );
        assert_eq!(
            select_capacity(DETECTED, configured, Some(6_000), None, Some(48)).unwrap(),
            WorkerCapacity {
                cpu_millis: 6_000,
                memory_bytes: configured.memory_bytes,
                slots: 48,
            }
        );
    }

    #[test]
    fn capacity_overrides_cannot_overcommit_the_docker_host() {
        for result in [
            select_capacity(DETECTED, DETECTED, Some(8_001), None, None),
            select_capacity(
                DETECTED,
                DETECTED,
                None,
                Some(DETECTED.memory_bytes + 1),
                None,
            ),
            select_capacity(DETECTED, DETECTED, None, None, Some(65)),
        ] {
            assert!(matches!(result, Err(ClientError::Configuration(_))));
        }
    }
}
