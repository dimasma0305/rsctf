use std::fs::File;
use std::io::BufReader;
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{ClientConfig, RootCertStore};
use thiserror::Error;
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;

use crate::config::AgentConfig;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct MtlsConnector {
    config: AgentConfig,
}

impl MtlsConnector {
    pub fn new(config: AgentConfig) -> Self {
        Self { config }
    }

    pub async fn connect_control(&self) -> Result<TlsStream<TcpStream>, TlsConnectorError> {
        self.connect(
            &self.config.control_address,
            rsctf_worker_protocol::CONTROL_ALPN,
        )
        .await
    }

    pub async fn connect_data(&self) -> Result<TlsStream<TcpStream>, TlsConnectorError> {
        self.connect(&self.config.data_address, rsctf_worker_protocol::DATA_ALPN)
            .await
    }

    async fn connect(
        &self,
        address: &str,
        alpn: &[u8],
    ) -> Result<TlsStream<TcpStream>, TlsConnectorError> {
        let mut roots = RootCertStore::empty();
        for certificate in load_certificates(&self.config.ca_path)? {
            roots.add(certificate)?;
        }
        let certificates = load_certificates(&self.config.certificate_path)?;
        if certificates.is_empty() {
            return Err(TlsConnectorError::MissingCertificate);
        }
        let key = load_private_key(&self.config.private_key_path)?;
        let mut client = ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_root_certificates(roots)
            .with_client_auth_cert(certificates, key)?;
        client.alpn_protocols = vec![alpn.to_vec()];
        client.enable_early_data = false;

        let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(address))
            .await
            .map_err(|_| TlsConnectorError::ConnectTimeout)??;
        stream.set_nodelay(true)?;
        let server_name = ServerName::try_from(self.config.server_name.clone())
            .map_err(|_| TlsConnectorError::InvalidServerName)?;
        let stream = tokio::time::timeout(
            TLS_HANDSHAKE_TIMEOUT,
            TlsConnector::from(Arc::new(client)).connect(server_name, stream),
        )
        .await
        .map_err(|_| TlsConnectorError::HandshakeTimeout)??;
        let negotiated = stream.get_ref().1.alpn_protocol();
        if negotiated != Some(alpn) {
            return Err(TlsConnectorError::AlpnMismatch);
        }
        Ok(stream)
    }
}

fn load_certificates(
    path: &std::path::Path,
) -> Result<Vec<CertificateDer<'static>>, TlsConnectorError> {
    let file = File::open(path)?;
    rustls_pemfile::certs(&mut BufReader::new(file))
        .collect::<Result<Vec<_>, _>>()
        .map_err(TlsConnectorError::Io)
}

fn load_private_key(path: &std::path::Path) -> Result<PrivateKeyDer<'static>, TlsConnectorError> {
    let file = File::open(path)?;
    rustls_pemfile::private_key(&mut BufReader::new(file))?
        .ok_or(TlsConnectorError::MissingPrivateKey)
}

#[derive(Debug, Error)]
pub enum TlsConnectorError {
    #[error("TLS identity I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("TLS configuration failed: {0}")]
    Rustls(#[from] rustls::Error),
    #[error("timed out connecting to the worker server")]
    ConnectTimeout,
    #[error("timed out completing the worker TLS handshake")]
    HandshakeTimeout,
    #[error("the certificate file contains no certificates")]
    MissingCertificate,
    #[error("the private-key file contains no supported private key")]
    MissingPrivateKey,
    #[error("the configured TLS server name is invalid")]
    InvalidServerName,
    #[error("the server did not negotiate the required worker ALPN")]
    AlpnMismatch,
}
