use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Parser)]
#[command(name = "rsctf-worker-agent", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Connect this machine to RSCTF using an existing identity.
    Run(RunArgs),
    /// Exchange a one-time enrollment token for a locally generated mTLS identity.
    Enroll(EnrollArgs),
    /// Validate Docker compatibility without enrolling or changing daemon state.
    Doctor(DoctorArgs),
}

#[derive(Clone, Args)]
pub struct DoctorArgs {
    /// Override the Docker endpoint (`local` or a Unix socket in v1).
    #[arg(long, env = "RSCTF_WORKER_DOCKER_ENDPOINT")]
    pub docker_endpoint: Option<String>,
    /// Development-only escape hatch for storage drivers without layer quotas.
    #[arg(
        long,
        env = "RSCTF_WORKER_ALLOW_UNBOUNDED_STORAGE",
        default_value_t = false
    )]
    pub allow_unbounded_storage: bool,
}

#[derive(Clone, Args)]
pub struct RunArgs {
    /// Agent configuration written by `enroll`.
    #[arg(long, env = "RSCTF_WORKER_CONFIG", default_value = "worker.json")]
    pub config: PathBuf,
    /// Confirm this agent runs inside a dedicated, firewalled worker host/VM.
    /// Docker workload networks can address their host-side gateway, so this
    /// boundary must not contain unrelated services or secrets.
    #[arg(
        long,
        env = "RSCTF_WORKER_ACCEPT_HOST_NETWORK_BOUNDARY",
        default_value_t = false
    )]
    pub accept_host_network_boundary: bool,
    /// Override the Docker endpoint (`local` or a Unix socket in v1).
    #[arg(long, env = "RSCTF_WORKER_DOCKER_ENDPOINT")]
    pub docker_endpoint: Option<String>,
    /// Maximum Docker reconciliation operations allowed concurrently.
    #[arg(long, env = "RSCTF_WORKER_RUNTIME_CONCURRENCY", default_value_t = 8)]
    pub runtime_concurrency: usize,
    /// Maximum writable container layer in bytes. Requires a quota-capable
    /// Docker storage driver (overlay2 on XFS with project quotas or windowsfilter).
    #[arg(
        long,
        env = "RSCTF_WORKER_WRITABLE_LAYER_BYTES",
        default_value_t = 536_870_912
    )]
    pub writable_layer_bytes: u64,
    /// Stop advertising and stop managed containers below this free-space floor.
    #[arg(
        long,
        env = "RSCTF_WORKER_MINIMUM_FREE_BYTES",
        default_value_t = 5_368_709_120
    )]
    pub minimum_free_bytes: u64,
    /// Development-only escape hatch for storage drivers without layer quotas.
    /// The free-space watchdog remains active, but hostile workloads are unsafe.
    #[arg(
        long,
        env = "RSCTF_WORKER_ALLOW_UNBOUNDED_STORAGE",
        default_value_t = false
    )]
    pub allow_unbounded_storage: bool,
    /// Override advertised CPU capacity in millicores.
    #[arg(long, env = "RSCTF_WORKER_CPU_MILLIS")]
    pub cpu_millis: Option<u64>,
    /// Override advertised memory capacity in bytes.
    #[arg(long, env = "RSCTF_WORKER_MEMORY_BYTES")]
    pub memory_bytes: Option<u64>,
    /// Override the number of workload slots.
    #[arg(long, env = "RSCTF_WORKER_SLOTS")]
    pub slots: Option<u32>,
    /// Placement label in `key=value` form. May be repeated.
    #[arg(long = "label", env = "RSCTF_WORKER_LABELS", value_delimiter = ',')]
    pub labels: Vec<String>,
}

#[derive(Args)]
pub struct EnrollArgs {
    /// Public HTTPS base URL of the RSCTF deployment.
    #[arg(long, env = "RSCTF_WORKER_SERVER_URL")]
    pub server_url: String,
    /// One-time token. Prefer --token-stdin/--token-file; command-line values
    /// may be visible to other local processes.
    #[arg(
        long,
        env = "RSCTF_WORKER_ENROLLMENT_TOKEN",
        hide_env_values = true,
        conflicts_with_all = ["token_file", "token_stdin"]
    )]
    pub token: Option<String>,
    /// Read the one-time enrollment token from this file.
    #[arg(long, conflicts_with = "token_stdin")]
    pub token_file: Option<PathBuf>,
    /// Read the one-time enrollment token from standard input.
    #[arg(long, default_value_t = false)]
    pub token_stdin: bool,
    /// Permit bootstrap over plain HTTP. Use only on a trusted local test link.
    #[arg(
        long,
        env = "RSCTF_WORKER_ALLOW_INSECURE_ENROLLMENT",
        default_value_t = false
    )]
    pub allow_insecure_enrollment: bool,
    /// Directory in which the new private identity is stored.
    #[arg(long, env = "RSCTF_WORKER_STATE_DIR", default_value = ".")]
    pub state_dir: PathBuf,
    /// Windows account that will run the service and exclusively own state.
    /// Defaults to the account performing enrollment.
    #[arg(long)]
    pub windows_service_account: Option<String>,
    /// Unix UID that will run the service. Root enrollment transfers ownership
    /// of the new state directory to this UID.
    #[arg(long, env = "RSCTF_WORKER_UNIX_SERVICE_UID")]
    pub unix_service_uid: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConfig {
    pub worker_id: Uuid,
    pub control_address: String,
    pub data_address: String,
    pub server_name: String,
    pub certificate_path: PathBuf,
    pub private_key_path: PathBuf,
    pub ca_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity: Option<rsctf_worker_protocol::WorkerCapacity>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

impl AgentConfig {
    pub async fn load(path: &Path) -> Result<Self, ConfigError> {
        let bytes = tokio::fs::read(path).await?;
        let mut config: Self = serde_json::from_slice(&bytes)?;
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        config.certificate_path = resolve_identity(parent, config.certificate_path)?;
        config.private_key_path = resolve_identity(parent, config.private_key_path)?;
        config.ca_path = resolve_identity(parent, config.ca_path)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        for (field, value) in [
            ("controlAddress", self.control_address.as_str()),
            ("dataAddress", self.data_address.as_str()),
            ("serverName", self.server_name.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(ConfigError::Invalid(format!("{field} must not be empty")));
            }
        }
        Ok(())
    }
}

fn resolve_identity(parent: &Path, path: PathBuf) -> Result<PathBuf, ConfigError> {
    let mut components = path.components();
    let Some(std::path::Component::Normal(filename)) = components.next() else {
        return Err(ConfigError::Invalid(
            "identity paths must name a regular file inside the state directory".to_string(),
        ));
    };
    if components.next().is_some() {
        return Err(ConfigError::Invalid(
            "identity paths must name a regular file inside the state directory".to_string(),
        ));
    }
    Ok(parent.join(filename))
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("configuration I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("configuration JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("configuration is invalid: {0}")]
    Invalid(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_server_name() {
        let config = AgentConfig {
            worker_id: Uuid::new_v4(),
            control_address: "example:443".to_string(),
            data_address: "example:443".to_string(),
            server_name: String::new(),
            certificate_path: "cert.pem".into(),
            private_key_path: "key.pem".into(),
            ca_path: "ca.pem".into(),
            capacity: None,
            labels: BTreeMap::new(),
        };
        assert!(matches!(config.validate(), Err(ConfigError::Invalid(_))));
    }
}
