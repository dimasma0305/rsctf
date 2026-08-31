use super::*;

mod repository_concurrency;
mod repository_identity_plan;
mod repository_regression;
mod variant_generator_regression;

#[test]
fn repository_challenges_disable_blood_bonus_unless_explicitly_enabled() {
    assert!(disable_blood_bonus_or_default(None));
    assert!(disable_blood_bonus_or_default(Some(true)));
    assert!(!disable_blood_bonus_or_default(Some(false)));
}

#[test]
fn import_stages_artifacts_before_reacquiring_domain_fences() {
    let source = include_str!("mod.rs");
    let archive_stage = source.find("let archive_intent = stage_archive(").unwrap();
    let attachment_stage = source
        .find("let attachment_intent = stage_attachment(")
        .unwrap();
    let reacquire = source.find("reacquire_import(").unwrap();
    assert!(archive_stage < reacquire);
    assert!(attachment_stage < reacquire);
}

#[test]
fn new_import_reserves_its_definition_fence_before_insert() {
    let source = include_str!("mod.rs");
    let reserve = source.find(".reserve_created_challenge(game_id)").unwrap();
    let persist = source
        .find("let persisted: AppResult<(game_challenge::Model, bool)>")
        .unwrap();
    assert!(reserve < persist);
    assert!(!source.contains("bind_created_challenge"));
}

#[test]
fn known_git_sync_publications_finalize_their_stage_receipts() {
    let attachment = include_str!("attach.rs");
    let archive = include_str!("archive.rs");
    for source in [attachment, archive] {
        assert!(source.contains("consume_with_existing_reference_as"));
        assert!(source.contains("state = 'Published'"));
        assert!(source.contains("published_owner_scope = $2"));
    }
}

#[test]
fn uncertain_attachment_commit_keeps_post_commit_invalidation_work() {
    let attachment = include_str!("attach.rs");
    let import = include_str!("mod.rs");
    assert!(attachment.contains("post_commit: AttachmentPostCommit"));
    assert!(attachment.contains("return Err(AttachmentPublishFailure"));
    assert!(import.contains("(false, failure.post_commit, true)"));
    let finish = import.find("finish_attachment_post_commit(").unwrap();
    let discard = import
        .rfind("discard_attachment(st, &attachment_intent).await")
        .unwrap();
    assert!(finish < discard);
}

#[test]
fn test_container_archive_finalizes_every_stage_outcome() {
    let source = include_str!("../../controllers/edit/test_container/archive.rs");
    assert!(source.contains("state = 'Published'"));
    assert!(source.contains("discard_unpublished_stage("));
}

async fn import_with_game_lock(
    state: &SharedState,
    game_id: i32,
    manifest: &Path,
) -> AppResult<ManifestImportResult> {
    // The import owns its short snapshot and publication fences. Keeping this
    // historical helper name avoids churn in the regression suite while
    // ensuring tests do not self-contend on an outer game transaction.
    import_manifest(state, game_id, manifest, ImportPolicy::Trusted).await
}

#[test]
fn pending_imports_are_inert_from_the_initial_insert() {
    let now = Utc::now();
    let submitter = uuid::Uuid::new_v4();
    let policy = ImportPolicy::PendingReview {
        submitted_by_user_id: submitter,
    };
    assert_eq!(policy.review_status(), ChallengeReviewStatus::Pending);
    assert_eq!(policy.reviewed_at(now), None);
    assert!(!policy.may_execute());
    assert_eq!(policy.submitted_by_user_id(), Some(submitter));
}

#[test]
fn trusted_imports_preserve_inline_preparation() {
    let now = Utc::now();
    let policy = ImportPolicy::Trusted;
    assert_eq!(policy.review_status(), ChallengeReviewStatus::Active);
    assert_eq!(policy.reviewed_at(now), Some(now));
    assert!(policy.may_execute());
    assert_eq!(policy.submitted_by_user_id(), None);
}

#[test]
fn pending_manifest_complexity_is_bounded_before_database_writes() {
    let mut model = ChallengeYaml {
        flags: Some(vec!["flag{ok}".to_string(); MAX_PENDING_STATIC_FLAGS]),
        hints: Some(vec!["hint".to_string(); MAX_PENDING_HINTS]),
        ..Default::default()
    };
    assert!(validate_pending_manifest(&model).is_ok());

    model.flags.as_mut().unwrap().push("flag{too_many}".into());
    assert!(validate_pending_manifest(&model).is_err());
    model.flags.as_mut().unwrap().pop();

    model.hints.as_mut().unwrap().push("too many".into());
    assert!(validate_pending_manifest(&model).is_err());
}

#[test]
fn challenge_manifest_parses_provenance_automation_fields() {
    let digest = format!("sha256:{}", "a".repeat(64));
    let source = format!(
        "name: Deterministic example\ntype: StaticAttachment\nvariantMode: PerParticipation\nvariantGeneratorImage: ghcr.io/example/generator@{digest}\nvariantGeneratorDigest: {digest}\nsolveReceiptMode: Required\nreceiptVerifierIdentity: example-verifier-v1\n"
    );
    let parsed: ChallengeYaml = serde_norway::from_str(&source).unwrap();

    assert_eq!(
        parsed.variant_mode,
        Some(crate::utils::enums::ChallengeVariantMode::PerParticipation)
    );
    assert_eq!(parsed.variant_generator_digest.as_deref(), Some(&*digest));
    assert_eq!(
        parsed.solve_receipt_mode,
        Some(crate::utils::enums::SolveReceiptMode::Required)
    );
    assert_eq!(
        parsed.receipt_verifier_identity.as_deref(),
        Some("example-verifier-v1")
    );
}

#[test]
fn new_import_review_metadata_keeps_submitter_attribution() {
    let now = Utc::now();
    let submitter = uuid::Uuid::new_v4();
    let mut pending = <game_challenge::ActiveModel as Default>::default();
    initialize_new_import_review(
        &mut pending,
        ImportPolicy::PendingReview {
            submitted_by_user_id: submitter,
        },
        now,
    );
    assert_eq!(pending.review_status, Set(ChallengeReviewStatus::Pending));
    assert_eq!(pending.submitted_by_user_id, Set(Some(submitter)));
    assert_eq!(pending.submitted_at_utc, Set(Some(now)));

    let mut trusted = <game_challenge::ActiveModel as Default>::default();
    initialize_new_import_review(&mut trusted, ImportPolicy::Trusted, now);
    assert_eq!(trusted.review_status, Set(ChallengeReviewStatus::Active));
    assert_eq!(trusted.submitted_by_user_id, Set(None));
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
    assert!(checkout_usage_exceeds(&root).await.unwrap());
    assert!(validate_checkout_tree(&root).await.is_err());
    let _ = tokio::fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn checkout_tree_quota_includes_git_object_storage() {
    let root = std::env::temp_dir().join(format!("rsctf-git-pack-{}", uuid::Uuid::new_v4()));
    let objects = root.join(".git/objects/pack");
    tokio::fs::create_dir_all(&objects).await.unwrap();
    let pack = tokio::fs::File::create(objects.join("pack-large.pack"))
        .await
        .unwrap();
    pack.set_len(MAX_REPO_TOTAL_BYTES + 1).await.unwrap();

    assert!(checkout_usage_exceeds(&root).await.unwrap());
    assert!(validate_checkout_tree(&root).await.is_err());
    let _ = tokio::fs::remove_dir_all(root).await;
}
