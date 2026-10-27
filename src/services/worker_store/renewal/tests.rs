use super::*;
use crate::services::{
    worker::{AuthorityFixture, PostgresWorkerAuthority, SessionContext, WorkerAuthority},
    worker_store::{PlatformOs, ResourceReservation, WorkerInventory},
};

fn request(operation_id: uuid::Uuid) -> RenewCertificate {
    let key = rcgen::KeyPair::generate().unwrap();
    RenewCertificate {
        operation_id,
        csr_pem: rcgen::CertificateParams::new(Vec::<String>::new())
            .unwrap()
            .serialize_request(&key)
            .unwrap()
            .pem()
            .unwrap(),
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn renewal_recovers_exact_reply_fences_sessions_and_bounds_old_certificate_authority() {
    let fixture = AuthorityFixture::create().await;
    sqlx::raw_sql(crate::migrations::m0361_worker_certificate_rotation::UP_SQL)
        .execute(&fixture.pool)
        .await
        .unwrap();
    let initial = fixture.insert_current_session().await;
    let session = SessionFence {
        worker_id: initial.worker_id,
        session_id: initial.fence.session_id,
        session_epoch: initial.fence.session_epoch as i64,
    };
    let store = WorkerStore::new(fixture.pool.clone());
    let (issuer, _) = crate::services::worker_pki::tests::issuer();
    let bad_csr = RenewCertificate {
        operation_id: uuid::Uuid::new_v4(),
        csr_pem: "bad CSR".into(),
    };
    assert!(store
        .renew_certificate(session, &bad_csr, issuer.clone())
        .await
        .is_err());
    let request = request(uuid::Uuid::new_v4());
    let response = store
        .renew_certificate(session, &request, issuer.clone())
        .await
        .unwrap();
    let node = store.get_worker(session.worker_id).await.unwrap().unwrap();
    assert!(
        node.session_id.is_none(),
        "rotation must end the old durable session"
    );
    assert!(
        store
            .renew_certificate(session, &request, issuer.clone())
            .await
            .is_err(),
        "old epoch cannot recover a response"
    );
    let fingerprint: Vec<u8> = sqlx::query_scalar(
        r#"SELECT certificate_fingerprint_sha256 FROM "WorkerNodes" WHERE id = $1"#,
    )
    .bind(session.worker_id)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    let fingerprint: [u8; 32] = fingerprint.try_into().unwrap();
    assert_ne!(fingerprint, [7; 32]);
    assert!(store
        .authenticate_certificate([7; 32])
        .await
        .unwrap()
        .is_some());
    assert!(store
        .authenticate_certificate(fingerprint)
        .await
        .unwrap()
        .is_some());
    assert!(sqlx::query_scalar::<_, bool>(
        r#"SELECT overlap_until <= old_expires_at AND overlap_until <= created_at + interval '7 days' FROM "WorkerCertificateRotations" WHERE worker_id = $1"#,
    ).bind(session.worker_id).fetch_one(&fixture.pool).await.unwrap());
    // Simulate a lost successful response: reconnect using the old certificate,
    // then retry the fsynced operation rather than issue a second certificate.
    let inventory = WorkerInventory {
        platform_os: PlatformOs::Linux,
        architecture: "amd64".into(),
        runtime_kind: "docker".into(),
        runtime_version: "test".into(),
        labels: node.labels,
        capabilities: node.capabilities,
        capacity: ResourceReservation {
            cpu_millis: 4000,
            memory_bytes: 8388608,
            slots: 8,
        },
    };
    let replacement = store
        .open_session(
            session.worker_id,
            [7; 32],
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            &inventory,
            std::time::Duration::from_secs(60),
        )
        .await
        .unwrap()
        .unwrap()
        .fence;
    assert!(replacement.session_epoch > session.session_epoch);
    assert_eq!(
        store
            .renew_certificate(replacement, &request, issuer.clone())
            .await
            .unwrap(),
        response
    );
    let changed = RenewCertificate {
        operation_id: request.operation_id,
        csr_pem: "other request".into(),
    };
    assert!(store
        .renew_certificate(replacement, &changed, issuer.clone())
        .await
        .is_err());
    let competing = RenewCertificate {
        operation_id: uuid::Uuid::new_v4(),
        csr_pem: request.csr_pem.clone(),
    };
    assert!(
        store
            .renew_certificate(replacement, &competing, issuer.clone())
            .await
            .is_err(),
        "new certificate is outside renewal window"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(r#"SELECT count(*) FROM "WorkerCertificateRotations""#)
            .fetch_one(&fixture.pool)
            .await
            .unwrap(),
        1
    );
    sqlx::query(r#"UPDATE "WorkerCertificateRotations" SET overlap_until = clock_timestamp() - interval '1 second'"#).execute(&fixture.pool).await.unwrap();
    assert!(store
        .authenticate_certificate([7; 32])
        .await
        .unwrap()
        .is_none());
    assert!(store
        .open_session(
            session.worker_id,
            [7; 32],
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            &inventory,
            std::time::Duration::from_secs(60)
        )
        .await
        .unwrap()
        .is_none());
    assert!(store
        .authenticate_certificate(fingerprint)
        .await
        .unwrap()
        .is_some());
    // A pre-existing TLS connection must also lose authority after overlap,
    // even though the replacement node certificate has a later expiry.
    let authority = PostgresWorkerAuthority::new(store.clone(), std::time::Duration::from_secs(60));
    let stale_connection = SessionContext {
        worker_id: replacement.worker_id,
        boot_id: uuid::Uuid::new_v4(),
        certificate_fingerprint_sha256: [7; 32],
        fence: rsctf_worker_protocol::SessionFence {
            session_id: replacement.session_id,
            session_epoch: replacement.session_epoch as u64,
        },
    };
    assert!(authority
        .handle_inbound(
            &stale_connection,
            rsctf_worker_protocol::ControlMessage::Heartbeat(rsctf_worker_protocol::Heartbeat {
                sent_at_unix_ms: chrono::Utc::now().timestamp_millis(),
                usage: rsctf_worker_protocol::ResourceUsage {
                    reserved_cpu_millis: 0,
                    reserved_memory_bytes: 0,
                    running_workloads: 0,
                },
                runtime_healthy: true,
                runtime_error: None,
            })
        )
        .await
        .is_err());
    // A manual enrollment invalidates the old overlap mapping immediately.
    sqlx::query(r#"UPDATE "WorkerCertificateRotations" SET overlap_until = clock_timestamp() + interval '1 hour'"#).execute(&fixture.pool).await.unwrap();
    sqlx::query(r#"UPDATE "WorkerNodes" SET certificate_fingerprint_sha256 = $1 WHERE id = $2"#)
        .bind([9_u8; 32].as_slice())
        .bind(session.worker_id)
        .execute(&fixture.pool)
        .await
        .unwrap();
    assert!(store
        .authenticate_certificate([7; 32])
        .await
        .unwrap()
        .is_none());
    assert!(store
        .authenticate_certificate(fingerprint)
        .await
        .unwrap()
        .is_none());
    sqlx::query(r#"UPDATE "WorkerNodes" SET certificate_fingerprint_sha256 = $1 WHERE id = $2"#)
        .bind(fingerprint.as_slice())
        .bind(session.worker_id)
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query(r#"UPDATE "WorkerNodes" SET administrative_state = 'Disabled' WHERE id = $1"#)
        .bind(session.worker_id)
        .execute(&fixture.pool)
        .await
        .unwrap();
    assert!(store
        .authenticate_certificate(fingerprint)
        .await
        .unwrap()
        .is_none());
    assert!(store
        .renew_certificate(replacement, &request, issuer)
        .await
        .is_err());
    fixture.destroy().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn renewal_operation_cannot_be_replayed_by_another_worker() {
    let fixture = AuthorityFixture::create().await;
    let store = WorkerStore::new(fixture.pool.clone());
    let (issuer, _) = crate::services::worker_pki::tests::issuer();
    let first = fixture.insert_current_session().await;
    let second = fixture.insert_current_session().await;
    sqlx::query(r#"UPDATE "WorkerNodes" SET certificate_fingerprint_sha256 = $1 WHERE id = $2"#)
        .bind([8_u8; 32].as_slice())
        .bind(second.worker_id)
        .execute(&fixture.pool)
        .await
        .unwrap();
    let fence = |s: &SessionContext| SessionFence {
        worker_id: s.worker_id,
        session_id: s.fence.session_id,
        session_epoch: s.fence.session_epoch as i64,
    };
    let request = request(uuid::Uuid::new_v4());
    store
        .renew_certificate(fence(&first), &request, issuer.clone())
        .await
        .unwrap();
    assert!(store
        .renew_certificate(fence(&second), &request, issuer)
        .await
        .is_err());
    let node = store.get_worker(second.worker_id).await.unwrap().unwrap();
    assert_eq!(node.session_id, Some(second.fence.session_id));
    assert!(store
        .authenticate_certificate([8; 32])
        .await
        .unwrap()
        .is_some());
    fixture.destroy().await;
}
