use super::*;

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn busy_claim_admission_releases_its_pool_connection_immediately() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .unwrap();
    let mut blocker = pool.acquire().await.unwrap();
    sqlx::query("SELECT pg_advisory_lock(hashtextextended($1, 0))")
        .bind(CLAIM_LOCK)
        .execute(&mut *blocker)
        .await
        .unwrap();

    let error = match claim::<ContainerInfoModel>(
        &pool,
        Uuid::new_v4(),
        "participation:7",
        Uuid::new_v4(),
        1,
        Some(7),
        11,
        Intent::Create,
        None,
        None,
        false,
    )
    .await
    {
        Ok(_) => panic!("a competing deployment claim must fail fast"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        AppError::RetryableUnavailable { retry_after: 1, .. }
    ));
    let headroom = tokio::time::timeout(Duration::from_secs(1), pool.acquire())
        .await
        .expect("a rejected claim retained the remaining pool connection")
        .unwrap();
    drop(headroom);

    sqlx::query("SELECT pg_advisory_unlock(hashtextextended($1, 0))")
        .bind(CLAIM_LOCK)
        .execute(&mut *blocker)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn exact_replay_and_stale_phase_recovery_are_durable() {
    use sqlx::postgres::PgPoolOptions;

    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("player_operations_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = crate::migrations::test_pg_connect_options(&database_url)
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"
            CREATE TABLE "Containers" (
                id UUID PRIMARY KEY, status SMALLINT NOT NULL,
                container_id TEXT NOT NULL
            );
            CREATE TABLE "GameInstances" (
                id INTEGER PRIMARY KEY, participation_id INTEGER NOT NULL,
                challenge_id INTEGER NOT NULL, container_id UUID
            );
            CREATE TABLE "GameChallenges" (
                id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
                shared_container_id UUID
            );
            "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(crate::migrations::PLAYER_CONTAINER_OPERATIONS_SQL)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(crate::migrations::PLAYER_OPERATION_RECOVERY_SQL)
        .execute(&pool)
        .await
        .unwrap();

    let actor = Uuid::new_v4();
    let reaping_publication = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "Containers" (id, status, container_id)
               VALUES ($1, $2, 'backend-reaping')"#,
    )
    .bind(reaping_publication)
    .bind(ContainerStatus::Running as i16)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "ManagedContainerReapOperations"
                   (backend_id, container_id, scope_key, lease_owner,
                    lease_expires_at_utc)
               VALUES ('backend-reaping', $1, 'game-container:70', $2,
                       clock_timestamp() - interval '1 second')"#,
    )
    .bind(reaping_publication)
    .bind(Uuid::new_v4())
    .execute(&pool)
    .await
    .unwrap();
    assert!(matches!(
        claim::<()>(
            &pool,
            Uuid::new_v4(),
            "participation:70",
            actor,
            1,
            Some(70),
            10,
            Intent::Delete,
            Some(reaping_publication),
            None,
            false,
        )
        .await
        .unwrap_err(),
        AppError::RetryableUnavailable { .. }
    ));
    sqlx::query(r#"DELETE FROM "ManagedContainerReapOperations""#)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"DELETE FROM "Containers" WHERE id = $1"#)
        .bind(reaping_publication)
        .execute(&pool)
        .await
        .unwrap();

    let operation_id = Uuid::new_v4();
    let publication_id = Uuid::new_v4();
    let owned = match claim::<()>(
        &pool,
        operation_id,
        "participation:71",
        actor,
        1,
        Some(71),
        11,
        Intent::Delete,
        Some(publication_id),
        None,
        false,
    )
    .await
    .unwrap()
    {
        ClaimOutcome::Owned(operation) => operation,
        _ => panic!("first exact operation must be owned"),
    };
    complete(&pool, &owned, &()).await.unwrap();
    assert!(matches!(
        claim::<()>(
            &pool,
            operation_id,
            "participation:71",
            actor,
            1,
            Some(71),
            11,
            Intent::Delete,
            Some(publication_id),
            None,
            false,
        )
        .await
        .unwrap(),
        ClaimOutcome::Recovered(())
    ));

    let stale_scope = "participation:72";
    let stale_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "PlayerContainerOperations"
                   (operation_id, scope_key, actor_user_id, game_id, participation_id,
                    challenge_id, intent, publication_id, state, lease_expires_at_utc)
               VALUES ($1, $2, $3, 1, 72, 12, 'Create', $4, 'Running',
                       clock_timestamp() - interval '1 second')"#,
    )
    .bind(stale_id)
    .bind(stale_scope)
    .bind(actor)
    .bind(Uuid::new_v4())
    .execute(&pool)
    .await
    .unwrap();
    assert!(matches!(
        claim::<()>(
            &pool,
            Uuid::new_v4(),
            stale_scope,
            actor,
            1,
            Some(72),
            13,
            Intent::Delete,
            Some(Uuid::new_v4()),
            None,
            false,
        )
        .await
        .unwrap(),
        ClaimOutcome::Owned(_)
    ));
    let stale_state: String = sqlx::query_scalar(
        r#"SELECT state FROM "PlayerContainerOperations" WHERE operation_id = $1"#,
    )
    .bind(stale_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stale_state, "Failed");

    let ambiguous_scope = "participation:73";
    let ambiguous_id = Uuid::new_v4();
    let ambiguous_publication = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "PlayerContainerOperations"
                   (operation_id, scope_key, actor_user_id, game_id, participation_id,
                    challenge_id, intent, publication_id, state, lease_expires_at_utc,
                    runtime_started, definition_fence)
               VALUES ($1, $2, $3, 1, 73, 14, 'Create', $4, 'Running',
                       clock_timestamp() - interval '1 second', TRUE, 'runtime:test')"#,
    )
    .bind(ambiguous_id)
    .bind(ambiguous_scope)
    .bind(actor)
    .bind(ambiguous_publication)
    .execute(&pool)
    .await
    .unwrap();
    let explicit_error = match claim::<ContainerInfoModel>(
        &pool,
        Uuid::new_v4(),
        ambiguous_scope,
        actor,
        1,
        Some(73),
        14,
        Intent::Create,
        Some(ambiguous_publication),
        Some("runtime:test"),
        false,
    )
    .await
    {
        Ok(_) => panic!("an explicit key must not adopt another ambiguous operation"),
        Err(error) => error,
    };
    assert!(matches!(explicit_error, AppError::Conflict(_)));
    assert!(matches!(
        claim::<ContainerInfoModel>(
            &pool,
            Uuid::new_v4(),
            ambiguous_scope,
            actor,
            1,
            Some(73),
            14,
            Intent::Create,
            Some(ambiguous_publication),
            Some("runtime:test"),
            true,
        )
        .await
        .unwrap(),
        ClaimOutcome::Owned(ref operation) if operation.operation_id == ambiguous_id
    ));

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}

#[test]
fn operation_budgets_and_deadlines_are_finite() {
    assert!((1..=64).contains(&MAX_DEPLOYMENT_OPERATIONS));
    assert!((1..=16).contains(&MAX_LOCAL_OPERATIONS));
    assert!(OPERATION_DEADLINE <= Duration::from_secs(5 * 60));
    assert!(MAX_LOCAL_RESULT_KEYS <= 512);
}

#[test]
fn fresh_and_recovery_claims_share_the_exact_reaper_fence() {
    assert!(MANAGED_REAP_PENDING_SQL.contains("reap.scope_key = $1"));
    assert!(MANAGED_REAP_PENDING_SQL.contains("container.id = reap.container_id"));
    assert!(MANAGED_REAP_PENDING_SQL.contains("container.container_id = reap.backend_id"));
    assert!(MANAGED_REAP_PENDING_SQL.contains("container.id = $2"));
}

#[test]
fn delete_and_extend_replays_are_bound_to_the_expected_runtime() {
    let actor = Uuid::new_v4();
    let runtime = Uuid::new_v4();
    let row = OperationRow {
        operation_id: Uuid::new_v4(),
        scope_key: "participation:7".to_string(),
        actor_user_id: actor,
        game_id: 2,
        participation_id: Some(7),
        challenge_id: 11,
        intent: Intent::Delete.as_str().to_string(),
        publication_id: runtime,
        state: "Running".to_string(),
        result: None,
        lease_active: true,
        runtime_started: false,
        definition_fence: None,
    };
    assert!(validate_identity(
        &row,
        "participation:7",
        actor,
        2,
        Some(7),
        11,
        Intent::Delete,
        Some(runtime),
    )
    .is_ok());
    assert!(validate_identity(
        &row,
        "participation:7",
        actor,
        2,
        Some(7),
        11,
        Intent::Delete,
        Some(Uuid::new_v4()),
    )
    .is_err());
    assert!(validate_identity(
        &row,
        "participation:7",
        actor,
        2,
        Some(7),
        11,
        Intent::Extend,
        Some(runtime),
    )
    .is_err());
}
