use sqlx::postgres::PgPoolOptions;

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn applying_fences_other_writers_and_retains_auditable_rounds() {
    let url = std::env::var("RSCTF_TEST_DATABASE_URL").unwrap();
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("maintenance_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect_with(
            crate::migrations::test_pg_connect_options(&url)
                .options([("search_path", schema.as_str())]),
        )
        .await
        .unwrap();
    sqlx::raw_sql(r#"
      CREATE TABLE "Games" (id INTEGER PRIMARY KEY, ad_scoring_paused BOOLEAN NOT NULL,
        ad_scoring_start_round INTEGER, koth_scoring_start_round INTEGER);
      CREATE TABLE "GameChallenges" (id INTEGER PRIMARY KEY, game_id INTEGER, "Type" SMALLINT DEFAULT 5,
        title TEXT, content TEXT, category SMALLINT, hints JSONB, released_hint_count INTEGER,
        accepted_count INTEGER, submission_count INTEGER, revision BIGINT, last_build_log TEXT,
        container_image TEXT, build_image_digest TEXT, build_status SMALLINT, ad_checker_image TEXT,
        original_archive_blob_path TEXT, build_context_subdir TEXT,
        workload_spec JSONB, runtime_profile TEXT, memory_limit INTEGER, cpu_count INTEGER,
        storage_limit INTEGER, network_mode SMALLINT, expose_port INTEGER, ad_allow_egress BOOLEAN,
        ad_self_hosted BOOLEAN, is_enabled BOOLEAN, enable_traffic_capture BOOLEAN,
        ad_allow_self_reset BOOLEAN, ad_ssh_requires_flag BOOLEAN);
      CREATE TABLE "AdRounds" (game_id INTEGER, number INTEGER);
      CREATE TABLE "KothApiObservers" (game_id INTEGER, challenge_id INTEGER);
      CREATE TABLE "KothApiObserverRevisions" (game_id INTEGER, challenge_id INTEGER, revision BIGINT);
      INSERT INTO "Games" VALUES (7, TRUE, NULL, 1);
      INSERT INTO "GameChallenges" (id, game_id, title, memory_limit, is_enabled)
        VALUES (9,7,'Hill',64,TRUE);
      INSERT INTO "AdRounds" VALUES (7,24);
    "#).execute(&pool).await.unwrap();
    for _ in 0..2 {
        sqlx::raw_sql(crate::migrations::m0266_competition_maintenance::UP_SQL)
            .execute(&pool)
            .await
            .unwrap();
    }
    let actor = uuid::Uuid::new_v4();
    let first = uuid::Uuid::new_v4();
    let second = uuid::Uuid::new_v4();
    let insert = |operation| {
        sqlx::query(
            r#"INSERT INTO "CompetitionMaintenanceWindows"
        (id, game_id, actor_id, state) VALUES ($1,7,$2,'Requested') ON CONFLICT DO NOTHING"#,
        )
        .bind(operation)
        .bind(actor)
        .execute(&pool)
    };
    let (a, b) = tokio::join!(insert(first), insert(second));
    assert_eq!(a.unwrap().rows_affected() + b.unwrap().rows_affected(), 1);
    sqlx::query(
        r#"UPDATE "CompetitionMaintenanceWindows" SET state='Editing',
        effective_from_round=25, before_snapshot='[]' WHERE game_id=7"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let mut connection = pool.acquire().await.unwrap();
    assert!(super::editing_allowed(&mut connection, 7).await.unwrap());
    assert!(
        sqlx::query(r#"UPDATE "Games" SET ad_scoring_paused=FALSE WHERE id=7"#)
            .execute(&pool)
            .await
            .is_err()
    );
    sqlx::query(
        r#"UPDATE "CompetitionMaintenanceWindows" SET state='Applying', after_snapshot='[]'
        WHERE game_id=7"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(!super::editing_allowed(&mut connection, 7).await.unwrap());
    assert!(
        sqlx::query(r#"UPDATE "GameChallenges" SET memory_limit=128 WHERE id=9"#)
            .execute(&pool)
            .await
            .is_err()
    );
    sqlx::query(r#"UPDATE "GameChallenges" SET title='Fixed typo' WHERE id=9"#)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        sqlx::query(r#"INSERT INTO "KothApiObservers" VALUES (7,9)"#)
            .execute(&pool)
            .await
            .is_err()
    );
    let mut applying = pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('rsctf.maintenance_writer','on',true)")
        .execute(&mut *applying)
        .await
        .unwrap();
    sqlx::query(r#"UPDATE "GameChallenges" SET memory_limit=128 WHERE id=9"#)
        .execute(&mut *applying)
        .await
        .unwrap();
    sqlx::query(r#"UPDATE "CompetitionMaintenanceWindows" SET state='Completed' WHERE game_id=7"#)
        .execute(&mut *applying)
        .await
        .unwrap();
    sqlx::query(r#"UPDATE "Games" SET ad_scoring_paused=FALSE WHERE id=7"#)
        .execute(&mut *applying)
        .await
        .unwrap();
    applying.commit().await.unwrap();
    let round: i32 = sqlx::query_scalar(r#"SELECT number FROM "AdRounds" WHERE game_id=7"#)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(round, 24);
    let activation: i32 = sqlx::query_scalar(
        r#"SELECT effective_from_round FROM "CompetitionMaintenanceWindows" WHERE game_id=7"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(activation, 25);
    drop(connection);
    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
}
