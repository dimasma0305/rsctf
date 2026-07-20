use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use super::{
    acquire_publication_lock, drain_publications, publish_managed_backend_if_eligible,
    retain_created_backend_identity, rollback_created_backend_with, ManagedBackendPublication,
};
use crate::utils::enums::{ChallengeReviewStatus, ChallengeType, ParticipationStatus};
use crate::utils::error::AppError;

struct Harness {
    admin: sqlx::PgPool,
    pool: sqlx::PgPool,
    schema: String,
}

impl Harness {
    async fn new() -> Self {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("rsctf_ad_publication_{}", uuid::Uuid::new_v4().simple());
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
            r#"
            CREATE TABLE "Games" (
              id INTEGER PRIMARY KEY,
              end_time_utc TIMESTAMPTZ NOT NULL,
              deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TABLE "Teams" (
              id INTEGER PRIMARY KEY,
              deletion_pending BOOLEAN NOT NULL
            );
            CREATE TABLE "Participations" (
              id INTEGER PRIMARY KEY,
              game_id INTEGER NOT NULL,
              team_id INTEGER NOT NULL,
              status SMALLINT NOT NULL
            );
            CREATE TABLE "GameChallenges" (
              id INTEGER PRIMARY KEY,
              game_id INTEGER NOT NULL,
              is_enabled BOOLEAN NOT NULL,
              review_status SMALLINT NOT NULL,
              "Type" SMALLINT NOT NULL,
              ad_self_hosted BOOLEAN NOT NULL,
              deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TABLE "AdTeamServices" (
              id SERIAL PRIMARY KEY,
              game_id INTEGER NOT NULL,
              participation_id INTEGER NOT NULL,
              challenge_id INTEGER NOT NULL,
              host TEXT NOT NULL,
              port INTEGER NOT NULL,
              status SMALLINT NOT NULL,
              container_id TEXT,
              last_reset_at TIMESTAMPTZ,
              UNIQUE (participation_id, challenge_id)
            );
            INSERT INTO "Games" VALUES (1, clock_timestamp() + interval '1 hour', FALSE);
            INSERT INTO "Teams" VALUES (2, FALSE);
            INSERT INTO "Participations" VALUES (3, 1, 2, 1);
            INSERT INTO "GameChallenges" VALUES (4, 1, TRUE, 0, 4, FALSE, FALSE);
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        Self {
            admin,
            pool,
            schema,
        }
    }

    async fn cleanup(self) {
        self.pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{}" CASCADE"#, self.schema))
            .execute(&self.admin)
            .await
            .unwrap();
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn deletion_drain_waits_for_absent_row_publication_without_serializing_other_pairs() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&database_url)
        .await
        .unwrap();
    let game_id = (uuid::Uuid::new_v4().as_u128() % 1_000_000_000) as i32;
    let first = acquire_publication_lock(&pool, game_id, 10, 20)
        .await
        .unwrap();
    let second = tokio::time::timeout(
        Duration::from_secs(1),
        acquire_publication_lock(&pool, game_id, 11, 20),
    )
    .await
    .expect("shared parent fence serialized distinct service publications")
    .unwrap();

    let mut deletion = tokio::spawn({
        let pool = pool.clone();
        async move { drain_publications(&pool, [game_id]).await }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut deletion)
            .await
            .is_err(),
        "deletion crossed publishers whose AdTeamServices rows do not exist yet"
    );

    first.release().await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut deletion)
            .await
            .is_err(),
        "deletion did not wait for every shared publication owner"
    );
    second.release().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), deletion)
        .await
        .expect("deletion drain stayed blocked after publishers released")
        .unwrap()
        .unwrap();
    pool.close().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn eligibility_cas_preserves_identity_and_cascaded_owner_still_destroys_local_backend() {
    let harness = Harness::new().await;
    let first = ManagedBackendPublication {
        game_id: 1,
        participation_id: 3,
        challenge_id: 4,
        host: "10.13.40.7",
        port: 8080,
        backend_id: "runtime-a",
    };
    assert!(publish_managed_backend_if_eligible(&harness.pool, first)
        .await
        .unwrap());

    sqlx::query(r#"UPDATE "Games" SET deletion_pending = TRUE WHERE id = 1"#)
        .execute(&harness.pool)
        .await
        .unwrap();
    let replacement = ManagedBackendPublication {
        backend_id: "runtime-b",
        ..first
    };
    assert!(
        !publish_managed_backend_if_eligible(&harness.pool, replacement)
            .await
            .unwrap(),
        "publication CAS ignored the durable game-deletion fence"
    );
    sqlx::query(r#"UPDATE "Games" SET deletion_pending = FALSE WHERE id = 1"#)
        .execute(&harness.pool)
        .await
        .unwrap();

    sqlx::query(r#"UPDATE "GameChallenges" SET deletion_pending = TRUE WHERE id = 4"#)
        .execute(&harness.pool)
        .await
        .unwrap();
    assert!(
        !publish_managed_backend_if_eligible(&harness.pool, replacement)
            .await
            .unwrap(),
        "publication CAS ignored the durable challenge-deletion fence"
    );
    sqlx::query(r#"UPDATE "GameChallenges" SET deletion_pending = FALSE WHERE id = 4"#)
        .execute(&harness.pool)
        .await
        .unwrap();

    sqlx::query(r#"UPDATE "Teams" SET deletion_pending = TRUE WHERE id = 2"#)
        .execute(&harness.pool)
        .await
        .unwrap();
    assert!(
        !publish_managed_backend_if_eligible(&harness.pool, replacement)
            .await
            .unwrap(),
        "publication CAS ignored the durable team-deletion fence"
    );
    let retained: Option<String> = sqlx::query_scalar(
        r#"SELECT container_id FROM "AdTeamServices"
            WHERE participation_id = 3 AND challenge_id = 4"#,
    )
    .fetch_one(&harness.pool)
    .await
    .unwrap();
    assert_eq!(retained.as_deref(), Some("runtime-a"));

    // Creation records an inactive retry identity even though deletion has
    // already made endpoint publication ineligible. A failed immediate destroy
    // therefore remains discoverable by the deletion sweep.
    assert!(
        retain_created_backend_identity(&harness.pool, 1, 3, 4, "runtime-b")
            .await
            .unwrap()
    );
    let failed_destroy =
        rollback_created_backend_with(&harness.pool, 3, 4, "runtime-b", async { Ok(()) }, async {
            Err(AppError::internal("injected backend destroy failure"))
        })
        .await;
    assert!(failed_destroy.is_err());
    let retained_after_failure: Option<String> = sqlx::query_scalar(
        r#"SELECT container_id FROM "AdTeamServices"
            WHERE participation_id = 3 AND challenge_id = 4"#,
    )
    .fetch_one(&harness.pool)
    .await
    .unwrap();
    assert_eq!(retained_after_failure.as_deref(), Some("runtime-b"));

    // Simulate the historical failure: the owner cascades after the publisher
    // remembers runtime-b. Rollback must invoke that local destroy directly;
    // it cannot depend on finding the vanished row again.
    sqlx::query(r#"DELETE FROM "AdTeamServices""#)
        .execute(&harness.pool)
        .await
        .unwrap();
    let reconciled = Arc::new(AtomicBool::new(false));
    let destroyed = Arc::new(AtomicBool::new(false));
    rollback_created_backend_with(
        &harness.pool,
        3,
        4,
        "runtime-b",
        {
            let reconciled = Arc::clone(&reconciled);
            async move {
                reconciled.store(true, Ordering::SeqCst);
                Ok(())
            }
        },
        {
            let destroyed = Arc::clone(&destroyed);
            async move {
                destroyed.store(true, Ordering::SeqCst);
                Ok(())
            }
        },
    )
    .await
    .unwrap();
    assert!(reconciled.load(Ordering::SeqCst));
    assert!(destroyed.load(Ordering::SeqCst));

    let destroyed_after_failed_reconcile = Arc::new(AtomicBool::new(false));
    let failed_reconcile = rollback_created_backend_with(
        &harness.pool,
        3,
        4,
        "runtime-b",
        async { Err(AppError::internal("injected reconciliation failure")) },
        {
            let destroyed = Arc::clone(&destroyed_after_failed_reconcile);
            async move {
                destroyed.store(true, Ordering::SeqCst);
                Ok(())
            }
        },
    )
    .await;
    assert!(failed_reconcile.is_err());
    assert!(
        !destroyed_after_failed_reconcile.load(Ordering::SeqCst),
        "backend address was released while stale routing may still exist"
    );
    harness.cleanup().await;
}

#[test]
fn fixture_enum_values_match_publication_predicates() {
    assert_eq!(ParticipationStatus::Accepted as i16, 1);
    assert_eq!(ChallengeReviewStatus::Active as i16, 0);
    assert_eq!(ChallengeType::AttackDefense as i16, 4);
}
