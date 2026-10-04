#[test]
fn topology_recovery_is_durable_before_runtime_teardown() {
    let source = include_str!("mod.rs");
    let transition = source
        .find("topology_transition::begin")
        .expect("durable transition begin");
    let teardown = source
        .find("destroy_challenge_containers(&st, &challenge, true, true)")
        .expect("external teardown");
    let definition_commit = source
        .find("let updated = definition_write::update")
        .expect("revisioned definition write");
    let transition_complete = source
        .find("topology_transition::complete")
        .expect("transition completion");

    assert!(transition < teardown);
    assert!(teardown < definition_commit);
    assert!(definition_commit < transition_complete);
}

#[test]
fn stale_revision_is_rejected_before_any_topology_teardown() {
    let source = include_str!("mod.rs");
    let stale_check = source
        .find("challenge.revision != model.expected_revision")
        .expect("early stale revision check");
    let teardown = source
        .find("destroy_challenge_containers(&st, &challenge, true, true)")
        .expect("external teardown");
    assert!(stale_check < teardown);
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn postgres_create_seed_replay_and_revision_cas_are_one_boundary() {
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use std::str::FromStr;
    use uuid::Uuid;

    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("challenge_revision_{}", Uuid::new_v4().simple());
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
        r#"CREATE TABLE "AspNetUsers" (id UUID PRIMARY KEY);
           CREATE TABLE "MutationOperations" (
             actor_id UUID NOT NULL REFERENCES "AspNetUsers"(id) ON DELETE CASCADE,
             resource_kind TEXT NOT NULL,
             scope_key TEXT NOT NULL,
             operation_id UUID NOT NULL,
             request_fingerprint BYTEA NOT NULL,
             result_id TEXT,
             result_revision BIGINT,
             created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
             completed_at_utc TIMESTAMPTZ,
             expires_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp() + interval '7 days',
             PRIMARY KEY (actor_id, resource_kind, scope_key, operation_id)
           );
           CREATE TABLE "GameChallenges" (
             id SERIAL PRIMARY KEY,
             game_id INTEGER NOT NULL,
             title TEXT NOT NULL,
             revision BIGINT NOT NULL DEFAULT 1
           );
           CREATE TABLE "Divisions" (id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL);
           CREATE TABLE "DivisionChallengeConfigs" (
             division_id INTEGER NOT NULL REFERENCES "Divisions"(id),
             challenge_id INTEGER NOT NULL REFERENCES "GameChallenges"(id) ON DELETE CASCADE,
             PRIMARY KEY (division_id, challenge_id)
           );"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let actor = Uuid::new_v4();
    let operation = Uuid::new_v4();
    sqlx::query(r#"INSERT INTO "AspNetUsers" (id) VALUES ($1)"#)
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "Divisions" (id, game_id) VALUES (1,7),(2,7)"#)
        .execute(&pool)
        .await
        .unwrap();
    let fingerprint =
        crate::services::mutation_operations::fingerprint("challenge-create", &("atomic", 7_i32))
            .unwrap();

    let mut failed = pool.begin().await.unwrap();
    assert!(crate::services::mutation_operations::claim(
        &mut failed,
        actor,
        "challenge-create",
        "game:7",
        operation,
        fingerprint,
    )
    .await
    .unwrap()
    .is_none());
    sqlx::query(r#"INSERT INTO "GameChallenges" (game_id,title) VALUES (7,'atomic')"#)
        .execute(&mut *failed)
        .await
        .unwrap();
    failed.rollback().await.unwrap();
    let after_failed_insert: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM "GameChallenges""#)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(after_failed_insert, 0);

    let mut committed = pool.begin().await.unwrap();
    assert!(crate::services::mutation_operations::claim(
        &mut committed,
        actor,
        "challenge-create",
        "game:7",
        operation,
        fingerprint,
    )
    .await
    .unwrap()
    .is_none());
    let challenge_id: i32 = sqlx::query_scalar(
        r#"INSERT INTO "GameChallenges" (game_id,title) VALUES (7,'atomic') RETURNING id"#,
    )
    .fetch_one(&mut *committed)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "DivisionChallengeConfigs" (division_id, challenge_id)
           SELECT id, $1 FROM "Divisions" WHERE game_id = 7"#,
    )
    .bind(challenge_id)
    .execute(&mut *committed)
    .await
    .unwrap();
    crate::services::mutation_operations::complete(
        &mut committed,
        actor,
        "challenge-create",
        "game:7",
        operation,
        &challenge_id.to_string(),
        Some(1),
    )
    .await
    .unwrap();
    committed.commit().await.unwrap();

    let mut replay = pool.begin().await.unwrap();
    let replayed = crate::services::mutation_operations::claim(
        &mut replay,
        actor,
        "challenge-create",
        "game:7",
        operation,
        fingerprint,
    )
    .await
    .unwrap()
    .unwrap();
    replay.commit().await.unwrap();
    assert_eq!(replayed.result_id, challenge_id.to_string());
    let counts: (i64, i64) = sqlx::query_as(
        r#"SELECT (SELECT COUNT(*) FROM "GameChallenges"),
                  (SELECT COUNT(*) FROM "DivisionChallengeConfigs")"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(counts, (1, 2));

    // A failure after both the child-policy seed and operation completion is
    // still one rollback: no second challenge or replay result survives.
    let rollback_operation = Uuid::new_v4();
    let rollback_fingerprint = crate::services::mutation_operations::fingerprint(
        "challenge-create",
        &("rolled-back", 7_i32),
    )
    .unwrap();
    let mut after_seed = pool.begin().await.unwrap();
    crate::services::mutation_operations::claim(
        &mut after_seed,
        actor,
        "challenge-create",
        "game:7",
        rollback_operation,
        rollback_fingerprint,
    )
    .await
    .unwrap();
    let rolled_back_id: i32 = sqlx::query_scalar(
        r#"INSERT INTO "GameChallenges" (game_id,title)
           VALUES (7,'rolled-back') RETURNING id"#,
    )
    .fetch_one(&mut *after_seed)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "DivisionChallengeConfigs" (division_id, challenge_id)
           SELECT id, $1 FROM "Divisions" WHERE game_id = 7"#,
    )
    .bind(rolled_back_id)
    .execute(&mut *after_seed)
    .await
    .unwrap();
    crate::services::mutation_operations::complete(
        &mut after_seed,
        actor,
        "challenge-create",
        "game:7",
        rollback_operation,
        &rolled_back_id.to_string(),
        Some(1),
    )
    .await
    .unwrap();
    after_seed.rollback().await.unwrap();
    let after_rollback: (i64, i64, i64) = sqlx::query_as(
        r#"SELECT (SELECT COUNT(*) FROM "GameChallenges"),
                  (SELECT COUNT(*) FROM "DivisionChallengeConfigs"),
                  (SELECT COUNT(*) FROM "MutationOperations")"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(after_rollback, (1, 2, 1));

    let first = sqlx::query(
        r#"UPDATE "GameChallenges" SET title='new', revision=revision+1
            WHERE id=$1 AND revision=1"#,
    )
    .bind(challenge_id)
    .execute(&pool)
    .await
    .unwrap();
    let stale = sqlx::query(
        r#"UPDATE "GameChallenges" SET title='stale', revision=revision+1
            WHERE id=$1 AND revision=1"#,
    )
    .bind(challenge_id)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(first.rows_affected(), 1);
    assert_eq!(stale.rows_affected(), 0);

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
