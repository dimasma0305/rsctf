//! Exercise the real framed mTLS transport against PostgreSQL authority.
use super::*;
use crate::services::worker::{
    build_mtls_server_config, AuthorityFixture, PostgresWorkerAuthority, WorkerServerConfig,
    WorkerService,
};
use rcgen::{CertificateParams, KeyPair};
use rsctf_worker_protocol::*;
use std::{net::SocketAddr, time::Duration};
use tokio::{net::TcpStream, sync::watch};
use tokio_rustls::{
    client::TlsStream,
    rustls::{
        pki_types::CertificateDer, pki_types::PrivatePkcs8KeyDer, ClientConfig, RootCertStore,
    },
    TlsConnector,
};

fn certificates(pem: &str) -> Vec<CertificateDer<'static>> {
    rustls_pemfile::certs(&mut std::io::Cursor::new(pem))
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

async fn connect(
    address: SocketAddr,
    server: CertificateDer<'static>,
    certificate: &str,
    key: &KeyPair,
    worker_id: uuid::Uuid,
) -> anyhow::Result<(TlsStream<TcpStream>, ServerWelcome)> {
    let mut roots = RootCertStore::empty();
    roots.add(server)?;
    let mut tls = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_client_auth_cert(
            certificates(certificate),
            PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
        )?;
    tls.alpn_protocols = vec![CONTROL_ALPN.to_vec()];
    let mut stream = TlsConnector::from(Arc::new(tls))
        .connect(
            "localhost".try_into().unwrap(),
            TcpStream::connect(address).await?,
        )
        .await?;
    write_json_frame(
        &mut stream,
        &WorkerHello {
            protocol_revision: PROTOCOL_REVISION,
            worker_id,
            boot_id: uuid::Uuid::new_v4(),
            agent_version: "renewal-test".into(),
            platform: Platform {
                operating_system: OperatingSystem::Linux,
                architecture: "amd64".into(),
                windows_build: None,
            },
            runtime: RuntimeDescriptor {
                kind: RuntimeKind::Docker,
                version: Some("test".into()),
                endpoint_kind: None,
            },
            capabilities: WorkerCapabilities {
                ensure_workload: true,
                write_flag: true,
                tcp_proxy: true,
                interactive_exec: false,
                inventory: true,
                local_image_build: false,
                max_data_lanes: 4,
                max_workload_replicas: 512,
            },
            capacity: WorkerCapacity {
                cpu_millis: 4000,
                memory_bytes: 8388608,
                slots: 8,
            },
            labels: Default::default(),
        },
    )
    .await?;
    let welcome = read_json_frame(&mut stream).await?;
    Ok((stream, welcome))
}

async fn renew(
    stream: &mut TlsStream<TcpStream>,
    welcome: &ServerWelcome,
    request: RenewCertificate,
) -> CertificateRenewed {
    assert!(welcome.certificate_renewal);
    write_json_frame(
        stream,
        &ControlEnvelope::new(
            welcome.session.session_epoch,
            ControlMessage::RenewCertificate(request),
        ),
    )
    .await
    .unwrap();
    loop {
        let response: ControlEnvelope = read_json_frame(stream).await.unwrap();
        assert_eq!(response.session_epoch, welcome.session.session_epoch);
        match response.body {
            ControlMessage::CertificateRenewed(response) => return response,
            ControlMessage::InventoryRequest(_) => {}
            other => panic!("unexpected renewal response: {other:?}"),
        }
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn mtls_renewal_reconnects_recovers_lost_reply_and_rejects_revoked_identity() {
    // This is a local recovery deadline, not a production network SLA.
    tokio::time::timeout(Duration::from_secs(20), scenario())
        .await
        .unwrap();
}

async fn scenario() {
    let _ = tokio_rustls::rustls::crypto::aws_lc_rs::default_provider().install_default();
    let fixture = AuthorityFixture::create().await;
    let worker = fixture.insert_current_session().await;
    let (issuer, _) = crate::services::worker_pki::tests::issuer();
    let key = KeyPair::generate().unwrap();
    let csr = CertificateParams::new(Vec::<String>::new())
        .unwrap()
        .serialize_request(&key)
        .unwrap()
        .pem()
        .unwrap();
    let initial = issuer.issue(worker.worker_id, &csr).unwrap();
    // Move only the authoritative expiry into the renewal window. Both actual
    // certificates are checked by rustls with the real dedicated worker CA.
    sqlx::query(r#"UPDATE "WorkerNodes" SET certificate_fingerprint_sha256 = $1 WHERE id = $2"#)
        .bind(initial.fingerprint_sha256.as_slice())
        .bind(worker.worker_id)
        .execute(&fixture.pool)
        .await
        .unwrap();
    let server_key = KeyPair::generate().unwrap();
    let server_cert = CertificateParams::new(vec!["localhost".into()])
        .unwrap()
        .self_signed(&server_key)
        .unwrap();
    let mut worker_roots = RootCertStore::empty();
    for certificate in certificates(&initial.ca_certificate_pem) {
        worker_roots.add(certificate).unwrap();
    }
    let tls = build_mtls_server_config(
        vec![server_cert.der().clone()],
        PrivatePkcs8KeyDer::from(server_key.serialize_der()).into(),
        worker_roots,
    )
    .unwrap();
    let authority = PostgresWorkerAuthority::new(
        WorkerStore::new(fixture.pool.clone()),
        Duration::from_secs(30),
    )
    .with_issuer(issuer);
    let service = WorkerService::new(WorkerServerConfig::default(), Arc::new(authority)).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, shutdown) = watch::channel(false);
    let task = tokio::spawn(async move { service.serve(listener, Arc::new(tls), shutdown).await });
    let (mut first, welcome) = connect(
        address,
        server_cert.der().clone(),
        &initial.certificate_pem,
        &key,
        worker.worker_id,
    )
    .await
    .unwrap();
    let request = RenewCertificate {
        operation_id: uuid::Uuid::new_v4(),
        csr_pem: csr,
    };
    let issued = renew(&mut first, &welcome, request.clone()).await;
    drop(first);
    // The caller lost its reply and persisted no replacement: old certificate
    // reconnect + exact operation must recover the same signed certificate.
    let (mut recovery, recovered_welcome) = connect(
        address,
        server_cert.der().clone(),
        &initial.certificate_pem,
        &key,
        worker.worker_id,
    )
    .await
    .unwrap();
    assert!(recovered_welcome.session.session_epoch > welcome.session.session_epoch);
    assert_eq!(
        renew(&mut recovery, &recovered_welcome, request).await,
        issued
    );
    drop(recovery);
    let (current, current_welcome) = connect(
        address,
        server_cert.der().clone(),
        &issued.certificate_pem,
        &key,
        worker.worker_id,
    )
    .await
    .unwrap();
    assert!(current_welcome.session.session_epoch > recovered_welcome.session.session_epoch);
    drop(current);
    sqlx::query(r#"UPDATE "WorkerCertificateRotations" SET overlap_until = clock_timestamp() - interval '1 second'"#)
        .execute(&fixture.pool).await.unwrap();
    assert!(connect(
        address,
        server_cert.der().clone(),
        &initial.certificate_pem,
        &key,
        worker.worker_id
    )
    .await
    .is_err());
    sqlx::query(r#"UPDATE "WorkerNodes" SET administrative_state = 'Disabled' WHERE id = $1"#)
        .bind(worker.worker_id)
        .execute(&fixture.pool)
        .await
        .unwrap();
    assert!(connect(
        address,
        server_cert.der().clone(),
        &issued.certificate_pem,
        &key,
        worker.worker_id
    )
    .await
    .is_err());
    stop.send(true).unwrap();
    task.await.unwrap().unwrap();
    fixture.destroy().await;
}
