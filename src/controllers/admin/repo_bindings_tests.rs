use super::*;

#[test]
fn scheduler_interval_rejects_tight_loops_and_unbounded_delays() {
    assert!(validate_scan_interval(60).is_ok());
    assert!(validate_scan_interval(86_400).is_ok());
    for invalid in [i32::MIN, -1, 0, 59, 86_401, i32::MAX] {
        assert!(
            validate_scan_interval(invalid).is_err(),
            "accepted {invalid}"
        );
    }
}

#[test]
fn scan_counts_distinguish_created_and_updated_challenges() {
    let mut counts = ChallengeSyncCounts::default();
    counts.record(crate::services::git_sync::ManifestImportResult {
        challenge_id: 10,
        created: true,
        build_queued: false,
        generator_build_queued: false,
        runtime_update_deferred: false,
        grading_update_deferred: false,
        attachment_synced: true,
    });
    counts.record(crate::services::git_sync::ManifestImportResult {
        challenge_id: 10,
        created: false,
        build_queued: false,
        generator_build_queued: false,
        runtime_update_deferred: false,
        grading_update_deferred: false,
        attachment_synced: true,
    });
    counts.record(crate::services::git_sync::ManifestImportResult {
        challenge_id: 11,
        created: true,
        build_queued: false,
        generator_build_queued: false,
        runtime_update_deferred: false,
        grading_update_deferred: false,
        attachment_synced: true,
    });
    assert_eq!(
        counts,
        ChallengeSyncCounts {
            imported: 2,
            updated: 1,
        }
    );
}

#[test]
fn safe_retained_updates_do_not_block_missing_challenge_reconciliation() {
    assert!(missing_challenge_reconciliation_is_safe(0));
    assert!(!missing_challenge_reconciliation_is_safe(1));
}

#[test]
fn event_preflight_rejects_missing_and_nested_event_roots() {
    assert!(validate_event_preflight(
        &["one/.gzevent".into(), "two/.gzevent".into()],
        &["one/.gzevent".into()]
    )
    .is_ok());
    assert!(validate_event_preflight(
        &["parent/.gzevent".into(), "parent/child/.gzevent".into()],
        &[]
    )
    .is_err());
    let missing = validate_event_preflight(
        &["replacement/.gzevent".into()],
        &["existing/.gzevent".into()],
    )
    .unwrap_err()
    .to_string();
    assert!(missing.contains("explicitly migrate, detach, or archive"));
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn repository_game_refresh_rejects_a_pending_hard_delete() {
    use std::str::FromStr;

    use sea_orm::SqlxPostgresConnector;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("repo_game_pending_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::query(
        r#"CREATE TABLE "Games" (
             id INTEGER PRIMARY KEY,
             event_manifest_path TEXT,
             deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
           )"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let game_id = (uuid::Uuid::new_v4().as_u128() % 1_000_000_000) as i32 + 1;
    sqlx::query(
        r#"INSERT INTO "Games" (id, event_manifest_path, deletion_pending)
           VALUES ($1, 'old/.gzevent', TRUE)"#,
    )
    .bind(game_id)
    .execute(&pool)
    .await
    .unwrap();
    let database = SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone());

    let error = update_bound_game_manifest_path(&database, game_id, "new/.gzevent")
        .await
        .expect_err("repository refresh crossed a durable game deletion fence");
    assert_eq!(error.status(), axum::http::StatusCode::CONFLICT);
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            r#"SELECT event_manifest_path FROM "Games" WHERE id = $1"#,
        )
        .bind(game_id)
        .fetch_one(&pool)
        .await
        .unwrap(),
        "old/.gzevent"
    );

    sqlx::query(r#"UPDATE "Games" SET deletion_pending = FALSE WHERE id = $1"#)
        .bind(game_id)
        .execute(&pool)
        .await
        .unwrap();
    update_bound_game_manifest_path(&database, game_id, "new/.gzevent")
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            r#"SELECT event_manifest_path FROM "Games" WHERE id = $1"#,
        )
        .bind(game_id)
        .fetch_one(&pool)
        .await
        .unwrap(),
        "new/.gzevent"
    );

    drop(database);
    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn scan_and_push_leases_are_single_owner_across_claimers() {
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("repo_claims_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect_with(options.clone())
        .await
        .unwrap();
    let second_pool = PgPoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"CREATE TABLE "RepoBindings" (
             id INTEGER PRIMARY KEY, status SMALLINT NOT NULL,
             repo_url TEXT NOT NULL,
             next_scan_utc TIMESTAMPTZ,
             scan_lease_token UUID, scan_lease_until TIMESTAMPTZ,
             scan_started_at_utc TIMESTAMPTZ,
             scan_host_key TEXT, scan_slot SMALLINT,
             consecutive_scan_failures INTEGER NOT NULL DEFAULT 0,
             last_scan_message TEXT,
             push_lease_token UUID, push_lease_until TIMESTAMPTZ
           );
           CREATE UNIQUE INDEX scan_slot_unique
             ON "RepoBindings" (scan_slot) WHERE scan_slot IS NOT NULL;
           CREATE UNIQUE INDEX scan_host_unique
             ON "RepoBindings" (scan_host_key) WHERE scan_host_key IS NOT NULL;
           CREATE TABLE "RepoBindingPushJobs" (
             binding_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
             game_id INTEGER NOT NULL, requested_revision BIGINT NOT NULL,
             attempts INTEGER NOT NULL DEFAULT 0, last_error TEXT,
             updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
             PRIMARY KEY (binding_id, challenge_id)
           );
           INSERT INTO "RepoBindings" (id, status, repo_url, next_scan_utc)
             VALUES
               (1, 0, 'https://github.com/one/repo', clock_timestamp() - INTERVAL '4 seconds'),
               (2, 0, 'https://GITHUB.com/two/repo', clock_timestamp() - INTERVAL '3 seconds'),
               (3, 0, 'https://gitlab.com/three/repo', clock_timestamp() - INTERVAL '2 seconds'),
               (4, 1, 'https://codeberg.org/four/repo', clock_timestamp() - INTERVAL '1 second');
           INSERT INTO "RepoBindingPushJobs"
             (binding_id, challenge_id, game_id, requested_revision)
             VALUES (1, 10, 20, 3), (1, 11, 20, 7);"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let (first_replica, second_replica) = tokio::join!(
        claim_repo_scan(&pool, None, 4),
        claim_repo_scan(&second_pool, None, 4)
    );
    let mut scan = first_replica.unwrap();
    scan.extend(second_replica.unwrap());
    scan.sort_unstable_by_key(|claim| claim.0);
    assert_eq!(
        scan.iter().map(|claim| claim.0).collect::<Vec<_>>(),
        vec![1, 3],
        "same-host binding #2 must not consume a second deployment slot"
    );
    assert!(claim_repo_scan(&second_pool, None, 4)
        .await
        .unwrap()
        .is_empty());
    let (active, slots, hosts): (i64, i64, i64) = sqlx::query_as(
        r#"SELECT COUNT(*)::BIGINT,
                  COUNT(DISTINCT scan_slot)::BIGINT,
                  COUNT(DISTINCT scan_host_key)::BIGINT
             FROM "RepoBindings" WHERE scan_lease_token IS NOT NULL"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((active, slots, hosts), (2, 2, 2));

    let first_token = scan[0].1;
    assert!(
        super::scheduler::renew_repo_scan_lease(&second_pool, 1, first_token)
            .await
            .unwrap()
    );
    assert!(
        !super::scheduler::renew_repo_scan_lease(&second_pool, 1, Uuid::new_v4())
            .await
            .unwrap()
    );
    sqlx::query(
        r#"UPDATE "RepoBindings"
              SET next_scan_utc = clock_timestamp() + INTERVAL '1 hour'
            WHERE id = 1"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    super::scheduler::finish_repo_scan_lease(&pool, 1, first_token, true, None)
        .await
        .unwrap();
    let same_host = claim_repo_scan(&second_pool, None, 1).await.unwrap();
    assert_eq!(same_host.len(), 1);
    assert_eq!(same_host[0].0, 2);

    // A crashed owner stops renewing. Database-time expiry releases both its
    // global and host slots, and a different replica can reclaim exact work.
    sqlx::query(
        r#"UPDATE "RepoBindings"
              SET scan_lease_until = clock_timestamp() - INTERVAL '1 second'
            WHERE id = 3"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let recovered = claim_repo_scan(&second_pool, None, 2).await.unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].0, 3);
    super::scheduler::finish_repo_scan_lease(
        &second_pool,
        3,
        recovered[0].1,
        false,
        Some("upstream unavailable"),
    )
    .await
    .unwrap();
    let (failures, backed_off, message): (i32, bool, Option<String>) = sqlx::query_as(
        r#"SELECT consecutive_scan_failures,
                  next_scan_utc > clock_timestamp(), last_scan_message
             FROM "RepoBindings" WHERE id = 3"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(failures, 1);
    assert!(backed_off);
    assert_eq!(message.as_deref(), Some("upstream unavailable"));

    // Manual scans may claim a paused binding, but they still obey the same
    // deployment-wide slot and per-host ownership contract.
    super::scheduler::finish_repo_scan_lease(&pool, 2, same_host[0].1, true, None)
        .await
        .unwrap();
    let paused = claim_repo_scan(&second_pool, Some(4), 1).await.unwrap();
    assert_eq!(paused.len(), 1);
    assert_eq!(paused[0].0, 4);

    let push = crate::controllers::edit::claim_repo_push_jobs(&pool, 1)
        .await
        .unwrap();
    assert_eq!(push.len(), 1);
    assert_eq!(push[0].binding_id, 1);
    assert!(crate::controllers::edit::claim_repo_push_jobs(&pool, 1)
        .await
        .unwrap()
        .is_empty());

    second_pool.close().await;
    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
