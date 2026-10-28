//! Authenticated, transactionally recoverable certificate renewal.
use super::{database_error, SessionFence, WorkerStore, WorkerStoreError};
use crate::services::worker_pki::WorkerIssuer;
use chrono::{DateTime, Utc};
use rsctf_worker_protocol::{CertificateRenewed, RenewCertificate};
use sha2::{Digest, Sha256};
use std::sync::Arc;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod transport_tests;

fn invalid(message: &str) -> WorkerStoreError {
    WorkerStoreError::InvalidInput(message.into())
}

impl WorkerStore {
    pub async fn renew_certificate(
        &self,
        session: SessionFence,
        request: &RenewCertificate,
        issuer: Arc<WorkerIssuer>,
    ) -> Result<CertificateRenewed, WorkerStoreError> {
        if request.operation_id.is_nil()
            || request.csr_pem.len() > 64 * 1024
            || request.csr_pem.is_empty()
        {
            return Err(invalid("invalid renewal identity or CSR size"));
        }
        let digest: [u8; 32] = Sha256::digest(request.csr_pem.as_bytes()).into();
        let mut transaction = crate::utils::database::begin_sqlx_transaction(&self.pool)
            .await
            .map_err(database_error)?;
        // Lock one worker. A replacement session or revocation wins before any
        // signing; competing rotations cannot each replace its active identity.
        let current = sqlx::query_as::<_, (Vec<u8>, DateTime<Utc>, DateTime<Utc>)>(
            r#"
            SELECT certificate_fingerprint_sha256, certificate_expires_at, clock_timestamp()
              FROM "WorkerNodes" WHERE id = $1 AND session_id = $2 AND session_epoch = $3
               AND lease_expires_at > clock_timestamp() AND administrative_state <> 'Disabled'
               AND certificate_expires_at > clock_timestamp() FOR UPDATE"#,
        )
        .bind(session.worker_id)
        .bind(session.session_id)
        .bind(session.session_epoch)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| invalid("renewal requires the current enabled worker session"))?;
        let previous = sqlx::query_as::<_, (uuid::Uuid, Vec<u8>, Vec<u8>, serde_json::Value)>(
            r#"
            SELECT operation_id, csr_digest, new_fingerprint, response
              FROM "WorkerCertificateRotations" WHERE worker_id = $1"#,
        )
        .bind(session.worker_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        if let Some((operation, csr_digest, new_fingerprint, response)) = previous {
            if operation == request.operation_id {
                if csr_digest != digest || new_fingerprint != current.0 {
                    return Err(invalid(
                        "renewal operation does not match the current identity",
                    ));
                }
                let response = serde_json::from_value(response)
                    .map_err(|_| invalid("invalid stored certificate response"))?;
                transaction.commit().await.map_err(database_error)?;
                return Ok(response);
            }
        }
        if current.1 - current.2 > chrono::Duration::days(30) {
            return Err(invalid(
                "certificate renewal is available within thirty days of expiry",
            ));
        }
        let csr = request.csr_pem.clone();
        let issued = tokio::task::spawn_blocking(move || issuer.issue(session.worker_id, &csr))
            .await
            .map_err(|_| invalid("certificate signer interrupted"))?
            .map_err(|_| invalid("certificate CSR verification or signing failed"))?;
        let response = CertificateRenewed {
            operation_id: request.operation_id,
            certificate_pem: issued.certificate_pem,
        };
        let encoded = serde_json::to_value(&response)
            .map_err(|_| invalid("cannot encode certificate response"))?;
        sqlx::query(
            r#"INSERT INTO "WorkerCertificateRotations"
            (worker_id, operation_id, csr_digest, old_fingerprint, new_fingerprint,
             old_expires_at, overlap_until, response)
            VALUES ($1, $2, $3, $4, $5, $6, LEAST($6, clock_timestamp() + interval '7 days'), $7)
            ON CONFLICT (worker_id) DO UPDATE SET operation_id = EXCLUDED.operation_id,
              csr_digest = EXCLUDED.csr_digest, old_fingerprint = EXCLUDED.old_fingerprint,
              new_fingerprint = EXCLUDED.new_fingerprint, old_expires_at = EXCLUDED.old_expires_at,
              overlap_until = EXCLUDED.overlap_until, response = EXCLUDED.response,
              created_at = clock_timestamp()"#,
        )
        .bind(session.worker_id)
        .bind(request.operation_id)
        .bind(digest.as_slice())
        .bind(&current.0)
        .bind(issued.fingerprint_sha256.as_slice())
        .bind(current.1)
        .bind(encoded)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        // End the old durable session. The response goes out on its still-open
        // control stream; reconnect (also after a lost reply) creates a new epoch.
        sqlx::query(
            r#"UPDATE "WorkerNodes" SET certificate_fingerprint_sha256 = $2,
              certificate_serial = $3, certificate_expires_at = $4,
              session_id = NULL, lease_expires_at = NULL, updated_at = clock_timestamp()
            WHERE id = $1"#,
        )
        .bind(session.worker_id)
        .bind(issued.fingerprint_sha256.as_slice())
        .bind(issued.serial)
        .bind(issued.expires_at)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        Ok(response)
    }
}
