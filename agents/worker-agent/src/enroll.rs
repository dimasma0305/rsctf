use std::path::{Path, PathBuf};
use std::time::Duration;

use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair};
use rsctf_worker_protocol::{EnrollmentRequest, EnrollmentResponse};
use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;
use tokio::io::AsyncReadExt;

use crate::config::{AgentConfig, EnrollArgs};

const ENROLLMENT_PATH: &str = "/api/workers/enroll";
const MAX_ENROLLMENT_TOKEN_BYTES: usize = 4 * 1024;
const MAX_ENROLLMENT_RESPONSE_BYTES: usize = 1024 * 1024;
const ENROLLMENT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const ENROLLMENT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

pub async fn run(arguments: EnrollArgs) -> Result<(), EnrollmentError> {
    let server_url = enrollment_url(&arguments.server_url, arguments.allow_insecure_enrollment)?;
    crate::security::prepare_state_dir(
        &arguments.state_dir,
        arguments.windows_service_account.as_deref(),
        arguments.unix_service_uid,
    )
    .await?;

    let key_path = arguments.state_dir.join("worker-key.pem");
    let cert_path = arguments.state_dir.join("worker-cert.pem");
    let ca_path = arguments.state_dir.join("worker-ca.pem");
    let config_path = arguments.state_dir.join("worker.json");
    let identity_paths = [
        key_path.as_path(),
        cert_path.as_path(),
        ca_path.as_path(),
        config_path.as_path(),
    ];
    require_new_identity(&identity_paths).await?;

    let token = read_token(&arguments).await?;
    if token.expose_secret().is_empty() {
        return Err(EnrollmentError::InvalidResponse(
            "enrollment token must not be empty".to_string(),
        ));
    }

    let (private_key, csr_pem) = generate_csr()?;
    let request = EnrollmentRequest {
        token: token.expose_secret().to_owned(),
        csr_pem,
    };
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(ENROLLMENT_CONNECT_TIMEOUT)
        .timeout(ENROLLMENT_REQUEST_TIMEOUT)
        .build()?;
    let response = client
        .post(server_url)
        .json(&request)
        .send()
        .await?
        .error_for_status()?;
    let response = decode_enrollment_response(response).await?;
    validate_response(&response)?;
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
    let public_files = [
        (&cert_path as &Path, response.certificate_pem.as_bytes()),
        (&ca_path as &Path, response.ca_pem.as_bytes()),
        (&config_path as &Path, config_json.as_slice()),
    ];
    persist_identity(
        &key_path,
        private_key.as_bytes(),
        &public_files,
        arguments.unix_service_uid,
    )
    .await?;
    tracing::info!(
        worker_id = %config.worker_id,
        config = %config_path.display(),
        "worker enrollment completed"
    );
    Ok(())
}

async fn decode_enrollment_response(
    mut response: reqwest::Response,
) -> Result<EnrollmentResponse, EnrollmentError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_ENROLLMENT_RESPONSE_BYTES as u64)
    {
        return Err(EnrollmentError::InvalidResponse(
            "enrollment response exceeds the size limit".to_string(),
        ));
    }
    let mut body = Vec::with_capacity(
        response
            .content_length()
            .unwrap_or_default()
            .min(MAX_ENROLLMENT_RESPONSE_BYTES as u64) as usize,
    );
    while let Some(chunk) = response.chunk().await? {
        if body.len().saturating_add(chunk.len()) > MAX_ENROLLMENT_RESPONSE_BYTES {
            return Err(EnrollmentError::InvalidResponse(
                "enrollment response exceeds the size limit".to_string(),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&body)?)
}

async fn require_new_identity(paths: &[&Path]) -> Result<(), EnrollmentError> {
    for path in paths {
        match tokio::fs::symlink_metadata(path).await {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
            Ok(_) => {
                return Err(EnrollmentError::InvalidResponse(format!(
                    "worker identity already exists at {}; revoke it and remove the state deliberately before re-enrolling",
                    path.display()
                )))
            }
        }
    }
    Ok(())
}

async fn persist_identity(
    key_path: &Path,
    private_key: &[u8],
    public_files: &[(&Path, &[u8])],
    unix_service_uid: Option<u32>,
) -> Result<(), EnrollmentError> {
    let mut created = Vec::with_capacity(4);
    let result = async {
        write_private_key(key_path, private_key).await?;
        created.push(key_path);
        crate::security::transfer_state_file(key_path, unix_service_uid)?;

        for &(path, contents) in public_files {
            write_new_file(path, contents).await?;
            created.push(path);
            crate::security::transfer_state_file(path, unix_service_uid)?;
        }
        Ok::<(), EnrollmentError>(())
    }
    .await;

    if let Err(error) = result {
        let original = error.to_string();
        for path in created.into_iter().rev() {
            if let Err(cleanup_error) = tokio::fs::remove_file(path).await {
                return Err(EnrollmentError::Persistence(format!(
                    "{original}; additionally could not remove partial state {}: {cleanup_error}",
                    path.display()
                )));
            }
        }
        return Err(error);
    }
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
        let mut value = String::new();
        tokio::fs::File::open(path)
            .await?
            .take(MAX_ENROLLMENT_TOKEN_BYTES as u64 + 1)
            .read_to_string(&mut value)
            .await?;
        value
    } else {
        let mut value = String::new();
        tokio::io::stdin()
            .take(MAX_ENROLLMENT_TOKEN_BYTES as u64 + 1)
            .read_to_string(&mut value)
            .await?;
        value
    };
    if value.len() > MAX_ENROLLMENT_TOKEN_BYTES {
        return Err(EnrollmentError::InvalidResponse(
            "enrollment token exceeds the size limit".to_string(),
        ));
    }
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
    let result = async {
        tokio::io::AsyncWriteExt::write_all(&mut file, contents).await?;
        tokio::io::AsyncWriteExt::flush(&mut file).await?;
        file.sync_all().await
    }
    .await;
    drop(file);
    if let Err(error) = result {
        return match tokio::fs::remove_file(path).await {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(std::io::Error::other(format!(
                "{error}; additionally could not remove partial state {}: {cleanup_error}",
                path.display()
            ))),
        };
    }
    Ok(())
}

async fn write_private_key(path: &Path, contents: &[u8]) -> Result<(), std::io::Error> {
    // All identity files are mode 0600 on Unix and inherit the protected state
    // directory ACL on Windows.
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
    #[error("enrollment state persistence failed: {0}")]
    Persistence(String),
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

    #[tokio::test]
    async fn enrollment_response_rejects_oversized_content_length() {
        use tokio::io::AsyncWriteExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 1048577\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
        });
        let response = reqwest::Client::new()
            .get(format!("http://{address}"))
            .send()
            .await
            .unwrap();
        assert!(matches!(
            decode_enrollment_response(response).await,
            Err(EnrollmentError::InvalidResponse(_))
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn identity_preflight_rejects_existing_state_before_enrollment() {
        let directory = std::env::temp_dir().join(format!("rsctf-enroll-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir(&directory).await.unwrap();
        let key = directory.join("worker-key.pem");
        let cert = directory.join("worker-cert.pem");
        require_new_identity(&[key.as_path(), cert.as_path()])
            .await
            .unwrap();
        tokio::fs::write(&cert, b"existing").await.unwrap();
        assert!(require_new_identity(&[key.as_path(), cert.as_path()])
            .await
            .is_err());
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn failed_identity_persistence_removes_only_files_it_created() {
        let directory = std::env::temp_dir().join(format!("rsctf-enroll-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir(&directory).await.unwrap();
        let key = directory.join("worker-key.pem");
        let cert = directory.join("worker-cert.pem");
        tokio::fs::write(&cert, b"existing").await.unwrap();

        let result = persist_identity(
            &key,
            b"private",
            &[(cert.as_path(), b"replacement".as_slice())],
            None,
        )
        .await;
        assert!(result.is_err());
        assert!(!key.exists());
        assert_eq!(tokio::fs::read(&cert).await.unwrap(), b"existing");
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }
}
