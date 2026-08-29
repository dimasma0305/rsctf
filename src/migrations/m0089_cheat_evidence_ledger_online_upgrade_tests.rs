#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn online_upgrade_drains_old_submit_before_locking_children() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let application_name = test_process_application_name();
    let admin_options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .application_name(application_name);
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(admin_options)
        .await
        .unwrap();
    let schema = format!("rsctf_m0089_lock_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .application_name(application_name)
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"
        CREATE TABLE "Games" (id INTEGER PRIMARY KEY);
        CREATE TABLE "Teams" (id INTEGER PRIMARY KEY);
        CREATE TABLE "AspNetUsers" (id INTEGER PRIMARY KEY);
        CREATE TABLE "Participations" (id INTEGER PRIMARY KEY);
        CREATE TABLE "GameChallenges" (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
        CREATE TABLE "GameInstances" (id INTEGER PRIMARY KEY);
        CREATE TABLE "FlagContexts" (id INTEGER PRIMARY KEY);
        CREATE TABLE "GameEvents" (id INTEGER PRIMARY KEY);
        CREATE TABLE "Submissions" (id INTEGER PRIMARY KEY);
        CREATE TABLE "FirstSolves" (id INTEGER PRIMARY KEY);
        CREATE TABLE "CheatInfo" (id INTEGER PRIMARY KEY);
        CREATE TABLE "ContainerAccessEvents" (id INTEGER PRIMARY KEY);
        CREATE TABLE "HoneypotHits" (id INTEGER PRIMARY KEY);
        CREATE TABLE "SuspicionEvents" (id INTEGER PRIMARY KEY);
        INSERT INTO "Games" (id) VALUES (1);
        INSERT INTO "Participations" (id) VALUES (1);
        INSERT INTO "GameChallenges" (id, revision) VALUES (1, 0);
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let old_options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .application_name("legacy-rsctf-web")
        .options([("search_path", schema.as_str())]);
    let old_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(old_options)
        .await
        .unwrap();
    let guard_end = UP_SQL
        .find("-- Drain in-flight gameplay writers")
        .expect("exclusive guard marker");
    let guard_error = sqlx::raw_sql(&UP_SQL[..guard_end])
        .execute(&pool)
        .await
        .expect_err("a legacy client must block m0089 before DDL");
    assert!(guard_error
        .to_string()
        .contains("exclusive schema cutover refused"));
    old_pool.close().await;

    // Model an already-running old submit: it owns Games/Participations row
    // locks and has inserted its child row, but still needs its late
    // GameChallenges counter update before commit.
    let mut old_submit = pool.begin().await.unwrap();
    sqlx::query(r#"SELECT id FROM "Games" WHERE id = 1 FOR SHARE"#)
        .execute(&mut *old_submit)
        .await
        .unwrap();
    sqlx::query(r#"SELECT id FROM "Participations" WHERE id = 1 FOR SHARE"#)
        .execute(&mut *old_submit)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "Submissions" (id) VALUES (1)"#)
        .execute(&mut *old_submit)
        .await
        .unwrap();

    let lock_end = UP_SQL
        .find("-- A manual idempotence/recovery rerun")
        .expect("lock prelude marker");
    let lock_sql = UP_SQL[..lock_end].to_owned();
    let migration_pool = pool.clone();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let migration = async move {
        let mut transaction = migration_pool.begin().await.unwrap();
        sqlx::query("SET LOCAL lock_timeout = '3s'")
            .execute(&mut *transaction)
            .await
            .unwrap();
        started_tx.send(()).unwrap();
        sqlx::raw_sql(&lock_sql)
            .execute(&mut *transaction)
            .await
            .unwrap();
        transaction.rollback().await.unwrap();
    };
    let finish_old_submit = async move {
        started_rx.await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            sqlx::query(r#"UPDATE "GameChallenges" SET revision = revision + 1 WHERE id = 1"#)
                .execute(&mut *old_submit),
        )
        .await
        .expect("migration must not hold a child lock while waiting for Games")
        .unwrap();
        old_submit.commit().await.unwrap();
    };
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        tokio::join!(migration, finish_old_submit)
    })
    .await
    .expect("migration lock drain completes after old submit");

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
}
