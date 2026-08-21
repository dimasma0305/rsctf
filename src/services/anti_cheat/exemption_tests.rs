use chrono::{Duration, Utc};
use uuid::Uuid;

use super::db_tests::{test_config, Harness};
use super::exempt_block;

async fn exemption_applies_at(
    pool: &sqlx::PgPool,
    left: Uuid,
    right: Uuid,
    kind: &str,
    value_hash: &[u8],
    edge_observed_at: chrono::DateTime<Utc>,
) -> bool {
    let (user_a, user_b) = super::exemption::canonical_pair(left, right);
    sqlx::query_scalar(
        r#"SELECT EXISTS (
               SELECT 1
                 FROM "AntiCheatExemptions" exemption
                WHERE exemption.user_a = $1
                  AND exemption.user_b = $2
                  AND exemption.kind = $3
                  AND exemption.value_hash = $4
                  AND exemption.created_at_utc <= $5
                  AND $5 < exemption.expires_at_utc
                  AND (exemption.revoked_at_utc IS NULL
                       OR $5 < exemption.revoked_at_utc)
           )"#,
    )
    .bind(user_a)
    .bind(user_b)
    .bind(kind)
    .bind(value_hash)
    .bind(edge_observed_at)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn exemption_renewal_preserves_historical_intervals_and_revocation_boundaries() {
    let harness = Harness::new().await;
    let config = test_config();
    let owner = Uuid::new_v4();
    let blocked = Uuid::new_v4();
    let adjudicator = Uuid::new_v4();
    let value_hash = vec![0x5a; 32];

    let mut block_ids = Vec::new();
    for occurred_at in [Utc::now() - Duration::minutes(1), Utc::now()] {
        block_ids.push(
            sqlx::query_scalar::<_, i32>(
                r#"INSERT INTO "AntiCheatBlocks"
                     (user_id, conflict_user_id, kind, conflicting_value,
                      conflicting_value_hash, occurred_at_utc)
                   VALUES ($1, $2, 'Ip', '203.0.113.x', $3, $4)
                   RETURNING id"#,
            )
            .bind(blocked)
            .bind(owner)
            .bind(&value_hash)
            .bind(occurred_at)
            .fetch_one(&harness.pool)
            .await
            .unwrap(),
        );
    }

    exempt_block(&harness.pool, &config, block_ids[0], adjudicator)
        .await
        .unwrap();
    let first_before_renewal: (i64, chrono::DateTime<Utc>) = sqlx::query_as(
        r#"SELECT id, created_at_utc
             FROM "AntiCheatExemptions"
            WHERE created_from_block_id = $1"#,
    )
    .bind(block_ids[0])
    .fetch_one(&harness.pool)
    .await
    .unwrap();
    exempt_block(&harness.pool, &config, block_ids[1], adjudicator)
        .await
        .unwrap();

    let intervals: Vec<(i64, i32, chrono::DateTime<Utc>)> = sqlx::query_as(
        r#"SELECT id, created_from_block_id, created_at_utc
             FROM "AntiCheatExemptions"
            ORDER BY id"#,
    )
    .fetch_all(&harness.pool)
    .await
    .unwrap();
    assert_eq!(intervals.len(), 2);
    assert_eq!(intervals[0].0, first_before_renewal.0);
    assert_eq!(intervals[0].2, first_before_renewal.1);
    assert_eq!(intervals[0].1, block_ids[0]);
    assert_eq!(intervals[1].1, block_ids[1]);

    // The first grant is valid until a revocation at hour 8, then there is an
    // uncovered gap until the independently-created renewal starts at hour 20.
    // Revocation is effective at equality.
    let base = Utc::now() - Duration::days(30);
    sqlx::query(
        r#"UPDATE "AntiCheatExemptions"
              SET created_at_utc = $2,
                  expires_at_utc = $3,
                  revoked_at_utc = $4
            WHERE id = $1"#,
    )
    .bind(intervals[0].0)
    .bind(base)
    .bind(base + Duration::hours(10))
    .bind(base + Duration::hours(8))
    .execute(&harness.pool)
    .await
    .unwrap();
    sqlx::query(
        r#"UPDATE "AntiCheatExemptions"
              SET created_at_utc = $2,
                  expires_at_utc = $3,
                  revoked_at_utc = NULL
            WHERE id = $1"#,
    )
    .bind(intervals[1].0)
    .bind(base + Duration::hours(20))
    .bind(base + Duration::hours(30))
    .execute(&harness.pool)
    .await
    .unwrap();

    for (hour, expected) in [(7, true), (8, false), (15, false), (25, true)] {
        assert_eq!(
            exemption_applies_at(
                &harness.pool,
                owner,
                blocked,
                "Ip",
                &value_hash,
                base + Duration::hours(hour),
            )
            .await,
            expected,
            "unexpected exemption result at hour {hour}",
        );
    }

    sqlx::query(
        r#"UPDATE "AntiCheatExemptions"
              SET revoked_at_utc = $2
            WHERE id = $1"#,
    )
    .bind(intervals[1].0)
    .bind(base + Duration::hours(26))
    .execute(&harness.pool)
    .await
    .unwrap();
    for (hour, expected) in [(25, true), (26, false)] {
        assert_eq!(
            exemption_applies_at(
                &harness.pool,
                owner,
                blocked,
                "Ip",
                &value_hash,
                base + Duration::hours(hour),
            )
            .await,
            expected,
            "unexpected revoked exemption result at hour {hour}",
        );
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn exemption_waits_for_bootstrap_before_locking_the_block_row() {
    let harness = Harness::new().await;
    let config = test_config();
    let block_id: i32 = sqlx::query_scalar(
        r#"INSERT INTO "AntiCheatBlocks"
             (user_id, conflict_user_id, kind, conflicting_value,
              conflicting_value_hash, occurred_at_utc)
           VALUES ($1, $2, 'Ip', '203.0.113.x', $3, clock_timestamp())
           RETURNING id"#,
    )
    .bind(Uuid::new_v4())
    .bind(Uuid::new_v4())
    .bind(vec![0x33_u8; 32])
    .fetch_one(&harness.pool)
    .await
    .unwrap();

    let mut bootstrap = harness.pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(super::IDENTITY_BOOTSTRAP_LOCK_ID)
        .execute(&mut *bootstrap)
        .await
        .unwrap();
    let exemption = tokio::spawn({
        let pool = harness.pool.clone();
        let config = config.clone();
        async move { exempt_block(&pool, &config, block_id, Uuid::new_v4()).await }
    });
    tokio::time::sleep(std::time::Duration::from_millis(75)).await;
    assert!(!exemption.is_finished());

    // A NOWAIT row probe succeeds while exemption is queued behind bootstrap.
    // The former row-first order made this fail and allowed bootstrap's table
    // SHARE lock to form a tuple/table deadlock with exemption's UPDATE.
    let mut probe = harness.pool.begin().await.unwrap();
    sqlx::query(r#"SELECT id FROM "AntiCheatBlocks" WHERE id=$1 FOR UPDATE NOWAIT"#)
        .bind(block_id)
        .execute(&mut *probe)
        .await
        .expect("exemption locked the audit tuple before the bootstrap advisory");
    probe.rollback().await.unwrap();

    bootstrap.commit().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), exemption)
        .await
        .expect("exemption did not drain after bootstrap")
        .unwrap()
        .unwrap();
}
