use std::str::FromStr;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use super::super::INSERTABLE_GAME_SQL;
use super::{
    clear_destroyed_koth_target, clear_destroyed_shared_container, handle_teardown_result,
    runtime_definition_snapshot, teardown_allowed,
};
use crate::utils::enums::{ChallengeReviewStatus, ChallengeType};
use crate::utils::error::AppError;

#[test]
fn physical_delete_propagates_teardown_failure_while_disable_keeps_retry_owner() {
    assert!(handle_teardown_result(
        Err(AppError::internal("injected destroy failure")),
        true,
        7,
        "test",
    )
    .is_err());
    assert!(handle_teardown_result(
        Err(AppError::internal("injected destroy failure")),
        false,
        7,
        "test",
    )
    .is_ok());
}

#[test]
fn challenge_insert_is_fenced_by_the_durable_game_delete_marker() {
    assert!(INSERTABLE_GAME_SQL.contains("NOT deletion_pending"));
    assert!(INSERTABLE_GAME_SQL.contains("FOR SHARE"));
}

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
        let schema = format!("rsctf_runtime_transition_{}", uuid::Uuid::new_v4().simple());
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
            CREATE TABLE "GameChallenges" (
              id INTEGER PRIMARY KEY,
              game_id INTEGER NOT NULL DEFAULT 1,
              is_enabled BOOLEAN NOT NULL,
              review_status SMALLINT NOT NULL,
              enable_shared_container BOOLEAN NOT NULL,
              shared_container_id UUID,
              container_image TEXT,
              build_status SMALLINT NOT NULL DEFAULT 0,
              title TEXT NOT NULL DEFAULT 'original',
              hints JSONB
            );
            CREATE TABLE "FlagContexts" (
              id SERIAL PRIMARY KEY,
              challenge_id INTEGER,
              flag TEXT NOT NULL
            );
            CREATE TABLE "KothTargets" (
              id INTEGER PRIMARY KEY,
              game_id INTEGER NOT NULL,
              challenge_id INTEGER NOT NULL,
              host TEXT NOT NULL,
              port INTEGER NOT NULL,
              container_id TEXT,
              holder_participation_id INTEGER,
              held_since TIMESTAMPTZ
            );
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

    async fn add_challenge(&self, id: i32, enabled: bool, review: ChallengeReviewStatus) {
        sqlx::query(
            r#"INSERT INTO "GameChallenges"
                 (id, is_enabled, review_status, enable_shared_container)
               VALUES ($1, $2, $3, FALSE)"#,
        )
        .bind(id)
        .bind(enabled)
        .bind(review as i16)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    async fn cleanup(self) {
        self.pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{}" CASCADE"#, self.schema))
            .execute(&self.admin)
            .await
            .unwrap();
    }
}

async fn assert_waiting(acquired: &mut tokio::sync::oneshot::Receiver<()>) {
    assert!(
        tokio::time::timeout(Duration::from_millis(100), acquired)
            .await
            .is_err(),
        "a later runtime transition overtook cleanup"
    );
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn active_shared_to_per_team_flip_rejects_stale_player_before_replacement() {
    let harness = Harness::new().await;
    harness
        .add_challenge(13, true, ChallengeReviewStatus::Active)
        .await;
    sqlx::query(
        r#"UPDATE "GameChallenges"
              SET enable_shared_container = TRUE
            WHERE id = 13"#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();

    // The player owns the old shared-runtime leaf while its backend launches.
    let runtime_key = "shared-container:13";
    let player_runtime = crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(
        &harness.pool,
        runtime_key,
    )
    .await
    .unwrap();
    let snapshot =
        crate::services::challenge_workloads::acquire_definition_lock(&harness.pool, 1, 13)
            .await
            .unwrap();
    let old_topology: (bool, bool) = sqlx::query_as(
        r#"SELECT is_enabled, enable_shared_container
             FROM "GameChallenges" WHERE id = 13"#,
    )
    .fetch_one(&harness.pool)
    .await
    .unwrap();
    assert_eq!(old_topology, (true, true));
    snapshot.release().await.unwrap();

    // Phase one publishes only the inactive marker under the definition lock.
    // Cleanup then waits for the player's runtime leaf without retaining that
    // definition lock, so the stale player can revalidate and roll back.
    let definition =
        crate::services::challenge_workloads::acquire_definition_lock(&harness.pool, 1, 13)
            .await
            .unwrap();
    sqlx::query(r#"UPDATE "GameChallenges" SET is_enabled = FALSE WHERE id = 13"#)
        .execute(&harness.pool)
        .await
        .unwrap();
    definition.release().await.unwrap();

    let mut cleanup = tokio::spawn({
        let pool = harness.pool.clone();
        async move {
            crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(&pool, runtime_key)
                .await
        }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut cleanup)
            .await
            .is_err(),
        "old-topology cleanup crossed the in-flight player runtime"
    );

    let publication =
        crate::services::challenge_workloads::acquire_definition_lock(&harness.pool, 1, 13)
            .await
            .unwrap();
    let stale_player_may_publish: bool = sqlx::query_scalar(
        r#"SELECT is_enabled AND enable_shared_container
             FROM "GameChallenges" WHERE id = 13"#,
    )
    .fetch_one(&harness.pool)
    .await
    .unwrap();
    assert!(
        !stale_player_may_publish,
        "stale shared player remained eligible during topology cleanup"
    );
    publication.release().await.unwrap();
    player_runtime.release().await.unwrap();

    let cleanup = tokio::time::timeout(Duration::from_secs(2), cleanup)
        .await
        .expect("cleanup stayed blocked after stale player rollback")
        .unwrap()
        .unwrap();
    cleanup.release().await.unwrap();

    sqlx::query(
        r#"UPDATE "GameChallenges"
              SET title = 'edited-during-drain',
                  hints = '[{"content":"preserve-me"}]'::jsonb
            WHERE id = 13"#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();

    // Phase two publishes the new topology and restores eligibility together;
    // no player can observe an enabled challenge with the old ownership mode.
    let definition =
        crate::services::challenge_workloads::acquire_definition_lock(&harness.pool, 1, 13)
            .await
            .unwrap();
    sqlx::query(
        r#"UPDATE "GameChallenges"
              SET enable_shared_container = FALSE, is_enabled = TRUE
            WHERE id = 13 AND is_enabled = FALSE"#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();
    definition.release().await.unwrap();
    let new_topology: (bool, bool, String, Option<serde_json::Value>) = sqlx::query_as(
        r#"SELECT is_enabled, enable_shared_container, title, hints
             FROM "GameChallenges" WHERE id = 13"#,
    )
    .fetch_one(&harness.pool)
    .await
    .unwrap();
    assert!(new_topology.0);
    assert!(!new_topology.1);
    assert_eq!(new_topology.2, "edited-during-drain");
    assert_eq!(
        new_topology.3,
        Some(serde_json::json!([{"content": "preserve-me"}]))
    );
    harness.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn repository_runtime_update_during_drain_leaves_topology_disabled() {
    let harness = Harness::new().await;
    harness
        .add_challenge(15, true, ChallengeReviewStatus::Active)
        .await;
    sqlx::query(
        r#"UPDATE "GameChallenges"
              SET enable_shared_container = TRUE,
                  container_image = 'registry.example/service@sha256:old'
            WHERE id = 15"#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "FlagContexts" (challenge_id, flag) VALUES (15, 'flag{old}')"#)
        .execute(&harness.pool)
        .await
        .unwrap();

    let before = runtime_definition_snapshot(&harness.pool, 15, ChallengeType::StaticContainer)
        .await
        .unwrap();
    sqlx::query(r#"UPDATE "GameChallenges" SET is_enabled = FALSE WHERE id = 15"#)
        .execute(&harness.pool)
        .await
        .unwrap();

    // Presentation changes are intentionally preserved by the phase-two fresh
    // reload and do not make an otherwise-compatible transition stale.
    sqlx::query(
        r#"UPDATE "GameChallenges"
              SET title = 'repo-title', hints = '["repo-hint"]'::jsonb
            WHERE id = 15"#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();
    assert_eq!(
        before,
        runtime_definition_snapshot(&harness.pool, 15, ChallengeType::StaticContainer)
            .await
            .unwrap()
    );

    // A scan performed while the durable disabled marker is visible may stage
    // a new image/build and flag policy. Phase two must detect that definition
    // revision and must not restore eligibility onto the queued runtime.
    sqlx::query(
        r#"UPDATE "GameChallenges"
              SET container_image = 'registry.example/service@sha256:new',
                  build_status = 1
            WHERE id = 15"#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();
    sqlx::query(r#"UPDATE "FlagContexts" SET flag = 'flag{new}' WHERE challenge_id = 15"#)
        .execute(&harness.pool)
        .await
        .unwrap();
    let after = runtime_definition_snapshot(&harness.pool, 15, ChallengeType::StaticContainer)
        .await
        .unwrap();
    assert_ne!(before, after);
    assert!(
        !sqlx::query_scalar::<_, bool>(r#"SELECT is_enabled FROM "GameChallenges" WHERE id = 15"#,)
            .fetch_one(&harness.pool)
            .await
            .unwrap(),
        "a stale phase two must leave the challenge fail-closed"
    );
    let presentation: (String, Option<serde_json::Value>) =
        sqlx::query_as(r#"SELECT title, hints FROM "GameChallenges" WHERE id = 15"#)
            .fetch_one(&harness.pool)
            .await
            .unwrap();
    assert_eq!(presentation.0, "repo-title");
    assert_eq!(presentation.1, Some(serde_json::json!(["repo-hint"])));
    harness.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn shared_cleanup_cas_preserves_concurrent_edits_and_replacement() {
    let harness = Harness::new().await;
    harness
        .add_challenge(14, false, ChallengeReviewStatus::Active)
        .await;
    let destroyed = uuid::Uuid::new_v4();
    let replacement = uuid::Uuid::new_v4();
    sqlx::query(
        r#"UPDATE "GameChallenges"
              SET shared_container_id = $2, title = 'edited',
                  hints = '[{"content":"new"}]'::jsonb
            WHERE id = $1"#,
    )
    .bind(14)
    .bind(replacement)
    .execute(&harness.pool)
    .await
    .unwrap();
    assert!(
        !clear_destroyed_shared_container(&harness.pool, 14, destroyed)
            .await
            .unwrap(),
        "stale cleanup cleared a replacement shared runtime"
    );
    let preserved: (Option<uuid::Uuid>, String, Option<serde_json::Value>) = sqlx::query_as(
        r#"SELECT shared_container_id, title, hints
             FROM "GameChallenges" WHERE id = 14"#,
    )
    .fetch_one(&harness.pool)
    .await
    .unwrap();
    assert_eq!(preserved.0, Some(replacement));
    assert_eq!(preserved.1, "edited");
    assert_eq!(preserved.2, Some(serde_json::json!([{"content": "new"}])));

    sqlx::query(r#"UPDATE "GameChallenges" SET shared_container_id = $2 WHERE id = $1"#)
        .bind(14)
        .bind(destroyed)
        .execute(&harness.pool)
        .await
        .unwrap();
    assert!(
        clear_destroyed_shared_container(&harness.pool, 14, destroyed)
            .await
            .unwrap()
    );
    let after_exact_clear: (Option<uuid::Uuid>, String) =
        sqlx::query_as(r#"SELECT shared_container_id, title FROM "GameChallenges" WHERE id = 14"#)
            .fetch_one(&harness.pool)
            .await
            .unwrap();
    assert_eq!(after_exact_clear, (None, "edited".to_string()));
    harness.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn distinct_transitions_leave_small_pool_headroom_for_nested_work() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .unwrap();
    let first =
        crate::services::challenge_workloads::acquire_runtime_transition_lock(&pool, 21_001)
            .await
            .unwrap();

    let contender_pool = pool.clone();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (acquired_tx, mut acquired_rx) = tokio::sync::oneshot::channel();
    let contender = tokio::spawn(async move {
        started_tx.send(()).unwrap();
        let lock = crate::services::challenge_workloads::acquire_runtime_transition_lock(
            &contender_pool,
            21_002,
        )
        .await
        .unwrap();
        acquired_tx.send(()).unwrap();
        lock
    });
    started_rx.await.unwrap();
    assert_waiting(&mut acquired_rx).await;

    let nested = tokio::time::timeout(Duration::from_secs(1), pool.acquire())
        .await
        .expect("an outer transition must leave one connection for nested work")
        .unwrap();
    drop(nested);

    first.release().await.unwrap();
    let second = tokio::time::timeout(Duration::from_secs(2), contender)
        .await
        .unwrap()
        .unwrap();
    second.release().await.unwrap();
    pool.close().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn disable_cleanup_blocks_reenable_and_rechecks_playability() {
    let harness = Harness::new().await;
    harness
        .add_challenge(11, true, ChallengeReviewStatus::Active)
        .await;
    let cleanup =
        crate::services::challenge_workloads::acquire_runtime_transition_lock(&harness.pool, 11)
            .await
            .unwrap();
    sqlx::query(r#"UPDATE "GameChallenges" SET is_enabled = FALSE WHERE id = 11"#)
        .execute(&harness.pool)
        .await
        .unwrap();

    let pool = harness.pool.clone();
    let (acquired_tx, mut acquired_rx) = tokio::sync::oneshot::channel();
    let reenable = tokio::spawn(async move {
        let transition =
            crate::services::challenge_workloads::acquire_runtime_transition_lock(&pool, 11)
                .await
                .unwrap();
        acquired_tx.send(()).unwrap();
        sqlx::query(r#"UPDATE "GameChallenges" SET is_enabled = TRUE WHERE id = 11"#)
            .execute(&pool)
            .await
            .unwrap();
        transition.release().await.unwrap();
    });
    assert_waiting(&mut acquired_rx).await;
    assert!(teardown_allowed(&harness.pool, 11, true).await);

    cleanup.release().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), reenable)
        .await
        .unwrap()
        .unwrap();
    assert!(!teardown_allowed(&harness.pool, 11, true).await);
    harness.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn reject_cleanup_blocks_approval_and_rechecks_review_state() {
    let harness = Harness::new().await;
    harness
        .add_challenge(12, true, ChallengeReviewStatus::Active)
        .await;
    let cleanup =
        crate::services::challenge_workloads::acquire_runtime_transition_lock(&harness.pool, 12)
            .await
            .unwrap();
    sqlx::query(r#"UPDATE "GameChallenges" SET review_status = $1 WHERE id = 12"#)
        .bind(ChallengeReviewStatus::Rejected as i16)
        .execute(&harness.pool)
        .await
        .unwrap();

    let pool = harness.pool.clone();
    let (acquired_tx, mut acquired_rx) = tokio::sync::oneshot::channel();
    let approve = tokio::spawn(async move {
        let transition =
            crate::services::challenge_workloads::acquire_runtime_transition_lock(&pool, 12)
                .await
                .unwrap();
        acquired_tx.send(()).unwrap();
        sqlx::query(r#"UPDATE "GameChallenges" SET review_status = $1 WHERE id = 12"#)
            .bind(ChallengeReviewStatus::Active as i16)
            .execute(&pool)
            .await
            .unwrap();
        transition.release().await.unwrap();
    });
    assert_waiting(&mut acquired_rx).await;
    assert!(teardown_allowed(&harness.pool, 12, true).await);

    cleanup.release().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), approve)
        .await
        .unwrap()
        .unwrap();
    assert!(!teardown_allowed(&harness.pool, 12, true).await);
    harness.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn opposite_shared_topology_edits_cannot_overtake_cleanup() {
    let harness = Harness::new().await;
    harness
        .add_challenge(13, true, ChallengeReviewStatus::Active)
        .await;
    let first =
        crate::services::challenge_workloads::acquire_runtime_transition_lock(&harness.pool, 13)
            .await
            .unwrap();
    sqlx::query(r#"UPDATE "GameChallenges" SET enable_shared_container = TRUE WHERE id = 13"#)
        .execute(&harness.pool)
        .await
        .unwrap();

    let pool = harness.pool.clone();
    let (acquired_tx, mut acquired_rx) = tokio::sync::oneshot::channel();
    let opposite = tokio::spawn(async move {
        let transition =
            crate::services::challenge_workloads::acquire_runtime_transition_lock(&pool, 13)
                .await
                .unwrap();
        acquired_tx.send(()).unwrap();
        sqlx::query(r#"UPDATE "GameChallenges" SET enable_shared_container = FALSE WHERE id = 13"#)
            .execute(&pool)
            .await
            .unwrap();
        transition.release().await.unwrap();
    });
    assert_waiting(&mut acquired_rx).await;
    assert!(sqlx::query_scalar::<_, bool>(
        r#"SELECT enable_shared_container FROM "GameChallenges" WHERE id = 13"#,
    )
    .fetch_one(&harness.pool)
    .await
    .unwrap());

    first.release().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), opposite)
        .await
        .unwrap()
        .unwrap();
    assert!(!sqlx::query_scalar::<_, bool>(
        r#"SELECT enable_shared_container FROM "GameChallenges" WHERE id = 13"#,
    )
    .fetch_one(&harness.pool)
    .await
    .unwrap());
    harness.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn koth_teardown_retains_backend_identity_until_destroy_succeeds() {
    let harness = Harness::new().await;
    sqlx::query(
        r#"INSERT INTO "KothTargets"
              (id, game_id, challenge_id, host, port, container_id,
               holder_participation_id, held_since)
            VALUES (31, 7, 11, '10.13.40.31', 8080, 'runtime-old',
                    17, clock_timestamp())"#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();

    assert!(
        !clear_destroyed_koth_target(&harness.pool, 31, "runtime-old")
            .await
            .unwrap(),
        "an active endpoint must not be detached by stale cleanup"
    );
    sqlx::query(
        r#"UPDATE "KothTargets"
              SET host = '', port = 0,
                  holder_participation_id = NULL, held_since = NULL
            WHERE id = 31 AND container_id = 'runtime-old'"#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            r#"SELECT COUNT(*) FROM "KothTargets"
                WHERE id = 31 AND host = '' AND port = 0
                  AND container_id = 'runtime-old'
                  AND holder_participation_id IS NULL AND held_since IS NULL"#,
        )
        .fetch_one(&harness.pool)
        .await
        .unwrap(),
        1,
        "policy reconciliation and failed destroys must retain the retry identity"
    );

    assert!(
        !clear_destroyed_koth_target(&harness.pool, 31, "runtime-new")
            .await
            .unwrap(),
        "a stale destroy acknowledgement must not detach another backend"
    );
    sqlx::query(r#"UPDATE "KothTargets" SET host = '10.13.40.99', port = 9090 WHERE id = 31"#)
        .execute(&harness.pool)
        .await
        .unwrap();
    assert!(
        !clear_destroyed_koth_target(&harness.pool, 31, "runtime-old")
            .await
            .unwrap(),
        "an endpoint republished during stale cleanup must remain attached"
    );

    sqlx::query(
        r#"UPDATE "KothTargets"
              SET host = '', port = 0,
                  holder_participation_id = NULL, held_since = NULL
            WHERE id = 31 AND container_id = 'runtime-old'"#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();
    assert!(
        clear_destroyed_koth_target(&harness.pool, 31, "runtime-old")
            .await
            .unwrap(),
        "only the exact inactive pointer is cleared after successful destruction"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            r#"SELECT COUNT(*) FROM "KothTargets"
                WHERE id = 31 AND container_id IS NULL"#,
        )
        .fetch_one(&harness.pool)
        .await
        .unwrap(),
        1
    );
    harness.cleanup().await;
}
