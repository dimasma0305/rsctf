use super::*;

#[test]
fn manual_reference_scan_runs_after_the_short_image_lock_is_released() {
    let source = include_str!("images.rs");
    let removal = source.find("async fn remove_one(").unwrap();
    let removal = &source[removal..];
    let claim = removal.find("sqlx::query(CLAIM_MANUAL_REMOVAL_SQL)").unwrap();
    let release = removal[claim..]
        .find("let released = lock.release().await")
        .map(|offset| claim + offset)
        .unwrap();
    let references = removal
        .find("sqlx::query_as::<_, ReferenceRow>(REFERENCES_SQL)")
        .unwrap();

    assert!(claim < release && release < references);
    assert!(!removal[..release].contains("ReferenceRow>(REFERENCES_SQL)"));
    assert!(removal[..release].contains("CLAIM_MANUAL_REMOVAL_SQL"));
}

#[test]
fn dispatched_manual_removal_keeps_its_claim_until_absence_is_committed() {
    let source = include_str!("images.rs");
    let dispatched = source.find("docker.remove_image(").unwrap();
    let committed = source[dispatched..]
        .find("commit_manual_removal")
        .map(|offset| dispatched + offset)
        .unwrap();
    assert!(!source[dispatched..committed].contains("release_manual_claim("));
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn manual_cleanup_finalization_waits_for_a_live_durable_build_lease() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(crate::migrations::test_pg_connect_options(&database_url))
        .await
        .unwrap();
    let schema = format!("manual_cleanup_build_{}", Uuid::new_v4().simple());
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
    let canonical = "docker.io/rsctf/game/manual:latest";
    let image_id = format!("sha256:{}", "a".repeat(64));
    let lock_key = crate::controllers::edit::image_build_lock_key(Some(canonical));
    let claim = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "BuildImageOwnerships"
             (installation_scope, canonical_ref, image_id)
           VALUES ($1, $2, $3)"#,
    )
    .bind(scope)
    .bind(canonical)
    .bind(&image_id)
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

    let blocked_claim = sqlx::query(CLAIM_MANUAL_REMOVAL_SQL)
        .bind(scope)
        .bind(canonical)
        .bind(&image_id)
        .bind(claim)
        .bind(MANUAL_CLAIM_SECONDS)
        .bind(&lock_key)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(blocked_claim.rows_affected(), 0);
    sqlx::query(
        r#"UPDATE "ControlPlaneResourceLeases"
              SET lease_expires_at_utc = clock_timestamp() - interval '1 second'
            WHERE resource_key = $1"#,
    )
    .bind(&lock_key)
    .execute(&pool)
    .await
    .unwrap();
    let claimed = sqlx::query(CLAIM_MANUAL_REMOVAL_SQL)
        .bind(scope)
        .bind(canonical)
        .bind(&image_id)
        .bind(claim)
        .bind(MANUAL_CLAIM_SECONDS)
        .bind(&lock_key)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(claimed.rows_affected(), 1);
    let claim_state: (Option<Uuid>, bool) = sqlx::query_as(
        r#"SELECT cleanup_claim_token, cleanup_removal_started
             FROM "BuildImageOwnerships"
            WHERE installation_scope = $1 AND canonical_ref = $2"#,
    )
    .bind(scope)
    .bind(canonical)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(claim_state, (Some(claim), true));
    sqlx::query(
        r#"UPDATE "ControlPlaneResourceLeases"
              SET lease_expires_at_utc = clock_timestamp() + interval '2 minutes'
            WHERE resource_key = $1"#,
    )
    .bind(&lock_key)
    .execute(&pool)
    .await
    .unwrap();

    let blocked = sqlx::query(FINALIZE_MANUAL_CLAIM_SQL)
        .bind(scope)
        .bind(canonical)
        .bind(&image_id)
        .bind(claim)
        .bind(MANUAL_CLAIM_SECONDS)
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
    let finalized = sqlx::query(FINALIZE_MANUAL_CLAIM_SQL)
        .bind(scope)
        .bind(canonical)
        .bind(&image_id)
        .bind(claim)
        .bind(MANUAL_CLAIM_SECONDS)
        .bind(&lock_key)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(finalized.rows_affected(), 1);

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
