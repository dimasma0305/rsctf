use std::io::BufReader;
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::{RootCertStore, ServerConfig};

use super::{build_mtls_server_config, PostgresWorkerAuthority, WorkerServerConfig, WorkerService};
use crate::services::worker_store::WorkerStore;

const LISTEN_ENV: &str = "RSCTF_WORKER_LISTEN";
const CA_CERT_ENV: &str = "RSCTF_WORKER_CA_CERT";
const SERVER_CERT_ENV: &str = "RSCTF_WORKER_SERVER_CERT";
const SERVER_KEY_ENV: &str = "RSCTF_WORKER_SERVER_KEY";

/// A fully validated and pre-bound listener. Binding before HTTP readiness
/// prevents a replica from advertising itself when its worker port or TLS
/// secrets are unusable.
pub struct BoundWorkerPlane {
    pub service: Arc<WorkerService>,
    listener: TcpListener,
    tls: Arc<ServerConfig>,
}

impl BoundWorkerPlane {
    pub fn start(
        self,
        shutdown: watch::Receiver<bool>,
    ) -> tokio::task::JoinHandle<anyhow::Result<()>> {
        tokio::spawn(async move {
            self.service
                .serve(self.listener, self.tls, shutdown)
                .await
                .map_err(anyhow::Error::from)
        })
    }
}

/// Load dedicated worker-plane TLS material and bind its raw TCP listener.
/// With no `RSCTF_WORKER_LISTEN`, the optional plane stays disabled. Partial
/// listener configuration is a startup error rather than a silent downgrade.
pub async fn bind_from_env(store: WorkerStore) -> anyhow::Result<Option<BoundWorkerPlane>> {
    let listen = nonempty_env(LISTEN_ENV);
    let server_cert = nonempty_env(SERVER_CERT_ENV);
    let server_key = nonempty_env(SERVER_KEY_ENV);
    if listen.is_none() && server_cert.is_none() && server_key.is_none() {
        return Ok(None);
    }
    let listen = required(listen, LISTEN_ENV)?;
    let ca_cert = required(nonempty_env(CA_CERT_ENV), CA_CERT_ENV)?;
    let server_cert = required(server_cert, SERVER_CERT_ENV)?;
    let server_key = required(server_key, SERVER_KEY_ENV)?;

    let server_chain = load_certificates(&server_cert)?;
    let private_key = load_private_key(&server_key)?;
    let mut worker_roots = RootCertStore::empty();
    for certificate in load_certificates(&ca_cert)? {
        worker_roots
            .add(certificate)
            .map_err(|error| anyhow::anyhow!("invalid worker CA certificate {ca_cert}: {error}"))?;
    }
    let tls = Arc::new(build_mtls_server_config(
        server_chain,
        private_key,
        worker_roots,
    )?);
    let config = WorkerServerConfig::default();
    let authority = Arc::new(PostgresWorkerAuthority::new(
        store,
        config.registry.heartbeat_lease,
    ));
    let service = Arc::new(WorkerService::new(config, authority)?);
    let listener = TcpListener::bind(&listen)
        .await
        .map_err(|error| anyhow::anyhow!("failed to bind worker listener {listen}: {error}"))?;
    tracing::info!(bind = %listen, "bound trusted worker listener");
    Ok(Some(BoundWorkerPlane {
        service,
        listener,
        tls,
    }))
}

fn load_certificates(path: &str) -> anyhow::Result<Vec<CertificateDer<'static>>> {
    let file = std::fs::File::open(path)
        .map_err(|error| anyhow::anyhow!("failed to open certificate {path}: {error}"))?;
    let mut reader = BufReader::new(file);
    let certificates = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| anyhow::anyhow!("failed to parse certificate {path}: {error}"))?;
    if certificates.is_empty() {
        anyhow::bail!("certificate file {path} contains no certificates");
    }
    Ok(certificates)
}

fn load_private_key(path: &str) -> anyhow::Result<PrivateKeyDer<'static>> {
    let file = std::fs::File::open(path)
        .map_err(|error| anyhow::anyhow!("failed to open private key {path}: {error}"))?;
    let mut reader = BufReader::new(file);
    rustls_pemfile::private_key(&mut reader)
        .map_err(|error| anyhow::anyhow!("failed to parse private key {path}: {error}"))?
        .ok_or_else(|| anyhow::anyhow!("private key file {path} contains no supported key"))
}

fn nonempty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn required(value: Option<String>, name: &str) -> anyhow::Result<String> {
    value
        .ok_or_else(|| anyhow::anyhow!("{name} is required when the worker listener is configured"))
}
