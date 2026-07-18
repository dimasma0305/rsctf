use std::path::{Path, PathBuf};

use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair};
use rsctf_worker_protocol::{EnrollmentRequest, EnrollmentResponse};
use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;
use tokio::io::AsyncReadExt;

use crate::config::{AgentConfig, EnrollArgs};

const ENROLLMENT_PATH: &str = "/api/workers/enroll";

pub async fn run(arguments: EnrollArgs) -> Result<(), EnrollmentError> {
    let server_url = enrollment_url(&arguments.server_url, arguments.allow_insecure_enrollment)?;
    let token = read_token(&arguments).await?;
    if token.expose_secret().is_empty() {
        return Err(EnrollmentError::InvalidResponse(
            "enrollment token must not be empty".to_string(),
        ));
    }
    crate::security::prepare_state_dir(
        &arguments.state_dir,
        arguments.windows_service_account.as_deref(),
        arguments.unix_service_uid,
    )
    .await?;

    let (private_key, csr_pem) = generate_csr()?;
    let request = EnrollmentRequest {
        token: token.expose_secret().to_owned(),
        csr_pem,
    };
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let response = client
        .post(server_url)
        .json(&request)
        .send()
        .await?
        .error_for_status()?
        .json::<EnrollmentResponse>()
        .await?;
    validate_response(&response)?;

    let key_path = arguments.state_dir.join("worker-key.pem");
    let cert_path = arguments.state_dir.join("worker-cert.pem");
    let ca_path = arguments.state_dir.join("worker-ca.pem");
    let config_path = arguments.state_dir.join("worker.json");

    write_private_key(&key_path, private_key.as_bytes()).await?;
    write_new_file(&cert_path, response.certificate_pem.as_bytes()).await?;
    write_new_file(&ca_path, response.ca_pem.as_bytes()).await?;
    let config = AgentConfig {
        worker_id: response.worker_id,
        control_address: response.control_address,
        data_address: response.data_address,
        server_name: response.server_name,
        certificate_path: relative_file(&cert_path),
        private_key_path: relative_file(&key_path),
        ca_path: relative_file(&ca_path),
        capacity: None,
        labels: Default::default(),
    };
    let config_json = serde_json::to_vec_pretty(&config)?;
    write_new_file(&config_path, &config_json).await?;
    for path in [&key_path, &cert_path, &ca_path, &config_path] {
        crate::security::transfer_state_file(path, arguments.unix_service_uid)?;
    }
    tracing::info!(
        worker_id = %config.worker_id,
        config = %config_path.display(),
        "worker enrollment completed"
    );
    Ok(())
}

async fn read_token(arguments: &EnrollArgs) -> Result<SecretString, EnrollmentError> {
    let sources = usize::from(arguments.token.is_some())
        + usize::from(arguments.token_file.is_some())
        + usize::from(arguments.token_stdin);
    if sources != 1 {
        return Err(EnrollmentError::InvalidResponse(
            "provide exactly one of --token, --token-file, or --token-stdin".to_string(),
        ));
    }
    let value = if let Some(token) = &arguments.token {
        token.clone()
    } else if let Some(path) = &arguments.token_file {
        tokio::fs::read_to_string(path).await?
    } else {
        let mut value = String::new();
        tokio::io::stdin().read_to_string(&mut value).await?;
        value
    };
    Ok(SecretString::from(
        value.trim_end_matches(['\r', '\n']).to_string(),
    ))
}

fn enrollment_url(value: &str, allow_insecure: bool) -> Result<reqwest::Url, EnrollmentError> {
    let mut url = reqwest::Url::parse(value).map_err(|error| {
        EnrollmentError::InvalidResponse(format!("invalid server URL: {error}"))
    })?;
    if url.scheme() != "https" && !(allow_insecure && url.scheme() == "http") {
        return Err(EnrollmentError::InvalidResponse(
            "enrollment requires HTTPS (or explicit --allow-insecure-enrollment for local tests)"
                .to_string(),
        ));
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(EnrollmentError::InvalidResponse(
            "server URL must not contain credentials, a query, or a fragment".to_string(),
        ));
    }
    url.set_path(ENROLLMENT_PATH);
    Ok(url)
}

fn generate_csr() -> Result<(String, String), EnrollmentError> {
    let key_pair = KeyPair::generate()?;
    let mut params = CertificateParams::default();
    let mut distinguished_name = DistinguishedName::new();
    distinguished_name.push(DnType::CommonName, "RSCTF worker enrollment");
    params.distinguished_name = distinguished_name;
    let csr = params.serialize_request(&key_pair)?;
    Ok((key_pair.serialize_pem(), csr.pem()?))
}

fn validate_response(response: &EnrollmentResponse) -> Result<(), EnrollmentError> {
    for (field, value) in [
        ("controlAddress", response.control_address.as_str()),
        ("dataAddress", response.data_address.as_str()),
        ("serverName", response.server_name.as_str()),
        ("certificatePem", response.certificate_pem.as_str()),
        ("caPem", response.ca_pem.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(EnrollmentError::InvalidResponse(format!(
                "server returned an empty {field}"
            )));
        }
    }
    Ok(())
}

fn relative_file(path: &Path) -> PathBuf {
    path.file_name()
        .map(PathBuf::from)
        .unwrap_or_else(|| path.to_owned())
}

async fn write_new_file(path: &Path, contents: &[u8]) -> Result<(), std::io::Error> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let mut file = options.open(path).await?;
    tokio::io::AsyncWriteExt::write_all(&mut file, contents).await?;
    tokio::io::AsyncWriteExt::flush(&mut file).await
}

#[cfg(unix)]
async fn write_private_key(path: &Path, contents: &[u8]) -> Result<(), std::io::Error> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    let mut file = options.open(path).await?;
    tokio::io::AsyncWriteExt::write_all(&mut file, contents).await?;
    tokio::io::AsyncWriteExt::flush(&mut file).await
}

#[cfg(not(unix))]
async fn write_private_key(path: &Path, contents: &[u8]) -> Result<(), std::io::Error> {
    // Windows inherits the state directory ACL. Installations should grant access
    // only to the dedicated worker service account.
    write_new_file(path, contents).await
}

#[derive(Debug, Error)]
pub enum EnrollmentError {
    #[error("enrollment HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("enrollment identity generation failed: {0}")]
    Rcgen(#[from] rcgen::Error),
    #[error("enrollment state I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("enrollment configuration encoding failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Security(#[from] crate::security::SecurityError),
    #[error("invalid enrollment response: {0}")]
    InvalidResponse(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enrollment_requires_https_by_default() {
        let url = enrollment_url("https://ctf.example/base", false).unwrap();
        assert_eq!(url.as_str(), "https://ctf.example/api/workers/enroll");
        assert!(enrollment_url("http://ctf.example", false).is_err());
        assert!(enrollment_url("http://127.0.0.1:8080", true).is_ok());
        assert!(enrollment_url("https://user@ctf.example", false).is_err());
    }
}
