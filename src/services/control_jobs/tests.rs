use super::*;

#[test]
fn enqueue_bounds_are_applied_before_postgres() {
    let input = serde_json::json!({"gameId": 1});
    assert!(validate_enqueue("game:1", &"a".repeat(64), &input).is_ok());
    assert!(validate_enqueue("", &"a".repeat(64), &input).is_err());
    assert!(validate_enqueue("game:1", "not-a-digest", &input).is_err());
    assert!(validate_enqueue(
        "game:1",
        &"a".repeat(64),
        &Value::String("x".repeat(MAX_INPUT_BYTES))
    )
    .is_err());
}

#[test]
fn immutable_jobs_never_coalesce_different_revisions() {
    for kind in [
        ControlJobKind::ChallengeBuild,
        ControlJobKind::BuildBatch,
        ControlJobKind::VariantGeneration,
        ControlJobKind::SecurityDerivation,
        ControlJobKind::WorkloadRollout,
    ] {
        assert!(can_coalesce_active(kind, "same", "same"));
        assert!(!can_coalesce_active(kind, "old", "new"));
    }
    assert!(can_coalesce_active(
        ControlJobKind::AdReconcile,
        "weaker",
        "stronger"
    ));
    assert!(can_coalesce_active(
        ControlJobKind::AdReset,
        "player",
        "operator"
    ));
}

#[tokio::test]
#[ignore = "requires migrated disposable PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn postgres_coalesces_retries_and_recovers_one_expired_lease() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .unwrap();
    let game_id: i32 = sqlx::query_scalar(r#"SELECT id FROM "Games" ORDER BY id LIMIT 1"#)
        .fetch_one(&pool)
        .await
        .expect("the disposable database needs one game");
    sqlx::query(r#"DELETE FROM "ControlPlaneJobs" WHERE kind = 'BuildBatch'"#)
        .execute(&pool)
        .await
        .unwrap();

    let operation_id = Uuid::new_v4();
    let scope = format!("test-game:{game_id}:{}", Uuid::new_v4());
    let input = serde_json::json!({ "gameId": game_id });
    let fingerprint = crate::utils::codec::sha256_str(&input.to_string());
    let first = enqueue(
        &pool,
        ControlJobKind::BuildBatch,
        &scope,
        game_id,
        None,
        operation_id,
        &fingerprint,
        input.clone(),
    );
    let second = enqueue(
        &pool,
        ControlJobKind::BuildBatch,
        &scope,
        game_id,
        None,
        operation_id,
        &fingerprint,
        input.clone(),
    );
    let (first, second) = tokio::join!(first, second);
    let first = first.unwrap();
    let second = second.unwrap();
    assert_eq!(first.id, second.id);

    let coalesced_operation = Uuid::new_v4();
    let coalesced = enqueue(
        &pool,
        ControlJobKind::BuildBatch,
        &scope,
        game_id,
        None,
        coalesced_operation,
        &fingerprint,
        input.clone(),
    )
    .await
    .unwrap();
    assert_eq!(coalesced.id, first.id);
    assert_eq!(
        get_by_operation(&pool, coalesced_operation)
            .await
            .unwrap()
            .unwrap()
            .id,
        first.id
    );

    let lease = claim_next(&pool, ControlJobKind::BuildBatch, Duration::from_secs(30))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lease.model.id, first.id);
    assert!(
        claim_next(&pool, ControlJobKind::BuildBatch, Duration::from_secs(30))
            .await
            .unwrap()
            .is_none()
    );
    sqlx::query(
        r#"UPDATE "ControlPlaneJobs"
              SET lease_expires_at_utc = clock_timestamp() - interval '1 second'
            WHERE id = $1"#,
    )
    .bind(first.id)
    .execute(&pool)
    .await
    .unwrap();
    let recovered = claim_next(&pool, ControlJobKind::BuildBatch, Duration::from_secs(30))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.model.id, first.id);
    assert_ne!(recovered.lease_token, lease.lease_token);
    assert!(!complete(
        &pool,
        first.id,
        lease.lease_token,
        lease.input_revision,
        serde_json::json!({ "enqueued": 1 }),
    )
    .await
    .unwrap());
    assert!(complete(
        &pool,
        first.id,
        recovered.lease_token,
        recovered.input_revision,
        serde_json::json!({ "enqueued": 1 }),
    )
    .await
    .unwrap());
    let replay = enqueue(
        &pool,
        ControlJobKind::BuildBatch,
        &scope,
        game_id,
        None,
        operation_id,
        &fingerprint,
        input,
    )
    .await
    .unwrap();
    assert_eq!(replay.id, first.id);
    assert_eq!(replay.status, ControlJobStatus::Succeeded);
    sqlx::query(r#"DELETE FROM "ControlPlaneJobs" WHERE id = $1"#)
        .bind(first.id)
        .execute(&pool)
        .await
        .unwrap();
}
