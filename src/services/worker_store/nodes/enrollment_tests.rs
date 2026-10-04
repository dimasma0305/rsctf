use chrono::{Duration, Utc};
use serde_json::json;
use uuid::Uuid;

use super::*;
use crate::services::worker::AuthorityFixture;

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn committed_enrollment_response_is_recovered_after_an_ambiguous_exchange() {
    let fixture = AuthorityFixture::create().await;
    sqlx::raw_sql(
        r#"CREATE TABLE "WorkerEnrollmentOperations" (
               operation_id UUID PRIMARY KEY,
               worker_id UUID NOT NULL REFERENCES "WorkerNodes"(id) ON DELETE CASCADE,
               token_hash BYTEA NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
               csr_digest BYTEA NOT NULL CHECK (octet_length(csr_digest) = 32),
               state TEXT NOT NULL CHECK (state IN ('Signing', 'Completed', 'Retryable', 'Failed')),
               claim_expires_at TIMESTAMPTZ NOT NULL,
               response JSONB,
               created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
               completed_at TIMESTAMPTZ,
               CHECK ((state = 'Completed') = (response IS NOT NULL))
           );"#,
    )
    .execute(&fixture.pool)
    .await
    .unwrap();

    let worker_id = Uuid::new_v4();
    let operation_id = Uuid::new_v4();
    let token_hash = [3_u8; 32];
    let csr_digest = [4_u8; 32];
    sqlx::query(
        r#"INSERT INTO "WorkerNodes" (
               id, name, enrollment_token_hash, enrollment_token_expires_at
           ) VALUES ($1, 'ambiguous-enrollment', $2, clock_timestamp() + interval '5 minutes')"#,
    )
    .bind(worker_id)
    .bind(token_hash.as_slice())
    .execute(&fixture.pool)
    .await
    .unwrap();

    let store = WorkerStore::new(fixture.pool.clone());
    assert!(matches!(
        store
            .claim_enrollment(operation_id, token_hash, csr_digest)
            .await
            .unwrap(),
        EnrollmentClaim::Claimed { worker_id: claimed } if claimed == worker_id
    ));
    let response = json!({
        "workerId": worker_id,
        "controlAddress": "workers.example.test:443",
        "dataAddress": "workers.example.test:443",
        "serverName": "workers.example.test",
        "certificatePem": "certificate",
        "caPem": "ca"
    });
    let enrolled = store
        .complete_enrollment(
            operation_id,
            token_hash,
            WorkerCertificate {
                fingerprint_sha256: [5; 32],
                serial: "serial-1".into(),
                expires_at: Utc::now() + Duration::days(30),
            },
            response.clone(),
        )
        .await
        .unwrap()
        .expect("the claimed token is consumed exactly once");
    assert_eq!(enrolled.id, worker_id);

    assert!(matches!(
        store
            .claim_enrollment(operation_id, token_hash, csr_digest)
            .await
            .unwrap(),
        EnrollmentClaim::Completed { worker_id: replay_worker, response: replay }
            if replay_worker == worker_id && replay == response
    ));
    assert!(matches!(
        store
            .claim_enrollment(operation_id, token_hash, [9; 32])
            .await
            .unwrap(),
        EnrollmentClaim::Invalid
    ));

    fixture.destroy().await;
}
