use super::*;

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
    let (first_id, first_token) = first.unwrap().unwrap();
    let (second_id, second_token) = second.unwrap().unwrap();
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
