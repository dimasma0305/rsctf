use super::*;

#[test]
fn source_normalization_coalesces_equivalent_github_urls() {
    assert_eq!(
        normalized_github_url("https://github.com/TCP1P/repo/").unwrap(),
        normalized_github_url("https://github.com/TCP1P/repo.git").unwrap()
    );
}

#[test]
fn private_tokens_are_encrypted_and_bound_to_job_identity() {
    let secret = "0123456789abcdef0123456789abcdef";
    let job = Uuid::new_v4();
    let actor = Uuid::new_v4();
    let (ciphertext, nonce) = encrypt_token(secret, job, 7, actor, "github_pat_secret").unwrap();
    let ciphertext = ciphertext.unwrap();
    assert!(!ciphertext.windows(6).any(|part| part == b"secret"));
    assert_eq!(nonce.unwrap().len(), 12);
}

#[test]
fn private_tokens_have_a_persisted_size_bound() {
    let error = encrypt_token(
        "0123456789abcdef0123456789abcdef",
        Uuid::new_v4(),
        7,
        Uuid::new_v4(),
        &"x".repeat(4 * 1024 + 1),
    )
    .unwrap_err();
    assert!(matches!(error, AppError::BadRequest(_)));
}

#[test]
fn admission_and_workspace_limits_are_compile_time_bounded() {
    assert_eq!(GLOBAL_ACTIVE_JOBS, 2);
    assert_eq!(EVENT_ACTIVE_JOBS, 1);
    assert_eq!(LOCAL_WORKSPACE_MIB, 128);
    assert!(TOTAL_JOB_DEADLINE < Duration::from_secs(16 * 60));
    const { assert!(ADMISSION_RETRY_SECONDS > 0) };
}

#[test]
fn zip_admission_is_durable_before_object_storage_and_worker_claim() {
    let zip_source = include_str!("zip.rs");
    let handler = zip_source.find("async fn enqueue_zip_owned(").unwrap();
    let body = &zip_source[handler..];
    let admission = body.find("begin_admitted(").unwrap();
    let reservation = body.find("source_staged, lease_owner").unwrap();
    let commit = body.find(".commit()").unwrap();
    let storage = body.find("stage_blob(").unwrap();
    assert!(admission < reservation && reservation < commit && commit < storage);

    let source = include_str!("../import_jobs.rs");
    let claim = source.find("async fn claim_job(").unwrap();
    let claim_body = &source[claim..source.find("fn decrypt_token(").unwrap()];
    assert!(claim_body.contains("job.source_kind = 1 OR job.source_staged = TRUE"));
}

#[test]
fn overload_is_immediate_and_carries_retry_after() {
    let response = busy();
    let expected = ADMISSION_RETRY_SECONDS.to_string();
    assert_eq!(
        response.status(),
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        response
            .headers()
            .get(header::RETRY_AFTER)
            .unwrap()
            .to_str()
            .unwrap(),
        expected.as_str()
    );
}

#[test]
fn persisted_results_have_bounded_message_count_and_bytes() {
    let bounded = bounded_result(ChallengeImportResult {
        messages: (0..1_000).map(|_| "é".repeat(4_096)).collect(),
        ..ChallengeImportResult::default()
    });
    assert!(bounded.messages.len() <= result::MAX_RESULT_MESSAGES);
    assert!(bounded
        .messages
        .iter()
        .all(|message| message.len() <= result::MAX_RESULT_MESSAGE_BYTES));
    assert!(
        bounded.messages.iter().map(String::len).sum::<usize>()
            <= result::MAX_RESULT_MESSAGES_BYTES
    );
}

#[test]
fn persisted_job_errors_do_not_expose_internal_details() {
    let error = AppError::internal("secret database topology");
    let persisted = bounded_error(&error);
    assert!(!persisted.contains("secret"));
    assert_eq!(
        persisted,
        "Challenge import failed due to an internal error"
    );
}

#[tokio::test]
async fn aborted_async_owner_still_cleans_its_workspace() {
    let job_id = Uuid::new_v4();
    let path = std::env::temp_dir().join(format!("rsctf-import-{job_id}"));
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let owner = tokio::spawn(async move {
        let workspace = Workspace::create(job_id).unwrap();
        ready_tx.send(()).unwrap();
        std::future::pending::<()>().await;
        drop(workspace);
    });
    ready_rx.await.unwrap();
    owner.abort();
    let _ = owner.await;
    for _ in 0..50 {
        if !path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("aborted workspace owner left {}", path.display());
}

#[tokio::test]
async fn detached_blocking_stage_retains_cleanup_ownership_until_it_exits() {
    let job_id = Uuid::new_v4();
    let path = std::env::temp_dir().join(format!("rsctf-import-{job_id}"));
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let owner = tokio::spawn(async move {
        tokio::task::spawn_blocking(move || {
            let workspace = Workspace::create(job_id).unwrap();
            ready_tx.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(50));
            workspace
        })
        .await
        .unwrap()
    });
    ready_rx.await.unwrap();
    owner.abort();
    let _ = owner.await;
    for _ in 0..100 {
        if !path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("detached blocking stage left {}", path.display());
}
