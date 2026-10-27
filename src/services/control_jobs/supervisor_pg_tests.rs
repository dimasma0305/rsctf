use super::*;

async fn fixture() -> crate::utils::test_database::TestDatabase {
    let db = crate::utils::test_database::TestDatabase::new("control_lanes").await;
    sqlx::raw_sql(
        r#"CREATE TABLE "Games" (id INTEGER PRIMARY KEY);
        CREATE TABLE "GameChallenges" (id INTEGER PRIMARY KEY);
        CREATE TABLE "Participations" (id INTEGER PRIMARY KEY);
        INSERT INTO "Games" VALUES (1), (2);"#,
    )
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::raw_sql(crate::migrations::CONTROL_JOBS_SQL)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::raw_sql(crate::migrations::CONTROL_JOB_CANCELLATION_SQL)
        .execute(&db.pool)
        .await
        .unwrap();
    db
}

async fn queue(pool: &sqlx::PgPool, kind: ControlJobKind, game: i32) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "ControlPlaneJobs"
        (id, kind, scope_key, game_id, operation_id, fingerprint, input)
        VALUES ($1, $2, $3, $4, $1, $5, '{}')"#,
    )
    .bind(id)
    .bind(kind.as_str())
    .bind(id.to_string())
    .bind(game)
    .bind("a".repeat(64))
    .execute(pool)
    .await
    .unwrap();
    id
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn replicas_share_lane_limits_while_gameplay_progresses() {
    let db = fixture().await;
    for _ in 0..10 {
        queue(&db.pool, ControlJobKind::BuildBatch, 1).await;
    }
    let mut replicas = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let pool = db.pool.clone();
        replicas.spawn(async move {
            claim_next(&pool, ControlJobKind::BuildBatch, Duration::from_secs(900))
                .await
                .unwrap()
        });
    }
    let mut builds = Vec::new();
    while let Some(result) = replicas.join_next().await {
        if let Some(job) = result.unwrap() {
            builds.push(job);
        }
    }
    // Admission uses a nonblocking transaction lock. A colliding poll may
    // defer; fill the remaining slot as subsequent owner polls would.
    while builds.len() < 2 {
        builds.push(
            claim_next(
                &db.pool,
                ControlJobKind::BuildBatch,
                Duration::from_secs(900),
            )
            .await
            .unwrap()
            .unwrap(),
        );
    }
    assert!(claim_next(
        &db.pool,
        ControlJobKind::BuildBatch,
        Duration::from_secs(900)
    )
    .await
    .unwrap()
    .is_none());
    let reset = queue(&db.pool, ControlJobKind::AdReset, 2).await;
    let recovery = claim_next(&db.pool, ControlJobKind::AdReset, Duration::from_secs(900))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        recovery.model.id, reset,
        "two running builds must not block another game's reset"
    );
    assert!(complete(
        &db.pool,
        reset,
        recovery.lease_token,
        recovery.input_revision,
        serde_json::json!({})
    )
    .await
    .unwrap());
    assert_eq!(
        sqlx::query_scalar::<_, i64>(r#"SELECT count(*) FROM "ControlPlaneJobs" WHERE status = 1"#)
            .fetch_one(&db.pool)
            .await
            .unwrap(),
        2
    );
    db.close().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn shutdown_releases_only_unstarted_current_claim_and_preserves_cancellation() {
    let db = fixture().await;
    let id = queue(&db.pool, ControlJobKind::BuildBatch, 1).await;
    let first = claim_next(
        &db.pool,
        ControlJobKind::BuildBatch,
        Duration::from_secs(900),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(release_unstarted(&db.pool, &first).await.unwrap());
    let second = claim_next(
        &db.pool,
        ControlJobKind::BuildBatch,
        Duration::from_secs(900),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        !release_unstarted(&db.pool, &first).await.unwrap(),
        "old drain must not release replacement"
    );
    assert_ne!(first.lease_token, second.lease_token);
    request_cancellation(&db.pool, id).await.unwrap();
    assert!(release_unstarted(&db.pool, &second).await.unwrap());
    assert_eq!(
        get(&db.pool, id).await.unwrap().unwrap().status,
        ControlJobStatus::Cancelled
    );
    assert!(claim_next(
        &db.pool,
        ControlJobKind::BuildBatch,
        Duration::from_secs(900)
    )
    .await
    .unwrap()
    .is_none());
    db.close().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL; run database regressions serially"]
async fn retry_counter_counts_committed_expired_claims_and_keeps_stale_writes_fenced() {
    use crate::services::observability_state::RECLAIMED;
    use std::sync::atomic::Ordering::Relaxed;
    let db = fixture().await;
    let before = RECLAIMED[7].load(Relaxed); // SecurityDerivation, a fixed label.
    let id = queue(&db.pool, ControlJobKind::SecurityDerivation, 1).await;
    let first = claim_next(
        &db.pool,
        ControlJobKind::SecurityDerivation,
        Duration::from_secs(30),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(RECLAIMED[7].load(Relaxed), before);
    assert!(claim_next(
        &db.pool,
        ControlJobKind::SecurityDerivation,
        Duration::from_secs(30)
    )
    .await
    .unwrap()
    .is_none());
    sqlx::query(r#"UPDATE "ControlPlaneJobs" SET lease_expires_at_utc = clock_timestamp() - interval '1 second' WHERE id = $1"#)
        .bind(id).execute(&db.pool).await.unwrap();
    let second = claim_next(
        &db.pool,
        ControlJobKind::SecurityDerivation,
        Duration::from_secs(30),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(RECLAIMED[7].load(Relaxed), before + 1);
    assert!(!complete(
        &db.pool,
        id,
        first.lease_token,
        first.input_revision,
        serde_json::json!({})
    )
    .await
    .unwrap());
    assert!(release_unstarted(&db.pool, &second).await.unwrap());
    let third = claim_next(
        &db.pool,
        ControlJobKind::SecurityDerivation,
        Duration::from_secs(30),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        RECLAIMED[7].load(Relaxed),
        before + 1,
        "re-admitting unstarted work is not expired-lease recovery"
    );
    assert!(complete(
        &db.pool,
        id,
        third.lease_token,
        third.input_revision,
        serde_json::json!({})
    )
    .await
    .unwrap());
    db.close().await;
}
