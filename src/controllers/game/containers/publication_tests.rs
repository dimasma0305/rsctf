use super::*;

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn stable_dynamic_flag_is_reused_only_by_its_original_instance() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("team_flag_restart_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = crate::migrations::test_pg_connect_options(&database_url)
        .options([("search_path", schema.as_str())]);
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"
        CREATE TABLE "FlagContexts" (
            id SERIAL PRIMARY KEY,
            flag TEXT NOT NULL,
            is_occupied BOOLEAN NOT NULL,
            attachment_id INTEGER,
            challenge_id INTEGER,
            exercise_id INTEGER,
            canonical_identity_enforced BOOLEAN NOT NULL DEFAULT TRUE
        );
        CREATE UNIQUE INDEX ux_flag_contexts_challenge_flag
            ON "FlagContexts" (challenge_id, flag)
            WHERE challenge_id IS NOT NULL AND canonical_identity_enforced;
        CREATE TABLE "GameInstances" (
            id SERIAL PRIMARY KEY,
            challenge_id INTEGER NOT NULL,
            participation_id INTEGER NOT NULL,
            is_loaded BOOLEAN NOT NULL,
            last_container_operation TIMESTAMPTZ,
            flag_id INTEGER REFERENCES "FlagContexts"(id),
            container_id UUID
        );
        CREATE TABLE "Containers" (
            id UUID PRIMARY KEY,
            image TEXT NOT NULL,
            container_id TEXT NOT NULL,
            status SMALLINT NOT NULL,
            started_at TIMESTAMPTZ NOT NULL,
            expect_stop_at TIMESTAMPTZ NOT NULL,
            is_proxy BOOLEAN NOT NULL,
            ip TEXT NOT NULL,
            port INTEGER NOT NULL,
            public_ip TEXT,
            public_port INTEGER,
            game_instance_id INTEGER,
            exercise_instance_id INTEGER,
            ad_team_service_id INTEGER
        );
        INSERT INTO "FlagContexts" (id, flag, is_occupied, challenge_id)
            VALUES (10, 'stable-team-flag', TRUE, 719);
        INSERT INTO "GameInstances"
            (id, challenge_id, participation_id, is_loaded, flag_id)
            VALUES (7, 719, 1, FALSE, 10), (8, 719, 2, FALSE, NULL),
                   (9, 719, 3, FALSE, NULL);
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let now = chrono::Utc::now();
    let first_container = uuid::Uuid::new_v4();
    let mut transaction = pool.begin().await.unwrap();
    publish_team_container_locked(
        &mut transaction,
        TeamPublication {
            container_id: first_container,
            backend_id: "first-backend",
            image: "test-image",
            is_proxy: true,
            ip: "127.0.0.1",
            port: 1337,
            participation_id: 1,
            challenge_id: 719,
            existing_instance_id: Some(7),
            dynamic_flag: Some("stable-team-flag"),
            started_at: now,
            expect_stop_at: now + chrono::Duration::hours(1),
        },
    )
    .await
    .unwrap();
    transaction.commit().await.unwrap();
    let linked_flag: i32 =
        sqlx::query_scalar(r#"SELECT flag_id FROM "GameInstances" WHERE id = 7"#)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(linked_flag, 10);

    let mut transaction = pool.begin().await.unwrap();
    let error = publish_team_container_locked(
        &mut transaction,
        TeamPublication {
            container_id: uuid::Uuid::new_v4(),
            backend_id: "other-team-backend",
            image: "test-image",
            is_proxy: true,
            ip: "127.0.0.1",
            port: 1337,
            participation_id: 2,
            challenge_id: 719,
            existing_instance_id: Some(8),
            dynamic_flag: Some("stable-team-flag"),
            started_at: now,
            expect_stop_at: now + chrono::Duration::hours(1),
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(error, AppError::Conflict(_)));
    transaction.rollback().await.unwrap();

    let count: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)::bigint FROM "FlagContexts" WHERE challenge_id = 719"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
    let other_team_flag: Option<i32> =
        sqlx::query_scalar(r#"SELECT flag_id FROM "GameInstances" WHERE id = 8"#)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(other_team_flag.is_none());

    sqlx::query(r#"DELETE FROM "Containers" WHERE id = $1"#)
        .bind(first_container)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"UPDATE "GameInstances" SET container_id = NULL, is_loaded = FALSE WHERE id = 7"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let mut transaction = pool.begin().await.unwrap();
    publish_team_container_locked(
        &mut transaction,
        TeamPublication {
            container_id: uuid::Uuid::new_v4(),
            backend_id: "restarted-backend",
            image: "test-image",
            is_proxy: true,
            ip: "127.0.0.1",
            port: 1337,
            participation_id: 1,
            challenge_id: 719,
            existing_instance_id: Some(7),
            dynamic_flag: Some("stable-team-flag"),
            started_at: now,
            expect_stop_at: now + chrono::Duration::hours(1),
        },
    )
    .await
    .unwrap();
    transaction.commit().await.unwrap();
    let linked_flag: i32 =
        sqlx::query_scalar(r#"SELECT flag_id FROM "GameInstances" WHERE id = 7"#)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(linked_flag, 10);

    // A competing team can start while the first flag insert is uncommitted.
    // It must wait for the unique index conflict, then observe the new owner.
    let mut first = pool.begin().await.unwrap();
    publish_team_container_locked(
        &mut first,
        TeamPublication {
            container_id: uuid::Uuid::new_v4(),
            backend_id: "race-winner",
            image: "test-image",
            is_proxy: true,
            ip: "127.0.0.1",
            port: 1337,
            participation_id: 2,
            challenge_id: 719,
            existing_instance_id: Some(8),
            dynamic_flag: Some("racing-team-flag"),
            started_at: now,
            expect_stop_at: now + chrono::Duration::hours(1),
        },
    )
    .await
    .unwrap();
    let mut second = pool.begin().await.unwrap();
    let mut competing = Box::pin(publish_team_container_locked(
        &mut second,
        TeamPublication {
            container_id: uuid::Uuid::new_v4(),
            backend_id: "race-loser",
            image: "test-image",
            is_proxy: true,
            ip: "127.0.0.1",
            port: 1337,
            participation_id: 3,
            challenge_id: 719,
            existing_instance_id: Some(9),
            dynamic_flag: Some("racing-team-flag"),
            started_at: now,
            expect_stop_at: now + chrono::Duration::hours(1),
        },
    ));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut competing)
            .await
            .is_err(),
        "the competing insert should wait for the first transaction"
    );
    first.commit().await.unwrap();
    assert!(matches!(
        competing.await.unwrap_err(),
        AppError::Conflict(_)
    ));
    second.rollback().await.unwrap();
    let racing_flags: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)::bigint FROM "FlagContexts"
            WHERE challenge_id = 719 AND flag = 'racing-team-flag'"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(racing_flags, 1);

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
