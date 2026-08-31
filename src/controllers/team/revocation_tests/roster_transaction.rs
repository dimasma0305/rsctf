use super::*;

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn roster_removal_stays_invisible_until_teardown_lock_commits() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect test database");
    let schema = format!("rsctf_roster_teardown_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin_pool)
        .await
        .expect("create isolated test schema");
    let options = PgConnectOptions::from_str(&database_url)
        .expect("parse test database URL")
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect_with(options)
        .await
        .expect("connect isolated test pool");
    sqlx::raw_sql(
        r#"
        CREATE TABLE "TeamMembers" (
          team_id INTEGER NOT NULL,
          user_id UUID NOT NULL
        );
        CREATE TABLE "Participations" (
          id INTEGER PRIMARY KEY,
          status SMALLINT NOT NULL,
          game_id INTEGER NOT NULL
        );
        CREATE TABLE "Games" (id INTEGER PRIMARY KEY, end_time_utc TIMESTAMPTZ NOT NULL);
        CREATE TABLE "UserParticipations" (
          team_id INTEGER NOT NULL,
          user_id UUID NOT NULL,
          participation_id INTEGER NOT NULL
        );
        "#,
    )
    .execute(&pool)
    .await
    .expect("create roster fixture tables");
    let user_id = uuid::Uuid::new_v4();
    sqlx::query(r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES (9, $1)"#)
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "UserParticipations" (team_id, user_id, participation_id)
           VALUES (9, $1, 99)"#,
    )
    .bind(user_id)
    .execute(&pool)
    .await
    .unwrap();

    let failed_attempt = acquire_roster_mutation(&pool, 9).await.unwrap();
    drop(failed_attempt);
    let visible_after_failure: i64 =
        sqlx::query_scalar(r#"SELECT COUNT(*) FROM "TeamMembers" WHERE user_id = $1"#)
            .bind(user_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(visible_after_failure, 1);

    let mut roster = acquire_roster_mutation(&pool, 9).await.unwrap();
    let mut issuer = tokio::spawn({
        let pool = pool.clone();
        async move { crate::utils::single_flight::PgAdvisoryLock::acquire(&pool, "team-roster:9").await }
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut issuer)
            .await
            .is_err(),
        "credential issuer entered during retained teardown lock"
    );
    let visible_before: i64 =
        sqlx::query_scalar(r#"SELECT COUNT(*) FROM "TeamMembers" WHERE user_id = $1"#)
            .bind(user_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        visible_before, 1,
        "membership vanished before teardown succeeded"
    );

    remove_membership(roster.transaction_mut(), 9, user_id)
        .await
        .unwrap();
    let visible_uncommitted: i64 =
        sqlx::query_scalar(r#"SELECT COUNT(*) FROM "TeamMembers" WHERE user_id = $1"#)
            .bind(user_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        visible_uncommitted, 1,
        "membership deletion leaked before commit"
    );
    roster.release().await.unwrap();

    let acquired = tokio::time::timeout(std::time::Duration::from_secs(2), issuer)
        .await
        .expect("issuer remained blocked after roster commit")
        .expect("issuer task failed")
        .expect("issuer lock failed");
    acquired.release().await.unwrap();
    for table in ["TeamMembers", "UserParticipations"] {
        let remaining: i64 = sqlx::query_scalar(&format!(
            r#"SELECT COUNT(*) FROM "{table}" WHERE user_id = $1"#
        ))
        .bind(user_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(remaining, 0, "{table} did not commit atomically");
    }

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin_pool)
        .await
        .expect("drop isolated test schema");
}
