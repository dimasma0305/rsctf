use super::*;

const ID: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

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
        build_image_digest: Some(ID.to_string()),
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
        image_id: ID.to_string(),
        updated_at_utc: Utc::now(),
        last_used_at_utc: None,
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
fn reference_snapshot_indexes_aliases_and_managed_generators_once() {
    let owned = ownership();
    let mut generator = reference();
    generator.container_image = None;
    generator.variant_generator_image = Some(ID.to_string());
    generator.variant_generator_digest = Some(ID.to_string());
    generator.variant_generator_build_context_subdir = Some("generator".to_string());
    generator.variant_generator_build_status = ChallengeBuildStatus::Success as i16;
    let snapshot = ReferenceSnapshot::new(vec![reference(), generator]);

    let matching = snapshot.matching(&owned);
    assert_eq!(matching.len(), 2);
    assert!(matching
        .iter()
        .any(|reference| managed_generator_matches(reference, &owned)));
}

#[test]
fn short_daemon_ids_fail_safe_without_accepting_tiny_prefixes() {
    assert!(image_id_may_match("aaaaaaaaaaaa", ID));
    assert!(image_id_may_match("sha256:aaaaaaaaaaaa", ID));
    assert!(!image_id_may_match("aaaaaaaaaaa", ID));
    assert!(!image_id_may_match("bbbbbbbbbbbb", ID));
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
fn candidate_claim_is_bounded_rotating_and_skip_locked() {
    assert!(CLAIM_CANDIDATES_SQL.contains("FOR UPDATE SKIP LOCKED"));
    assert!(CLAIM_CANDIDATES_SQL.contains("LIMIT $6"));
    assert!(CLAIM_CANDIDATES_SQL.contains("cleanup_checked_at_utc NULLS FIRST"));
    assert!((1..=8).contains(&CLEANUP_CONCURRENCY));
    assert!((1..=64).contains(&CLEANUP_BATCH_SIZE));
    assert!(RENEW_CLAIM_SQL.contains("SET cleanup_claim_until"));
    assert!(RENEW_CLAIM_SQL.contains("cleanup_removal_started = TRUE"));
    assert!(RENEW_CLAIM_SQL.contains("ControlPlaneResourceLeases"));
    assert!(RENEW_CLAIM_SQL.contains("cleanup_claim_until > clock_timestamp()"));
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn cleanup_finalization_waits_for_a_live_durable_build_lease() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(crate::migrations::test_pg_connect_options(&database_url))
        .await
        .unwrap();
    let schema = format!("cleanup_build_lease_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect_with(
            crate::migrations::test_pg_connect_options(&database_url)
                .options([("search_path", schema.as_str())]),
        )
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"CREATE TABLE "BuildImageOwnerships" (
             installation_scope TEXT NOT NULL,
             canonical_ref TEXT NOT NULL,
             image_id TEXT NOT NULL,
             cleanup_claim_token UUID NULL,
             cleanup_claim_until TIMESTAMPTZ NULL,
             cleanup_removal_started BOOLEAN NOT NULL DEFAULT FALSE,
             PRIMARY KEY (installation_scope, canonical_ref)
           );
           CREATE TABLE "ControlPlaneResourceLeases" (
             resource_key TEXT PRIMARY KEY,
             owner_job_id UUID NOT NULL,
             lease_expires_at_utc TIMESTAMPTZ NOT NULL
           )"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let scope = "0123456789abcdef0123456789abcdef";
    let canonical = "docker.io/rsctf/game/building:latest";
    let lock_key = crate::controllers::edit::image_build_lock_key(Some(canonical));
    let claim = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "BuildImageOwnerships"
             (installation_scope, canonical_ref, image_id,
              cleanup_claim_token, cleanup_claim_until)
           VALUES ($1, $2, $3, $4, clock_timestamp() + interval '2 minutes')"#,
    )
    .bind(scope)
    .bind(canonical)
    .bind(ID)
    .bind(claim)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "ControlPlaneResourceLeases"
             (resource_key, owner_job_id, lease_expires_at_utc)
           VALUES ($1, $2, clock_timestamp() + interval '2 minutes')"#,
    )
    .bind(&lock_key)
    .bind(Uuid::new_v4())
    .execute(&pool)
    .await
    .unwrap();

    let blocked = sqlx::query(RENEW_CLAIM_SQL)
        .bind(scope)
        .bind(canonical)
        .bind(ID)
        .bind(claim)
        .bind(CLAIM_LEASE_SECONDS)
        .bind(&lock_key)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(blocked.rows_affected(), 0);
    sqlx::query(
        r#"UPDATE "ControlPlaneResourceLeases"
              SET lease_expires_at_utc = clock_timestamp() - interval '1 second'
            WHERE resource_key = $1"#,
    )
    .bind(&lock_key)
    .execute(&pool)
    .await
    .unwrap();
    let finalized = sqlx::query(RENEW_CLAIM_SQL)
        .bind(scope)
        .bind(canonical)
        .bind(ID)
        .bind(claim)
        .bind(CLAIM_LEASE_SECONDS)
        .bind(&lock_key)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(finalized.rows_affected(), 1);
    let started: bool = sqlx::query_scalar(
        r#"SELECT cleanup_removal_started FROM "BuildImageOwnerships"
            WHERE installation_scope = $1 AND canonical_ref = $2"#,
    )
    .bind(scope)
    .bind(canonical)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(started);

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}

#[tokio::test]
async fn absolute_deadline_cancels_a_hung_daemon_future() {
    let started = tokio::time::Instant::now();
    let result = docker_call::<(), std::io::Error, _>(
        started + Duration::from_millis(20),
        "test operation",
        std::future::pending(),
    )
    .await;
    assert!(result.is_err());
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn dispatched_removal_keeps_its_claim_until_absence_is_committed() {
    let source = include_str!("cleanup.rs");
    let dispatched = source.find("docker.remove_image(").unwrap();
    let committed = source[dispatched..]
        .find("commit_removed_identity")
        .map(|offset| dispatched + offset)
        .unwrap();
    assert!(!source[dispatched..committed].contains("release_claim("));
}

#[test]
fn successful_docker_and_ledger_removal_resolves_one_backlog_row() {
    // `images_removed` is a daemon metric for the same ownership identity;
    // backlog advances only when the one corresponding ledger row commits.
    assert_eq!(remaining_backlog(10, 1), 9);
    assert_eq!(remaining_backlog(1, 1), 0);
    assert_eq!(remaining_backlog(0, 1), 0);
}
