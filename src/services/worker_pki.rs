//! Worker certificate issuance for the one-time enrollment endpoint.
//!
//! The worker creates its private key and CSR locally. RSCTF only receives the
//! verified CSR and returns a short-lived client certificate signed by a CA
//! dedicated to the trusted worker plane.

use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use rcgen::{
    CertificateParams, CertificateSigningRequestParams, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose, SerialNumber,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const CERTIFICATE_LIFETIME_DAYS: i64 = 90;
const CLOCK_SKEW_MINUTES: i64 = 5;
const MAX_CSR_BYTES: usize = 64 * 1024;

/// Result returned exactly once after an enrollment token is consumed.
#[derive(Debug)]
pub struct IssuedWorkerCertificate {
    pub certificate_pem: String,
    pub ca_certificate_pem: String,
    pub fingerprint_sha256: [u8; 32],
    pub serial: String,
    pub expires_at: DateTime<Utc>,
}

/// In-memory CA signer. `Debug` is intentionally omitted because it owns the
/// worker CA private key.
pub struct WorkerIssuer {
    issuer: Issuer<'static, KeyPair>,
    ca_certificate_pem: String,
    public_endpoint: String,
    server_name: String,
}

impl WorkerIssuer {
    /// Load worker CA material when all worker-PKI variables are configured.
    /// No variables means the optional worker plane remains disabled; a partial
    /// configuration is rejected instead of silently disabling enrollment.
    pub fn from_env() -> anyhow::Result<Option<Arc<Self>>> {
        let cert_path = nonempty_env("RSCTF_WORKER_CA_CERT");
        let key_path = nonempty_env("RSCTF_WORKER_CA_KEY");
        let public_endpoint = nonempty_env("RSCTF_WORKER_PUBLIC_ENDPOINT");
        if cert_path.is_none() && key_path.is_none() && public_endpoint.is_none() {
            return Ok(None);
        }
        let cert_path = cert_path.ok_or_else(|| {
            anyhow::anyhow!("RSCTF_WORKER_CA_CERT is required when worker PKI is configured")
        })?;
        let key_path = key_path.ok_or_else(|| {
            anyhow::anyhow!("RSCTF_WORKER_CA_KEY is required when worker PKI is configured")
        })?;
        let public_endpoint = public_endpoint.ok_or_else(|| {
            anyhow::anyhow!(
                "RSCTF_WORKER_PUBLIC_ENDPOINT is required when worker PKI is configured"
            )
        })?;
        let configured_server_name = nonempty_env("RSCTF_WORKER_SERVER_NAME");
        Self::from_files_with_server_name(
            &cert_path,
            &key_path,
            public_endpoint,
            configured_server_name,
        )
        .map(Some)
    }

    pub fn from_files(
        cert_path: impl AsRef<Path>,
        key_path: impl AsRef<Path>,
        public_endpoint: String,
    ) -> anyhow::Result<Arc<Self>> {
        Self::from_files_with_server_name(cert_path, key_path, public_endpoint, None)
    }

    fn from_files_with_server_name(
        cert_path: impl AsRef<Path>,
        key_path: impl AsRef<Path>,
        public_endpoint: String,
        configured_server_name: Option<String>,
    ) -> anyhow::Result<Arc<Self>> {
        let ca_certificate_pem = std::fs::read_to_string(cert_path.as_ref()).map_err(|error| {
            anyhow::anyhow!(
                "failed to read worker CA certificate {}: {error}",
                cert_path.as_ref().display()
            )
        })?;
        let key_pem = std::fs::read_to_string(key_path.as_ref()).map_err(|error| {
            anyhow::anyhow!(
                "failed to read worker CA key {}: {error}",
                key_path.as_ref().display()
            )
        })?;
        let key = KeyPair::from_pem(&key_pem)
            .map_err(|error| anyhow::anyhow!("invalid worker CA private key: {error}"))?;
        let issuer = Issuer::from_ca_cert_pem(&ca_certificate_pem, key)
            .map_err(|error| anyhow::anyhow!("invalid worker CA certificate: {error}"))?;
        if public_endpoint.trim().is_empty() || public_endpoint.chars().any(char::is_whitespace) {
            anyhow::bail!("RSCTF_WORKER_PUBLIC_ENDPOINT must be a non-empty host:port");
        }
        let server_name = configured_server_name
            .unwrap_or_else(|| endpoint_server_name(&public_endpoint).to_string());
        if server_name.is_empty() || server_name.chars().any(char::is_whitespace) {
            anyhow::bail!("RSCTF_WORKER_SERVER_NAME must be a valid DNS name or IP address");
        }
        Ok(Arc::new(Self {
            issuer,
            ca_certificate_pem,
            public_endpoint,
            server_name,
        }))
    }

    pub fn public_endpoint(&self) -> &str {
        &self.public_endpoint
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// Verify the CSR proof-of-possession, replace every caller-controlled
    /// extension with RSCTF's client-only profile, and sign it.
    pub fn issue(&self, worker_id: Uuid, csr_pem: &str) -> anyhow::Result<IssuedWorkerCertificate> {
        if csr_pem.len() > MAX_CSR_BYTES {
            anyhow::bail!("worker CSR exceeds {MAX_CSR_BYTES} bytes");
        }
        let csr = CertificateSigningRequestParams::from_pem(csr_pem)
            .map_err(|error| anyhow::anyhow!("invalid worker CSR: {error}"))?;

        let now = Utc::now();
        let expires_at = now + Duration::days(CERTIFICATE_LIFETIME_DAYS);
        let dns_name = format!("worker-{worker_id}.workers.rsctf.invalid");
        let mut params = CertificateParams::new(vec![dns_name])
            .map_err(|error| anyhow::anyhow!("invalid worker certificate identity: {error}"))?;
        let mut distinguished_name = DistinguishedName::new();
        distinguished_name.push(DnType::CommonName, format!("rsctf-worker:{worker_id}"));
        params.distinguished_name = distinguished_name;
        params.is_ca = IsCa::NoCa;
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        let serial_bytes = Uuid::new_v4().as_bytes().to_vec();
        params.serial_number = Some(SerialNumber::from(serial_bytes.clone()));
        params.not_before = chrono_to_time(now - Duration::minutes(CLOCK_SKEW_MINUTES))?;
        params.not_after = chrono_to_time(expires_at)?;

        let certificate = params
            .signed_by(&csr.public_key, &self.issuer)
            .map_err(|error| anyhow::anyhow!("failed to sign worker certificate: {error}"))?;
        let fingerprint_sha256: [u8; 32] = Sha256::digest(certificate.der().as_ref()).into();
        Ok(IssuedWorkerCertificate {
            certificate_pem: certificate.pem(),
            ca_certificate_pem: self.ca_certificate_pem.clone(),
            fingerprint_sha256,
            serial: hex::encode(serial_bytes),
            expires_at,
        })
    }
}

fn nonempty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn endpoint_server_name(endpoint: &str) -> &str {
    let endpoint = endpoint.trim();
    if let Some(rest) = endpoint.strip_prefix('[') {
        return rest.split_once(']').map(|(host, _)| host).unwrap_or(rest);
    }
    endpoint
        .rsplit_once(':')
        .map(|(host, _)| host)
        .unwrap_or(endpoint)
}

fn chrono_to_time(value: DateTime<Utc>) -> anyhow::Result<time::OffsetDateTime> {
    time::OffsetDateTime::from_unix_timestamp(value.timestamp())
        .map_err(|error| anyhow::anyhow!("certificate time is out of range: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::{BasicConstraints, CertifiedIssuer};

    fn issuer() -> (Arc<WorkerIssuer>, String) {
        let key = KeyPair::generate().expect("CA key");
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("CA params");
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        let ca = CertifiedIssuer::self_signed(params, key).expect("CA certificate");
        let ca_pem = ca.pem();
        let ca_key = ca.key().serialize_pem();
        let signing_key = KeyPair::from_pem(&ca_key).expect("reload CA key");
        let parsed = Issuer::from_ca_cert_pem(&ca_pem, signing_key).expect("reload CA");
        (
            Arc::new(WorkerIssuer {
                issuer: parsed,
                ca_certificate_pem: ca_pem,
                public_endpoint: "agents.example.test:443".into(),
                server_name: "agents.example.test".into(),
            }),
            ca_key,
        )
    }

    #[test]
    fn issues_client_only_certificate_from_local_csr() {
        let (issuer, _) = issuer();
        let worker_key = KeyPair::generate().expect("worker key");
        let params = CertificateParams::new(Vec::<String>::new()).expect("worker params");
        let csr = params
            .serialize_request(&worker_key)
            .expect("CSR")
            .pem()
            .expect("PEM");
        let issued = issuer
            .issue(Uuid::new_v4(), &csr)
            .expect("issue certificate");
        assert!(issued.certificate_pem.contains("BEGIN CERTIFICATE"));
        assert_eq!(issued.fingerprint_sha256.len(), 32);
        assert_eq!(issued.serial.len(), 32);
        assert_eq!(issued.ca_certificate_pem, issuer.ca_certificate_pem);
    }

    #[test]
    fn rejects_oversized_or_invalid_csr() {
        let (issuer, _) = issuer();
        assert!(issuer.issue(Uuid::new_v4(), "not a csr").is_err());
        assert!(issuer
            .issue(Uuid::new_v4(), &"x".repeat(MAX_CSR_BYTES + 1))
            .is_err());
    }

    #[test]
    fn derives_tls_server_name_from_endpoint() {
        assert_eq!(endpoint_server_name("agents.example:443"), "agents.example");
        assert_eq!(endpoint_server_name("[2001:db8::1]:9443"), "2001:db8::1");
    }
}
