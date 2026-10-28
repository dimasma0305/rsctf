//! Recoverable certificate renewal over the authenticated control session.
//! Keep the existing private key; only the certificate is atomically replaced.
use crate::{client::ClientError, config::AgentConfig};
use rcgen::{CertificateParams, KeyPair, PublicKeyData};
use rsctf_worker_protocol::{CertificateRenewed, RenewCertificate};
use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::io::AsyncReadExt;

const MAX_IDENTITY_BYTES: u64 = 64 * 1024;
const RENEW_BEFORE_SECONDS: i64 = 29 * 24 * 60 * 60;

fn protocol(message: &str) -> ClientError {
    ClientError::Protocol(message.into())
}
fn io(error: std::io::Error) -> ClientError {
    ClientError::Transport(format!("certificate renewal I/O failed: {error}"))
}
fn pending_path(config: &AgentConfig) -> PathBuf {
    config.certificate_path.with_extension("renewal.json")
}

async fn read_bounded(path: &Path) -> Result<Vec<u8>, ClientError> {
    crate::security::verify_state_file(path).await?;
    let file = tokio::fs::File::open(path).await.map_err(io)?;
    let mut bytes = Vec::new();
    file.take(MAX_IDENTITY_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(io)?;
    if bytes.len() as u64 > MAX_IDENTITY_BYTES {
        return Err(protocol("renewal identity file exceeds the size limit"));
    }
    Ok(bytes)
}

fn certificate_details(
    bytes: &[u8],
    worker: uuid::Uuid,
) -> Result<(i64, i64, Vec<u8>), ClientError> {
    let (_, pem) = x509_parser::pem::parse_x509_pem(bytes)
        .map_err(|_| protocol("invalid renewal certificate PEM"))?;
    let (_, cert) = x509_parser::parse_x509_certificate(&pem.contents)
        .map_err(|_| protocol("invalid renewal certificate DER"))?;
    let dns = format!("worker-{worker}.workers.rsctf.invalid");
    let san = cert
        .subject_alternative_name()
        .map_err(|_| protocol("invalid renewal certificate identity"))?;
    if !san.is_some_and(|san| san.value.general_names.iter().any(|name| matches!(name, x509_parser::extensions::GeneralName::DNSName(value) if *value == dns))) {
        return Err(protocol("renewal certificate has another worker identity"));
    }
    Ok((
        cert.validity().not_before.timestamp(),
        cert.validity().not_after.timestamp(),
        cert.public_key().raw.to_vec(),
    ))
}

/// The pending operation is fsynced before the CSR is sent. An ambiguous reply
/// therefore retries the exact same operation and CSR after process restart.
pub(crate) async fn prepare(config: &AgentConfig) -> Result<Option<RenewCertificate>, ClientError> {
    let path = pending_path(config);
    if tokio::fs::try_exists(&path).await.map_err(io)? {
        let request: RenewCertificate = serde_json::from_slice(&read_bounded(&path).await?)
            .map_err(|_| protocol("invalid pending certificate renewal"))?;
        if request.operation_id.is_nil()
            || request.csr_pem.is_empty()
            || request.csr_pem.len() > 64 * 1024
        {
            return Err(protocol("invalid pending certificate renewal bounds"));
        }
        return Ok(Some(request));
    }
    let certificate = read_bounded(&config.certificate_path).await?;
    let key = read_bounded(&config.private_key_path).await?;
    let worker = config.worker_id;
    let request = tokio::task::spawn_blocking(move || {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| protocol("system clock precedes Unix epoch"))?
            .as_secs() as i64;
        let (_, expires, public_key) = certificate_details(&certificate, worker)?;
        if expires - now > RENEW_BEFORE_SECONDS {
            return Ok(None);
        }
        let key =
            std::str::from_utf8(&key).map_err(|_| protocol("invalid private key encoding"))?;
        let key =
            KeyPair::from_pem(key).map_err(|_| protocol("cannot load renewal private key"))?;
        if key.subject_public_key_info() != public_key {
            return Err(protocol("renewal certificate and private key do not match"));
        }
        let csr = CertificateParams::new(Vec::<String>::new())
            .map_err(|_| protocol("cannot prepare renewal CSR"))?
            .serialize_request(&key)
            .and_then(|csr| csr.pem())
            .map_err(|_| protocol("cannot sign renewal CSR"))?;
        Ok(Some(RenewCertificate {
            operation_id: uuid::Uuid::new_v4(),
            csr_pem: csr,
        }))
    })
    .await
    .map_err(|_| protocol("renewal preparation interrupted"))??;
    if let Some(request) = &request {
        let encoded =
            serde_json::to_vec(request).map_err(|_| protocol("cannot encode renewal operation"))?;
        // A crash during the write must not publish a truncated pending request.
        // Nothing is sent until the complete operation has been atomically published.
        let staged = path.with_extension("pending.tmp");
        if tokio::fs::try_exists(&staged).await.map_err(io)? {
            crate::security::verify_state_file(&staged).await?;
            tokio::fs::remove_file(&staged).await.map_err(io)?;
        }
        crate::enroll::write_new_file(&staged, &encoded)
            .await
            .map_err(io)?;
        tokio::fs::rename(&staged, &path).await.map_err(io)?;
        crate::enroll::sync_parent_directory(&path)
            .await
            .map_err(io)?;
    }
    Ok(request)
}

pub(crate) async fn install(
    config: &AgentConfig,
    response: CertificateRenewed,
) -> Result<(), ClientError> {
    if response.certificate_pem.len() > 16 * 1024 {
        return Err(protocol("renewal response exceeds certificate bound"));
    }
    let pending = pending_path(config);
    let request: RenewCertificate = serde_json::from_slice(&read_bounded(&pending).await?)
        .map_err(|_| protocol("invalid pending renewal operation"))?;
    if response.operation_id != request.operation_id {
        return Err(protocol("renewal response has another operation ID"));
    }
    let key = read_bounded(&config.private_key_path).await?;
    let certificate = response.certificate_pem.into_bytes();
    let worker = config.worker_id;
    let certificate = tokio::task::spawn_blocking(move || {
        let (valid_from, expires, public_key) = certificate_details(&certificate, worker)?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| protocol("invalid system clock"))?
            .as_secs() as i64;
        let key = KeyPair::from_pem(
            std::str::from_utf8(&key).map_err(|_| protocol("invalid private key encoding"))?,
        )
        .map_err(|_| protocol("cannot load renewal private key"))?;
        if public_key != key.subject_public_key_info() || expires <= now + 24 * 60 * 60 {
            return Err(protocol("renewal certificate has the wrong key or expiry"));
        }
        if valid_from > now {
            return Err(ClientError::Transport(
                "renewal certificate is not yet valid; check system clock synchronization".into(),
            ));
        }
        Ok(certificate)
    })
    .await
    .map_err(|_| protocol("renewal verification interrupted"))??;
    let staged = config
        .certificate_path
        .with_extension(format!("renewal-{}.tmp", response.operation_id));
    // Only the file owned by this durable operation is replaceable after a crash.
    if tokio::fs::try_exists(&staged).await.map_err(io)? {
        crate::security::verify_state_file(&staged).await?;
        tokio::fs::remove_file(&staged).await.map_err(io)?;
    }
    crate::enroll::write_new_file(&staged, &certificate)
        .await
        .map_err(io)?;
    crate::security::verify_state_file(&config.certificate_path).await?;
    tokio::fs::rename(&staged, &config.certificate_path)
        .await
        .map_err(io)?;
    crate::enroll::sync_parent_directory(&config.certificate_path)
        .await
        .map_err(io)?;
    tokio::fs::remove_file(&pending).await.map_err(io)?;
    crate::enroll::sync_parent_directory(&pending)
        .await
        .map_err(io)?;
    Ok(())
}

#[cfg(test)]
mod tests;
