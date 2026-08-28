use super::*;

fn reference() -> ChallengeReference {
    ChallengeReference {
        id: 7,
        title: "recoverable".to_string(),
        challenge_type: ChallengeType::DynamicContainer as i16,
        container_image: Some("docker.io/rsctf/game/app:latest".to_string()),
        ad_checker_image: None,
        original_archive_blob_path: Some("build/source.zip".to_string()),
        build_context_subdir: Some("src".to_string()),
        build_status: ChallengeBuildStatus::Success as i16,
        build_image_digest: Some(format!("sha256:{}", "a".repeat(64))),
        workload_spec: None,
        variant_generator_image: None,
        variant_generator_digest: None,
        variant_generator_build_context_subdir: None,
        variant_generator_build_status: ChallengeBuildStatus::None as i16,
    }
}

fn ownership() -> ImageOwnership {
    ImageOwnership {
        canonical_ref: "docker.io/rsctf/game/app:latest".to_string(),
        image_id: format!("sha256:{}", "a".repeat(64)),
        updated_at_utc: Utc::now(),
        last_used_at_utc: None,
        cleanup_claim_id: None,
        cleanup_claim_expires_at_utc: None,
    }
}

#[test]
fn only_exact_recoverable_jeopardy_sources_are_evictable() {
    let owned = ownership();
    let mut candidate = reference();
    assert!(reference_is_rebuildable(&candidate, &owned));
    candidate.challenge_type = ChallengeType::AttackDefense as i16;
    assert!(!reference_is_rebuildable(&candidate, &owned));
    candidate = reference();
    candidate.original_archive_blob_path = None;
    assert!(!reference_is_rebuildable(&candidate, &owned));
    candidate = reference();
    candidate.ad_checker_image = candidate.container_image.clone();
    assert!(!reference_is_rebuildable(&candidate, &owned));
    candidate = reference();
    candidate.build_image_digest = Some(format!("sha256:{}", "b".repeat(64)));
    assert!(!reference_is_rebuildable(&candidate, &owned));
}

#[test]
fn managed_generators_keep_their_immutable_image_owned() {
    let owned = ownership();
    let digest = owned.image_id.clone();
    let mut candidate = reference();
    candidate.variant_generator_image = Some(digest.clone());
    candidate.variant_generator_digest = Some(digest);
    candidate.variant_generator_build_context_subdir = Some("generator".to_string());
    candidate.variant_generator_build_status = ChallengeBuildStatus::Success as i16;

    assert_eq!(references_for(&[candidate.clone()], &owned).len(), 1);
    assert!(!reference_is_rebuildable(&candidate, &owned));
}

#[test]
fn start_use_replaces_build_time_as_retention_anchor() {
    let built = Utc::now() - ChronoDuration::hours(30);
    let used = Utc::now() - ChronoDuration::hours(2);
    let ownership = ImageOwnership {
        updated_at_utc: built,
        last_used_at_utc: Some(used),
        ..ownership()
    };
    assert_eq!(ownership.retention_anchor(), used);
}

#[test]
fn local_socket_resolution_fails_closed_for_remote_docker() {
    assert!(docker_socket_path_from(Some("tcp://docker.example:2376")).is_none());
    assert_eq!(
        docker_socket_path_from(Some("unix:///run/docker.sock")),
        Some(PathBuf::from("/run/docker.sock"))
    );
}

#[test]
fn storage_status_excludes_cache_bytes_shared_with_images() {
    assert_eq!(
        build_cache_space([
            (Some(100), Some(true), Some(false)),
            (Some(50), Some(false), Some(false)),
            (Some(25), Some(false), Some(true)),
        ]),
        (75, 50),
    );
}

#[test]
fn scheduled_batches_rotate_past_permanently_retained_images() {
    assert!(OWNERSHIPS_AFTER_SQL.contains("canonical_ref > $2"));
    let scheduler = include_str!("scheduler.rs");
    assert!(scheduler.contains("candidate_cursor_ref"));
    assert!(scheduler.contains("report.next_candidate_cursor"));
    let source = include_str!("../image_storage.rs");
    assert!(source.contains("canonical_ref > $2"));
    assert!(source.contains("report.candidate_backlog"));
}

#[test]
fn final_reference_probe_covers_every_managed_docker_hub_alias() {
    assert_eq!(
        canonical_reference_aliases("docker.io/rsctf/event/challenge:latest"),
        vec![
            "docker.io/rsctf/event/challenge".to_string(),
            "docker.io/rsctf/event/challenge:latest".to_string(),
            "index.docker.io/rsctf/event/challenge".to_string(),
            "index.docker.io/rsctf/event/challenge:latest".to_string(),
            "rsctf/event/challenge".to_string(),
            "rsctf/event/challenge:latest".to_string(),
        ]
    );
    assert!(CANDIDATE_REFERENCES_SQL.contains("BTRIM(container_image) = ANY($1)"));
    assert!(CANDIDATE_REFERENCES_SQL.contains("variant_generator_image = $2"));
}

#[test]
fn runtime_reservation_and_cleanup_share_one_durable_claim_fence() {
    let source = include_str!("../image_storage.rs");
    assert!(source.contains("cleanup_claim_id = $4"));
    assert!(source.contains("cleanup_claim_expires_at_utc <= clock_timestamp()"));
    assert!(source.contains("commit_removed_ownership"));
    assert!(source.contains("cleanup_claim_id == Some(claim_id)"));
    let mismatch = source
        .split_once("if !exact_owner {")
        .expect("runtime mismatch branch exists")
        .1;
    let release = mismatch.find("lock.release().await?").unwrap();
    let inspect = mismatch.find("st.containers.image_exists").unwrap();
    assert!(
        release < inspect,
        "runtime image inspection must not retain the PostgreSQL build-fence connection"
    );
}

#[tokio::test]
#[ignore = "requires PostgreSQL, local Docker, and RSCTF_TEST_CONTAINER_IMAGE"]
async fn real_docker_reservation_waits_for_cleanup_claim_publication() {
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let image_id = std::env::var("RSCTF_TEST_CONTAINER_IMAGE")
        .expect("RSCTF_TEST_CONTAINER_IMAGE must be an immutable local image ID");
    assert!(crate::services::challenge_images::is_local_image_id(
        &image_id
    ));
    let docker = connect_local_docker().await.unwrap();
    let inspected = tokio::time::timeout(DOCKER_CALL_BUDGET, docker.inspect_image(&image_id))
        .await
        .expect("real Docker inspect stayed within its deadline")
        .expect("fixture image exists");
    assert_eq!(
        crate::services::challenge_images::inspected_local_image_id(&inspected),
        Some(image_id.as_str())
    );

    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("rsctf_image_claim_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"CREATE TABLE "BuildImageOwnerships" (
             installation_scope TEXT NOT NULL,
             canonical_ref TEXT NOT NULL,
             image_id TEXT NOT NULL,
             updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
             last_used_at_utc TIMESTAMPTZ,
             cleanup_claim_id UUID,
             cleanup_claim_expires_at_utc TIMESTAMPTZ,
             PRIMARY KEY (installation_scope, canonical_ref)
           )"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let scope = "0123456789abcdef0123456789abcdef";
    let canonical_ref = format!("docker.io/rsctf/test/{}:latest", uuid::Uuid::new_v4());
    sqlx::query(
        r#"INSERT INTO "BuildImageOwnerships"
             (installation_scope, canonical_ref, image_id)
           VALUES ($1, $2, $3)"#,
    )
    .bind(scope)
    .bind(&canonical_ref)
    .bind(&image_id)
    .execute(&pool)
    .await
    .unwrap();

    let key = crate::controllers::edit::image_build_lock_key(Some(&canonical_ref));
    let mut cleanup_lock = crate::utils::single_flight::PgAdvisoryLock::acquire_build(&pool, &key)
        .await
        .unwrap();
    let claim_id = uuid::Uuid::new_v4();
    sqlx::query(
        r#"UPDATE "BuildImageOwnerships"
              SET cleanup_claim_id = $3,
                  cleanup_claim_expires_at_utc = clock_timestamp() + INTERVAL '3 minutes'
            WHERE installation_scope = $1 AND canonical_ref = $2"#,
    )
    .bind(scope)
    .bind(&canonical_ref)
    .bind(claim_id)
    .execute(cleanup_lock.connection_mut())
    .await
    .unwrap();

    let contender_pool = pool.clone();
    let contender_key = key.clone();
    let contender_ref = canonical_ref.clone();
    let contender = tokio::spawn(async move {
        let mut lock = crate::utils::single_flight::PgAdvisoryLock::acquire_build(
            &contender_pool,
            &contender_key,
        )
        .await
        .unwrap();
        let claimed: bool = sqlx::query_scalar(
            r#"SELECT cleanup_claim_id IS NOT NULL
                 FROM "BuildImageOwnerships" WHERE canonical_ref = $1"#,
        )
        .bind(contender_ref)
        .fetch_one(lock.connection_mut())
        .await
        .unwrap();
        lock.release().await.unwrap();
        claimed
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !contender.is_finished(),
        "runtime fence must wait for cleanup"
    );
    cleanup_lock.release().await.unwrap();
    assert!(
        contender.await.unwrap(),
        "runtime observes the durable claim"
    );

    tokio::time::timeout(DOCKER_CALL_BUDGET, docker.inspect_image(&image_id))
        .await
        .expect("post-claim Docker inspect stayed bounded")
        .expect("claim publication did not mutate the fixture image");

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
