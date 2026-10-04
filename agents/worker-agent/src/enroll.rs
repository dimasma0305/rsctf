use std::path::{Path, PathBuf};
use std::time::Duration;

use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair};
use rsctf_worker_protocol::{EnrollmentRequest, EnrollmentResponse};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::AsyncReadExt;

use crate::config::{AgentConfig, EnrollArgs};

const ENROLLMENT_PATH: &str = "/api/workers/enroll";
const MAX_ENROLLMENT_TOKEN_BYTES: usize = 4 * 1024;
const MAX_ENROLLMENT_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_PENDING_ENROLLMENT_BYTES: usize = 128 * 1024;
const MAX_PENDING_CSR_BYTES: usize = 64 * 1024;
const ENROLLMENT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const ENROLLMENT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const ENROLLMENT_RETRY_ATTEMPTS: usize = 5;
const ENROLLMENT_RETRY_MAX: Duration = Duration::from_secs(10);

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
    let pending_path = arguments.state_dir.join("worker-enrollment-pending.json");
    let identity_paths = [
        key_path.as_path(),
        cert_path.as_path(),
        ca_path.as_path(),
        config_path.as_path(),
    ];
    if completed_identity_matches_pending(
        &key_path,
        &cert_path,
        &ca_path,
        &config_path,
        &pending_path,
    )
    .await?
    {
        // Re-establish directory durability before deleting the only recovery
        // record; this also closes a crash after the final create-new write.
        sync_parent_directory(&config_path).await?;
        cleanup_completed_pending(&pending_path).await;
        tracing::info!(
            config = %config_path.display(),
            "worker enrollment was already completed; recovered pending cleanup"
        );
        return Ok(());
    }
    require_new_identity(&identity_paths).await?;

    let token = read_token(&arguments).await?;
    if token.expose_secret().is_empty() {
        return Err(EnrollmentError::InvalidResponse(
            "enrollment token must not be empty".to_string(),
        ));
    }

    let pending = load_or_create_pending(&pending_path, arguments.unix_service_uid).await?;
    let request = EnrollmentRequest {
        operation_id: pending.operation_id,
        token: token.expose_secret().to_owned(),
        csr_pem: pending.csr_pem.clone(),
    };
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(ENROLLMENT_CONNECT_TIMEOUT)
        .timeout(ENROLLMENT_REQUEST_TIMEOUT)
        .build()?;
    let response = send_enrollment(&client, server_url, &request).await?;
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
        pending.private_key.as_bytes(),
        &public_files,
        arguments.unix_service_uid,
    )
    .await?;
    // Every identity file is already create-new, flushed, and parent-synced.
    // Pending cleanup cannot invalidate the one-use server exchange, so retain
    // a verifiable recovery record rather than reporting a failed enrollment.
    cleanup_completed_pending(&pending_path).await;
    tracing::info!(
        worker_id = %config.worker_id,
        config = %config_path.display(),
        "worker enrollment completed"
    );
    Ok(())
}

async fn send_enrollment(
    client: &reqwest::Client,
    server_url: reqwest::Url,
    request: &EnrollmentRequest,
) -> Result<reqwest::Response, EnrollmentError> {
    let mut backoff = Duration::from_millis(250);
    for attempt in 0..ENROLLMENT_RETRY_ATTEMPTS {
        match client.post(server_url.clone()).json(request).send().await {
            Ok(response) if response.status().is_success() => return Ok(response),
            Ok(response) => {
                let status = response.status();
                let retry_after = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok())
                    .map(Duration::from_secs)
                    .map(|delay| delay.min(ENROLLMENT_RETRY_MAX));
                if !transient_status(status) || attempt + 1 == ENROLLMENT_RETRY_ATTEMPTS {
                    return response.error_for_status().map_err(EnrollmentError::from);
                }
                tokio::time::sleep(
                    retry_after.unwrap_or_else(|| {
                        enrollment_jitter(request.operation_id, attempt, backoff)
                    }),
                )
                .await;
            }
            Err(error) => {
                if attempt + 1 == ENROLLMENT_RETRY_ATTEMPTS {
                    return Err(error.into());
                }
                tokio::time::sleep(enrollment_jitter(request.operation_id, attempt, backoff)).await;
            }
        }
        backoff = backoff.saturating_mul(2).min(ENROLLMENT_RETRY_MAX);
    }
    unreachable!("enrollment retry loop always returns on its final attempt")
}

fn transient_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

fn enrollment_jitter(operation_id: uuid::Uuid, attempt: usize, ceiling: Duration) -> Duration {
    let mut first = [0_u8; 8];
    first.copy_from_slice(&operation_id.as_bytes()[..8]);
    let mixed = u64::from_be_bytes(first) ^ (attempt as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    let ceiling_millis = u64::try_from(ceiling.as_millis()).unwrap_or(u64::MAX);
    Duration::from_millis(mixed % ceiling_millis.saturating_add(1))
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PendingEnrollment {
    operation_id: uuid::Uuid,
    private_key: String,
    csr_pem: String,
}

async fn load_or_create_pending(
    path: &Path,
    unix_service_uid: Option<u32>,
) -> Result<PendingEnrollment, EnrollmentError> {
    if let Some(pending) = load_existing_pending(path).await? {
        return Ok(pending);
    }
    let (private_key, csr_pem) = generate_csr()?;
    let pending = PendingEnrollment {
        operation_id: uuid::Uuid::new_v4(),
        private_key,
        csr_pem,
    };
    let encoded = serde_json::to_vec(&pending)?;
    write_new_file(path, &encoded).await?;
    crate::security::transfer_state_file(path, unix_service_uid)?;
    Ok(pending)
}

async fn load_existing_pending(path: &Path) -> Result<Option<PendingEnrollment>, EnrollmentError> {
    let metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_file() || metadata.len() > MAX_PENDING_ENROLLMENT_BYTES as u64 {
        return Err(EnrollmentError::InvalidResponse(
            "pending enrollment state is not a bounded regular file".to_string(),
        ));
    }
    let mut bytes =
        Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(MAX_PENDING_ENROLLMENT_BYTES));
    tokio::fs::File::open(path)
        .await?
        .take(MAX_PENDING_ENROLLMENT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > MAX_PENDING_ENROLLMENT_BYTES {
        return Err(EnrollmentError::InvalidResponse(
            "pending enrollment state exceeds the size limit".to_string(),
        ));
    }
    let pending: PendingEnrollment = serde_json::from_slice(&bytes)?;
    if pending.operation_id.is_nil()
        || pending.private_key.is_empty()
        || pending.csr_pem.is_empty()
        || pending.csr_pem.len() > MAX_PENDING_CSR_BYTES
    {
        return Err(EnrollmentError::InvalidResponse(
            "pending enrollment state is invalid".to_string(),
        ));
    }
    Ok(Some(pending))
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

async fn completed_identity_matches_pending(
    key_path: &Path,
    cert_path: &Path,
    ca_path: &Path,
    config_path: &Path,
    pending_path: &Path,
) -> Result<bool, EnrollmentError> {
    let mut lengths = [0_u64; 4];
    for (index, path) in [key_path, cert_path, ca_path, config_path]
        .into_iter()
        .enumerate()
    {
        match tokio::fs::symlink_metadata(path).await {
            Ok(metadata) if metadata.file_type().is_file() => lengths[index] = metadata.len(),
            Ok(_) => return Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        }
    }
    let Some(pending) = load_existing_pending(pending_path).await? else {
        return Ok(false);
    };
    if lengths[0] != u64::try_from(pending.private_key.len()).unwrap_or(u64::MAX)
        || tokio::fs::read(key_path).await? != pending.private_key.as_bytes()
    {
        return Err(EnrollmentError::InvalidResponse(
            "completed worker identity does not match its pending enrollment key".to_string(),
        ));
    }
    if lengths[1] == 0
        || lengths[2] == 0
        || lengths[1] > MAX_ENROLLMENT_RESPONSE_BYTES as u64
        || lengths[2] > MAX_ENROLLMENT_RESPONSE_BYTES as u64
    {
        return Err(EnrollmentError::InvalidResponse(
            "completed worker identity contains an empty or oversized certificate".to_string(),
        ));
    }
    if lengths[3] == 0 || lengths[3] > MAX_PENDING_ENROLLMENT_BYTES as u64 {
        return Err(EnrollmentError::InvalidResponse(
            "completed worker configuration is empty or oversized".to_string(),
        ));
    }
    let config: AgentConfig = serde_json::from_slice(&tokio::fs::read(config_path).await?)?;
    config.validate().map_err(|error| {
        EnrollmentError::InvalidResponse(format!(
            "completed worker configuration is invalid: {error}"
        ))
    })?;
    if config.private_key_path != relative_file(key_path)
        || config.certificate_path != relative_file(cert_path)
        || config.ca_path != relative_file(ca_path)
    {
        return Err(EnrollmentError::InvalidResponse(
            "completed worker identity references unexpected credential paths".to_string(),
        ));
    }
    Ok(true)
}

async fn cleanup_completed_pending(path: &Path) {
    match tokio::fs::remove_file(path).await {
        Ok(()) => {
            if let Err(error) = sync_parent_directory(path).await {
                tracing::warn!(
                    %error,
                    path = %path.display(),
                    "worker enrollment completed but pending cleanup was not directory-synced"
                );
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(
            %error,
            path = %path.display(),
            "worker enrollment completed but pending cleanup will be retried"
        ),
    }
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

#[cfg(unix)]
async fn sync_parent_directory(path: &Path) -> Result<(), std::io::Error> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("enrollment state path has no parent"))?;
    tokio::fs::File::open(parent).await?.sync_all().await
}

#[cfg(not(unix))]
async fn sync_parent_directory(_path: &Path) -> Result<(), std::io::Error> {
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
    sync_parent_directory(path).await?;
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

    #[test]
    fn enrollment_retry_policy_is_bounded_and_only_retries_transient_statuses() {
        assert!(transient_status(reqwest::StatusCode::REQUEST_TIMEOUT));
        assert!(transient_status(reqwest::StatusCode::TOO_MANY_REQUESTS));
        assert!(transient_status(reqwest::StatusCode::SERVICE_UNAVAILABLE));
        assert!(!transient_status(reqwest::StatusCode::UNAUTHORIZED));
        let operation = uuid::Uuid::new_v4();
        for attempt in 0..ENROLLMENT_RETRY_ATTEMPTS {
            assert!(
                enrollment_jitter(operation, attempt, ENROLLMENT_RETRY_MAX) <= ENROLLMENT_RETRY_MAX
            );
        }
    }

    #[tokio::test]
    async fn enrollment_response_rejects_oversized_content_length() {
        use tokio::io::AsyncWriteExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let mut chunk = [0u8; 1_024];
                let read = socket.read(&mut chunk).await.unwrap();
                assert!(read > 0, "client closed before sending request headers");
                request.extend_from_slice(&chunk[..read]);
                assert!(request.len() <= 16 * 1_024, "request headers are too large");
            }
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 1048577\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            socket.shutdown().await.unwrap();
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

    #[tokio::test]
    async fn ambiguous_enrollment_reuses_the_persisted_key_csr_and_operation() {
        let directory = std::env::temp_dir().join(format!("rsctf-enroll-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir(&directory).await.unwrap();
        let path = directory.join("worker-enrollment-pending.json");
        let first = load_or_create_pending(&path, None).await.unwrap();
        let second = load_or_create_pending(&path, None).await.unwrap();
        assert_eq!(first.operation_id, second.operation_id);
        assert_eq!(first.private_key, second.private_key);
        assert_eq!(first.csr_pem, second.csr_pem);
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn pending_enrollment_state_is_regular_and_byte_bounded() {
        let directory =
            std::env::temp_dir().join(format!("rsctf-enroll-bounds-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir(&directory).await.unwrap();
        let path = directory.join("worker-enrollment-pending.json");
        tokio::fs::write(&path, vec![b'x'; MAX_PENDING_ENROLLMENT_BYTES + 1])
            .await
            .unwrap();
        assert!(matches!(
            load_or_create_pending(&path, None).await,
            Err(EnrollmentError::InvalidResponse(_))
        ));
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn completed_identity_retry_requires_the_exact_pending_private_key() {
        let directory = std::env::temp_dir().join(format!("rsctf-enroll-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir(&directory).await.unwrap();
        let key = directory.join("worker-key.pem");
        let cert = directory.join("worker-cert.pem");
        let ca = directory.join("worker-ca.pem");
        let config_path = directory.join("worker.json");
        let pending_path = directory.join("worker-enrollment-pending.json");
        let private_key = "exact-private-key";
        let pending = PendingEnrollment {
            operation_id: uuid::Uuid::new_v4(),
            private_key: private_key.to_string(),
            csr_pem: "exact-csr".to_string(),
        };
        let config = AgentConfig {
            worker_id: uuid::Uuid::new_v4(),
            control_address: "control.example:443".to_string(),
            data_address: "data.example:443".to_string(),
            server_name: "worker.example".to_string(),
            certificate_path: relative_file(&cert),
            private_key_path: relative_file(&key),
            ca_path: relative_file(&ca),
            capacity: None,
            labels: Default::default(),
        };
        tokio::fs::write(&key, private_key).await.unwrap();
        tokio::fs::write(&cert, b"certificate").await.unwrap();
        tokio::fs::write(&ca, b"certificate-authority")
            .await
            .unwrap();
        tokio::fs::write(&config_path, serde_json::to_vec(&config).unwrap())
            .await
            .unwrap();
        tokio::fs::write(&pending_path, serde_json::to_vec(&pending).unwrap())
            .await
            .unwrap();

        assert!(
            completed_identity_matches_pending(&key, &cert, &ca, &config_path, &pending_path)
                .await
                .unwrap()
        );
        tokio::fs::write(&key, b"different-private-key")
            .await
            .unwrap();
        assert!(matches!(
            completed_identity_matches_pending(&key, &cert, &ca, &config_path, &pending_path).await,
            Err(EnrollmentError::InvalidResponse(_))
        ));
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn completed_identity_cleanup_failure_is_nonfatal_and_retryable() {
        let directory = std::env::temp_dir().join(format!("rsctf-enroll-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir(&directory).await.unwrap();
        let pending_path = directory.join("worker-enrollment-pending.json");
        tokio::fs::create_dir(&pending_path).await.unwrap();

        cleanup_completed_pending(&pending_path).await;
        assert!(pending_path.is_dir());
        tokio::fs::remove_dir(&pending_path).await.unwrap();
        tokio::fs::write(&pending_path, b"retryable pending state")
            .await
            .unwrap();
        cleanup_completed_pending(&pending_path).await;
        assert!(!pending_path.exists());
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }
}
