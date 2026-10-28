use super::*;

fn certificate(key: &KeyPair, worker: uuid::Uuid, expired: bool) -> String {
    let mut params =
        CertificateParams::new(vec![format!("worker-{worker}.workers.rsctf.invalid")]).unwrap();
    if expired {
        params.not_after = rcgen::date_time_ymd(2000, 1, 1);
    }
    params.self_signed(key).unwrap().pem()
}

async fn fixture() -> (PathBuf, AgentConfig, KeyPair) {
    let root = std::env::temp_dir().join(format!("rsctf-renewal-{}", uuid::Uuid::new_v4()));
    crate::security::prepare_state_dir(&root, None, None)
        .await
        .unwrap();
    let key = KeyPair::generate().unwrap();
    let worker_id = uuid::Uuid::new_v4();
    let config = AgentConfig {
        worker_id,
        control_address: "localhost:443".into(),
        data_address: "localhost:443".into(),
        server_name: "localhost".into(),
        certificate_path: root.join("worker-cert.pem"),
        private_key_path: root.join("worker-key.pem"),
        ca_path: root.join("worker-ca.pem"),
        capacity: None,
        labels: Default::default(),
    };
    crate::enroll::write_new_file(&config.private_key_path, key.serialize_pem().as_bytes())
        .await
        .unwrap();
    crate::enroll::write_new_file(
        &config.certificate_path,
        certificate(&key, worker_id, true).as_bytes(),
    )
    .await
    .unwrap();
    (root, config, key)
}

#[tokio::test]
async fn lost_reply_reuses_durable_csr_and_install_recovers_partial_staging() {
    let (root, config, key) = fixture().await;
    let pending_staged = pending_path(&config).with_extension("pending.tmp");
    crate::enroll::write_new_file(&pending_staged, b"partial pending operation")
        .await
        .unwrap();
    let request = prepare(&config).await.unwrap().unwrap();
    assert!(!pending_staged.exists());
    assert_eq!(prepare(&config).await.unwrap().unwrap(), request);
    crate::security::verify_state_file(&pending_path(&config))
        .await
        .unwrap();
    let staged = config
        .certificate_path
        .with_extension(format!("renewal-{}.tmp", request.operation_id));
    crate::enroll::write_new_file(&staged, b"partial previous process output")
        .await
        .unwrap();
    let renewed = certificate(&key, config.worker_id, false);
    install(
        &config,
        CertificateRenewed {
            operation_id: request.operation_id,
            certificate_pem: renewed.clone(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        tokio::fs::read_to_string(&config.certificate_path)
            .await
            .unwrap(),
        renewed
    );
    assert_eq!(
        tokio::fs::read_to_string(&config.private_key_path)
            .await
            .unwrap(),
        key.serialize_pem()
    );
    assert!(!pending_path(&config).exists());
    assert!(!staged.exists());
    assert!(prepare(&config).await.unwrap().is_none());
    crate::security::verify_state_file(&config.certificate_path)
        .await
        .unwrap();
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[tokio::test]
async fn wrong_operation_worker_key_and_expired_reply_never_replace_the_identity() {
    let (root, config, key) = fixture().await;
    let before = tokio::fs::read(&config.certificate_path).await.unwrap();
    let request = prepare(&config).await.unwrap().unwrap();
    let wrong_key = KeyPair::generate().unwrap();
    for (operation_id, pem) in [
        (
            uuid::Uuid::new_v4(),
            certificate(&key, config.worker_id, false),
        ),
        (
            request.operation_id,
            certificate(&key, uuid::Uuid::new_v4(), false),
        ),
        (
            request.operation_id,
            certificate(&wrong_key, config.worker_id, false),
        ),
        (
            request.operation_id,
            certificate(&key, config.worker_id, true),
        ),
    ] {
        assert!(install(
            &config,
            CertificateRenewed {
                operation_id,
                certificate_pem: pem
            }
        )
        .await
        .is_err());
        assert_eq!(
            tokio::fs::read(&config.certificate_path).await.unwrap(),
            before
        );
        assert!(pending_path(&config).exists());
    }
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[tokio::test]
async fn clock_behind_certificate_validity_preserves_current_identity_for_retry() {
    let (root, config, key) = fixture().await;
    let request = prepare(&config).await.unwrap().unwrap();
    let before = tokio::fs::read(&config.certificate_path).await.unwrap();
    let mut params = CertificateParams::new(vec![format!(
        "worker-{}.workers.rsctf.invalid",
        config.worker_id
    )])
    .unwrap();
    params.not_before = rcgen::date_time_ymd(4000, 1, 1);
    params.not_after = rcgen::date_time_ymd(4001, 1, 1);
    let result = install(
        &config,
        CertificateRenewed {
            operation_id: request.operation_id,
            certificate_pem: params.self_signed(&key).unwrap().pem(),
        },
    )
    .await;
    assert!(matches!(result, Err(ClientError::Transport(_))));
    assert_eq!(
        tokio::fs::read(&config.certificate_path).await.unwrap(),
        before
    );
    assert_eq!(prepare(&config).await.unwrap().unwrap(), request);
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[tokio::test]
async fn interrupted_after_certificate_rename_can_replay_and_finish_pending_cleanup() {
    let (root, config, key) = fixture().await;
    let request = prepare(&config).await.unwrap().unwrap();
    let pem = certificate(&key, config.worker_id, false);
    // Simulate the durable certificate rename succeeding immediately before a crash.
    tokio::fs::write(&config.certificate_path, &pem)
        .await
        .unwrap();
    assert_eq!(prepare(&config).await.unwrap().unwrap(), request);
    install(
        &config,
        CertificateRenewed {
            operation_id: request.operation_id,
            certificate_pem: pem,
        },
    )
    .await
    .unwrap();
    assert!(prepare(&config).await.unwrap().is_none());
    tokio::fs::remove_dir_all(root).await.unwrap();
}
