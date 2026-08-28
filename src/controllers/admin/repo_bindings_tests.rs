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
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"CREATE TABLE "RepoBindings" (
             id INTEGER PRIMARY KEY, status SMALLINT NOT NULL,
             next_scan_utc TIMESTAMPTZ,
             scan_lease_token UUID, scan_lease_until TIMESTAMPTZ,
             scan_started_at_utc TIMESTAMPTZ,
             push_lease_token UUID, push_lease_until TIMESTAMPTZ
           );
           CREATE TABLE "RepoBindingPushJobs" (
             binding_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
             game_id INTEGER NOT NULL, requested_revision BIGINT NOT NULL,
             attempts INTEGER NOT NULL DEFAULT 0, last_error TEXT,
             updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
             PRIMARY KEY (binding_id, challenge_id)
           );
           INSERT INTO "RepoBindings" (id, status, next_scan_utc)
             VALUES (1, 0, clock_timestamp() - INTERVAL '1 second');
           INSERT INTO "RepoBindingPushJobs"
             (binding_id, challenge_id, game_id, requested_revision)
             VALUES (1, 10, 20, 3), (1, 11, 20, 7);"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let scan = claim_repo_scan(&pool, None, 1).await.unwrap();
    assert_eq!(scan.len(), 1);
    assert!(claim_repo_scan(&pool, None, 1).await.unwrap().is_empty());
    let push = crate::controllers::edit::claim_repo_push_jobs(&pool, 1)
        .await
        .unwrap();
    assert_eq!(push.len(), 1);
    assert_eq!(push[0].binding_id, 1);
    assert!(crate::controllers::edit::claim_repo_push_jobs(&pool, 1)
        .await
        .unwrap()
        .is_empty());

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
