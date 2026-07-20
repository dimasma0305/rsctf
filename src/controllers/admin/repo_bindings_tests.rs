use super::*;

#[test]
fn scan_counts_distinguish_created_and_updated_challenges() {
    let mut counts = ChallengeSyncCounts::default();
    counts.record(crate::services::git_sync::ManifestImportResult {
        challenge_id: 10,
        created: true,
        build_queued: false,
        runtime_update_deferred: false,
        grading_update_deferred: false,
        attachment_synced: true,
    });
    counts.record(crate::services::git_sync::ManifestImportResult {
        challenge_id: 10,
        created: false,
        build_queued: false,
        runtime_update_deferred: false,
        grading_update_deferred: false,
        attachment_synced: true,
    });
    counts.record(crate::services::git_sync::ManifestImportResult {
        challenge_id: 11,
        created: true,
        build_queued: false,
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
