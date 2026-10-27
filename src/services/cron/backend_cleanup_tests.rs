use super::*;

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn extension_and_cleanup_are_ordered_and_failed_teardown_remains_fenced() {
    let db = crate::utils::test_database::TestDatabase::new("cleanup_extension").await;
    sqlx::raw_sql(
        r#"CREATE TABLE "Games" (id INTEGER PRIMARY KEY, end_time_utc TIMESTAMPTZ NOT NULL);
        CREATE TABLE "AdTeamServices" (id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
            container_id TEXT, host TEXT NOT NULL DEFAULT '');
        CREATE TABLE "KothTargets" (LIKE "AdTeamServices" INCLUDING ALL);
        INSERT INTO "Games" VALUES (1, now() - interval '1 hour'), (2, now() - interval '1 hour');
        INSERT INTO "AdTeamServices" VALUES (1, 1, 'runtime-one', '10.0.0.1');
    "#,
    )
    .execute(&db.pool)
    .await
    .unwrap();
    for _ in 0..2 {
        sqlx::raw_sql(crate::migrations::m0360_backend_cleanup_claims::UP_SQL)
            .execute(&db.pool)
            .await
            .unwrap();
        sqlx::raw_sql(crate::migrations::m0362_pending_backend_cleanup::UP_SQL)
            .execute(&db.pool)
            .await
            .unwrap();
    }

    // An extension already holding its event row wins. The old claim query
    // locked only the service and could admit teardown against the old window.
    let mut extension = db.pool.begin().await.unwrap();
    sqlx::query(r#"SELECT id FROM "Games" WHERE id = 1 FOR UPDATE"#)
        .execute(&mut *extension)
        .await
        .unwrap();
    ensure_extension_safe(&mut extension, 1).await.unwrap();
    sqlx::query(r#"UPDATE "Games" SET end_time_utc = now() + interval '1 hour' WHERE id = 1"#)
        .execute(&mut *extension)
        .await
        .unwrap();
    assert!(claim(&db.pool, Kind::Ad).await.unwrap().is_none());
    extension.commit().await.unwrap();
    assert!(claim(&db.pool, Kind::Ad).await.unwrap().is_none());
    sqlx::query(r#"UPDATE "Games" SET end_time_utc = now() - interval '1 hour' WHERE id = 1"#)
        .execute(&db.pool)
        .await
        .unwrap();

    let (id, first) = claim(&db.pool, Kind::Ad).await.unwrap().unwrap();
    let mut extension = db.pool.begin().await.unwrap();
    sqlx::query(r#"SELECT id FROM "Games" WHERE id = 1 FOR UPDATE"#)
        .execute(&mut *extension)
        .await
        .unwrap();
    assert!(matches!(
        ensure_extension_safe(&mut extension, 1).await,
        Err(crate::utils::error::AppError::Conflict(_))
    ));
    ensure_extension_safe(&mut extension, 2).await.unwrap();
    extension.rollback().await.unwrap();

    assert!(finish(&db.pool, Kind::Ad, id, first, false).await.unwrap());
    let retained: (Option<Uuid>, Option<String>) = sqlx::query_as(
        r#"SELECT cleanup_lease_token, container_id FROM "AdTeamServices" WHERE id = $1"#,
    )
    .bind(id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(retained, (Some(first), Some("runtime-one".into())));
    sqlx::query(r#"UPDATE "AdTeamServices"
        SET cleanup_lease_until = now() - interval '1 second', cleanup_next_at = '-infinity' WHERE id = $1"#)
        .bind(id).execute(&db.pool).await.unwrap();
    let mut connection = db.pool.acquire().await.unwrap();
    assert!(
        ensure_extension_safe(&mut connection, 1).await.is_err(),
        "expiry admits a recovering executor, not an event extension"
    );
    drop(connection);
    let (_, recovered) = claim(&db.pool, Kind::Ad).await.unwrap().unwrap();
    assert_ne!(recovered, first);
    assert!(!finish(&db.pool, Kind::Ad, id, first, true).await.unwrap());

    // Destruction and identity withdrawal committed, then the process died
    // before completing its cleanup claim. This row must remain discoverable.
    sqlx::query(r#"UPDATE "AdTeamServices" SET container_id = NULL, host = '',
        cleanup_lease_until = now() - interval '1 second', cleanup_next_at = '-infinity' WHERE id = $1"#)
        .bind(id).execute(&db.pool).await.unwrap();
    let (_, after_crash) = claim(&db.pool, Kind::Ad).await.unwrap().unwrap();
    assert!(!finish(&db.pool, Kind::Ad, id, recovered, true)
        .await
        .unwrap());
    assert!(finish(&db.pool, Kind::Ad, id, after_crash, false)
        .await
        .unwrap());
    let mut connection = db.pool.acquire().await.unwrap();
    ensure_extension_safe(&mut connection, 1).await.unwrap();
    drop(connection);

    sqlx::query(
        r#"INSERT INTO "KothTargets" (id, game_id, container_id, host)
        VALUES (9, 1, NULL, '10.0.0.9')"#,
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let (id, token) = claim(&db.pool, Kind::Koth).await.unwrap().unwrap();
    let mut connection = db.pool.acquire().await.unwrap();
    assert!(ensure_extension_safe(&mut connection, 1).await.is_err());
    drop(connection);
    sqlx::query(r#"UPDATE "KothTargets" SET host = '' WHERE id = $1"#)
        .bind(id)
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(finish(&db.pool, Kind::Koth, id, token, true).await.unwrap());
    let mut connection = db.pool.acquire().await.unwrap();
    ensure_extension_safe(&mut connection, 1).await.unwrap();
    drop(connection);
    db.close().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn cleanup_is_bounded_fair_and_fenced_across_restarts_and_extensions() {
    let db = crate::utils::test_database::TestDatabase::new("backend_cleanup").await;
    sqlx::raw_sql(r#"CREATE TABLE "Games" (id INTEGER PRIMARY KEY, end_time_utc TIMESTAMPTZ NOT NULL);
        CREATE TABLE "AdTeamServices" (id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
            container_id TEXT, host TEXT NOT NULL DEFAULT '');
        CREATE TABLE "KothTargets" (LIKE "AdTeamServices" INCLUDING ALL);
        INSERT INTO "Games" VALUES (1, now() - interval '1 hour'), (2, now() + interval '1 hour');
        INSERT INTO "AdTeamServices" SELECT n, 1, 'backend-' || n, '' FROM generate_series(1, 2000) n;
        INSERT INTO "KothTargets" VALUES (1, 2, 'future-backend', '');"#)
        .execute(&db.pool).await.unwrap();
    for _ in 0..2 {
        sqlx::raw_sql(crate::migrations::m0360_backend_cleanup_claims::UP_SQL)
            .execute(&db.pool)
            .await
            .unwrap();
    }
    let (first, second) = tokio::join!(claim(&db.pool, Kind::Ad), claim(&db.pool, Kind::Ad));
    // Claims briefly share the parent event lock with schedule edits. A
    // concurrent claimant may skip that locked event and retry next pass.
    let first = match first.unwrap() {
        Some(claimed) => claimed,
        None => claim(&db.pool, Kind::Ad).await.unwrap().unwrap(),
    };
    let second = match second.unwrap() {
        Some(claimed) => claimed,
        None => claim(&db.pool, Kind::Ad).await.unwrap().unwrap(),
    };
    let (first_id, first_token) = first;
    let (second_id, second_token) = second;
    assert_ne!(first_id, second_id);
    assert!(claim(&db.pool, Kind::Koth).await.unwrap().is_none());
    assert!(finish(&db.pool, Kind::Ad, first_id, first_token, false)
        .await
        .unwrap());
    let third = claim(&db.pool, Kind::Ad).await.unwrap().unwrap();
    assert_ne!(
        third.0, first_id,
        "failed early rows must not starve later backends"
    );
    sqlx::query(
        r#"UPDATE "AdTeamServices" SET cleanup_lease_until = now() - interval '1 second'
        WHERE id = $1"#,
    )
    .bind(second_id)
    .execute(&db.pool)
    .await
    .unwrap();
    let recovered = claim(&db.pool, Kind::Ad).await.unwrap().unwrap();
    assert_eq!(recovered.0, second_id);
    assert_ne!(recovered.1, second_token);
    assert!(!finish(&db.pool, Kind::Ad, second_id, second_token, true)
        .await
        .unwrap());
    sqlx::query(r#"UPDATE "Games" SET end_time_utc = now() + interval '1 hour' WHERE id = 1"#)
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(claim(&db.pool, Kind::Ad).await.unwrap().is_none());
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            r#"SELECT count(*) FROM "AdTeamServices" WHERE container_id IS NOT NULL"#
        )
        .fetch_one(&db.pool)
        .await
        .unwrap(),
        2000,
        "claim and retry must preserve runtime identity"
    );
    db.close().await;
}
