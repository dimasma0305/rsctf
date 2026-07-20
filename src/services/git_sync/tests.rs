use super::*;

mod repository_concurrency;
mod repository_regression;

async fn import_with_game_lock(
    state: &SharedState,
    game_id: i32,
    manifest: &Path,
) -> AppResult<ManifestImportResult> {
    let lock = crate::services::ad_engine::acquire_ad_game_lock(&state.db, game_id).await?;
    let result = import_manifest(state, game_id, manifest, ImportPolicy::Trusted).await;
    lock.release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    result
}

#[test]
fn pending_imports_are_inert_from_the_initial_insert() {
    let now = Utc::now();
    let policy = ImportPolicy::PendingReview;
    assert_eq!(policy.review_status(), ChallengeReviewStatus::Pending);
    assert_eq!(policy.reviewed_at(now), None);
    assert!(!policy.may_execute());
}

#[test]
fn trusted_imports_preserve_inline_preparation() {
    let now = Utc::now();
    let policy = ImportPolicy::Trusted;
    assert_eq!(policy.review_status(), ChallengeReviewStatus::Active);
    assert_eq!(policy.reviewed_at(now), Some(now));
    assert!(policy.may_execute());
}

#[test]
fn source_paths_are_binding_relative_and_replica_independent() {
    let root = std::env::temp_dir().join(format!(
        "rsctf-durable-source-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let checkout = root.join("repos/7/challenge");
    std::fs::create_dir_all(&checkout).unwrap();
    let manifest = checkout.join("challenge.yml");
    std::fs::write(&manifest, b"name: example\n").unwrap();
    let outside = root.join("temporary.yml");
    std::fs::write(&outside, b"name: temporary\n").unwrap();

    assert_eq!(
        durable_repo_manifest_path(root.to_str().unwrap(), Some(7), &manifest),
        Some("binding/7/challenge/challenge.yml".to_string())
    );
    assert_eq!(
        durable_repo_manifest_path(root.to_str().unwrap(), Some(7), &outside),
        None
    );
    assert_eq!(
        durable_repo_manifest_path(root.to_str().unwrap(), None, &manifest),
        None
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn checkout_lock_serializes_one_checkout_only() {
    let root = std::env::temp_dir().join(format!("rsctf-lock-{}", uuid::Uuid::new_v4()));
    let same = root.join("repo");
    let different = root.join("other");
    let first = lock_checkout(&same).await;

    let independent = tokio::time::timeout(Duration::from_millis(250), lock_checkout(&different))
        .await
        .expect("different checkouts must not block each other");
    drop(independent);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), lock_checkout(&same))
            .await
            .is_err(),
        "the same checkout must remain locked"
    );

    drop(first);
    tokio::time::timeout(Duration::from_millis(250), lock_checkout(&same))
        .await
        .expect("the checkout lock must be released with its guard");
}

#[test]
fn repository_url_policy_rejects_local_and_option_like_transports() {
    assert!(validate_github_repo_url("https://github.com/rsctf/example.git").is_ok());
    assert!(validate_github_repo_url("http://github.com/rsctf/example.git").is_err());
    assert!(validate_github_repo_url("https://github.com.evil.test/a/b").is_err());
    for invalid in [
        "--upload-pack=/tmp/pwn",
        "/tmp/repo",
        "file:///tmp/repo",
        "ext::sh -c id",
        "ssh://example.com/repo",
        "https://user:pass@example.com/repo",
        "http://127.0.0.1/repo",
        "http://localhost/repo",
    ] {
        assert!(
            validate_binding_repo_url(invalid).is_err(),
            "accepted {invalid}"
        );
    }
    assert!(validate_binding_repo_url("https://git.example.com/team/repo.git").is_ok());
}

#[test]
fn git_refs_reject_option_and_ref_syntax_injection() {
    for invalid in [
        "--upload-pack=evil",
        "main..evil",
        "bad ref",
        "x@{y",
        "a\\b",
    ] {
        assert!(
            validate_git_ref(Some(invalid)).is_err(),
            "accepted {invalid}"
        );
    }
    assert_eq!(
        validate_git_ref(Some(" refs/tags/v1 ")).unwrap().as_deref(),
        Some("refs/tags/v1")
    );
    assert_eq!(validate_git_ref(None).unwrap(), None);
}

#[test]
fn credentials_are_encoded_and_removable() {
    let authenticated =
        GitCredentials::new("token:@/value").apply("https://github.com/rsctf/example.git");
    validate_sync_repo_url(&authenticated).unwrap();
    assert_eq!(
        url_without_credentials(&authenticated).unwrap(),
        "https://github.com/rsctf/example.git"
    );
}

#[tokio::test]
async fn checkout_tree_limits_depth_before_packaging() {
    let root = std::env::temp_dir().join(format!("rsctf-tree-{}", uuid::Uuid::new_v4()));
    let mut current = root.clone();
    for _ in 0..=MAX_REPO_DEPTH {
        current.push("d");
    }
    tokio::fs::create_dir_all(&current).await.unwrap();
    tokio::fs::write(current.join("file"), b"x").await.unwrap();
    assert!(validate_checkout_tree(&root).await.is_err());
    let _ = tokio::fs::remove_dir_all(root).await;
}
