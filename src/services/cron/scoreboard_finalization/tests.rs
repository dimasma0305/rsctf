use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use sea_orm::{ActiveModelTrait, Set, SqlxPostgresConnector};
use sea_orm_migration::MigratorTrait;
use sqlx::postgres::PgPoolOptions;

use super::*;
use crate::app_state::AppState;
use crate::models::data::game;
use crate::models::internal::configs::AppConfig;
use crate::services::cache::{Cache, InMemoryCache, RedisCache};
use crate::services::container::NoopContainerManager;
use crate::services::token::TokenService;
use crate::storage::LocalBlobStorage;

struct CountingCache {
    inner: InMemoryCache,
    removals: AtomicUsize,
    sets: AtomicUsize,
    reject_removals: std::sync::atomic::AtomicBool,
}

impl CountingCache {
    fn new() -> Self {
        Self {
            inner: InMemoryCache::new(),
            removals: AtomicUsize::new(0),
            sets: AtomicUsize::new(0),
            reject_removals: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn reject_removals(&self, reject: bool) {
        self.reject_removals.store(reject, Ordering::SeqCst);
    }
}

#[async_trait]
impl Cache for CountingCache {
    async fn get(&self, key: &str) -> Option<Bytes> {
        self.inner.get(key).await
    }

    async fn get_authoritative(&self, key: &str) -> Option<Bytes> {
        self.inner.get_authoritative(key).await
    }

    async fn get_and_remove(&self, key: &str) -> Option<Bytes> {
        self.inner.get_and_remove(key).await
    }

    async fn compare_and_remove(&self, key: &str, expected: &[u8]) -> bool {
        self.inner.compare_and_remove(key, expected).await
    }

    async fn compare_and_remove_confirmed(&self, key: &str, expected: &[u8]) -> Option<bool> {
        self.inner.compare_and_remove_confirmed(key, expected).await
    }

    async fn set_if_absent(
        &self,
        key: &str,
        value: &[u8],
        ttl: Option<std::time::Duration>,
    ) -> bool {
        self.inner.set_if_absent(key, value, ttl).await
    }

    async fn set(&self, key: &str, value: &[u8], ttl: Option<std::time::Duration>) {
        self.sets.fetch_add(1, Ordering::SeqCst);
        self.inner.set(key, value, ttl).await;
    }

    async fn set_confirmed(
        &self,
        key: &str,
        value: &[u8],
        ttl: Option<std::time::Duration>,
    ) -> bool {
        self.sets.fetch_add(1, Ordering::SeqCst);
        self.inner.set_confirmed(key, value, ttl).await
    }

    async fn remove(&self, key: &str) {
        self.removals.fetch_add(1, Ordering::SeqCst);
        if !self.reject_removals.load(Ordering::SeqCst) {
            self.inner.remove(key).await;
        }
    }

    async fn remove_confirmed(&self, key: &str) -> bool {
        self.removals.fetch_add(1, Ordering::SeqCst);
        if self.reject_removals.load(Ordering::SeqCst) {
            false
        } else {
            self.inner.remove_confirmed(key).await
        }
    }
}

async fn publish_missing(cache: &dyn Cache, game_id: i32, builds: &AtomicUsize) -> AppResult<()> {
    for key in crate::controllers::game::scoreboard_render_cache_keys(game_id) {
        if cache.get(&key).await.is_none() {
            builds.fetch_add(1, Ordering::SeqCst);
            cache
                .set(
                    &key,
                    b"immutable-final-board",
                    Some(crate::controllers::game::FINAL_SCOREBOARD_CACHE_TTL),
                )
                .await;
        }
    }
    Ok(())
}

#[test]
fn retry_backoff_is_bounded_and_errors_fit_the_database_column() {
    assert!(MATERIALIZATION_TIMEOUT_SECONDS < LEASE_SECONDS as u64);
    assert_eq!(retry_delay(1), chrono::Duration::seconds(60));
    assert_eq!(retry_delay(2), chrono::Duration::seconds(120));
    assert_eq!(retry_delay(16), chrono::Duration::hours(1));
    for attempt in 1..=10_000 {
        let delay = retry_delay(attempt);
        assert!(delay >= chrono::Duration::seconds(RETRY_BASE_SECONDS));
        assert!(delay <= chrono::Duration::seconds(RETRY_MAX_SECONDS));
    }
    assert_eq!(bounded_error("x".repeat(1_024)).chars().count(), 256);
}

#[test]
fn finalization_sql_is_bounded_and_the_old_sweep_is_gone() {
    assert!(CLAIM_SQL.contains("FOR UPDATE OF finalization SKIP LOCKED"));
    assert!(CLAIM_SQL.contains("LIMIT $4"));
    assert!(CLAIM_SQL.contains("NOT game.practice_mode"));
    assert!(RENEW_LEASE_SQL.contains("lease_expires_at_utc = $4"));
    assert!(RENEW_COMPLETION_LEASE_SQL.contains("invalidated_at_utc IS NOT NULL"));
    assert!(COMPLETE_SQL.contains("game.end_time_utc = finalization.game_end_time_utc"));
    assert!(REQUEST_REPAIR_SQL.contains("invalidated_at_utc = NULL"));
    assert!(REQUEST_REPAIR_SQL.contains("NOT game.practice_mode"));
    assert!(REQUEST_REPAIR_SQL.contains("THEN \"FinalScoreboardMaterializations\".lease_token"));
    let worker = include_str!("../scoreboard_finalization.rs");
    assert!(!worker.contains("ENQUEUE_ENDED_SQL"));
    assert!(!worker.contains("ORDER BY game.end_time_utc"));
    let cron = include_str!("../mod.rs");
    assert!(!cron.contains("flush_stale_scoreboards"));
    assert!(!cron.contains("RECENT_ENDED_HOURS"));
    assert!(!cron.contains("_RecentGames"));
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn multi_replica_claim_retry_restart_and_six_hour_closeout_are_idempotent() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin_options = crate::migrations::test_pg_connect_options(&database_url);
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(admin_options)
        .await
        .unwrap();
    let schema = format!("rsctf_final_board_{}", Uuid::new_v4().simple());
    assert!(schema
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'));
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = crate::migrations::test_pg_connect_options(&database_url)
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"
        CREATE TABLE "Games" (
            id INTEGER PRIMARY KEY,
            end_time_utc TIMESTAMPTZ NOT NULL,
            practice_mode BOOLEAN NOT NULL DEFAULT FALSE
        );
        CREATE TABLE "FinalScoreboardMaterializations" (
            game_id INTEGER PRIMARY KEY REFERENCES "Games"(id) ON DELETE CASCADE,
            game_end_time_utc TIMESTAMPTZ NOT NULL,
            available_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
            invalidated_at_utc TIMESTAMPTZ,
            completed_at_utc TIMESTAMPTZ,
            dead_at_utc TIMESTAMPTZ,
            lease_token UUID,
            lease_expires_at_utc TIMESTAMPTZ,
            attempts INTEGER NOT NULL DEFAULT 0,
            last_error VARCHAR(256),
            updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
            CHECK (attempts >= 0 AND attempts <= 16),
            CHECK ((lease_token IS NULL) = (lease_expires_at_utc IS NULL)),
            CHECK (NOT (completed_at_utc IS NOT NULL AND dead_at_utc IS NOT NULL)),
            CHECK (completed_at_utc IS NULL OR invalidated_at_utc IS NOT NULL),
            CHECK ((completed_at_utc IS NULL AND dead_at_utc IS NULL)
                   OR (lease_token IS NULL AND lease_expires_at_utc IS NULL))
        );
        CREATE INDEX ix_final_scoreboard_materialization_pending
            ON "FinalScoreboardMaterializations" (available_at_utc, game_id)
            WHERE completed_at_utc IS NULL AND dead_at_utc IS NULL;
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "Games" (id, end_time_utc, practice_mode)
           VALUES (17, $1, FALSE), (18, $1, FALSE), (19, $1, TRUE)"#,
    )
    .bind(now - chrono::Duration::minutes(1))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "FinalScoreboardMaterializations"
                  (game_id, game_end_time_utc, available_at_utc)
           SELECT id, end_time_utc, end_time_utc FROM "Games""#,
    )
    .execute(&pool)
    .await
    .unwrap();

    // Two maintenance replicas race. PostgreSQL leases each version once.
    let (left, right) = tokio::join!(claim(&pool, now), claim(&pool, now));
    let batches = [left.unwrap(), right.unwrap()];
    assert_eq!(
        batches.iter().map(|batch| batch.jobs.len()).sum::<usize>(),
        2
    );
    let claimed: Vec<_> = batches
        .iter()
        .flat_map(|batch| {
            batch
                .jobs
                .iter()
                .map(move |job| (batch.lease_token, job.clone()))
        })
        .collect();
    assert_ne!(claimed[0].1.game_id, claimed[1].1.game_id);
    let (first_token, first_job) = claimed
        .iter()
        .find(|(_, job)| job.game_id == 17)
        .cloned()
        .unwrap();
    let (_, restart_job) = claimed
        .iter()
        .find(|(_, job)| job.game_id == 18)
        .cloned()
        .unwrap();
    sqlx::query(
        r#"UPDATE "FinalScoreboardMaterializations"
              SET attempts = 7, invalidated_at_utc = $2
            WHERE game_id = $1"#,
    )
    .bind(restart_job.game_id)
    .bind(now)
    .execute(&pool)
    .await
    .unwrap();
    request_repair(&pool, restart_job.game_id).await.unwrap();
    let (retained_lease, reset_attempts, reset_invalidation): (
        Option<Uuid>,
        i32,
        Option<DateTime<Utc>>,
    ) = sqlx::query_as(
        r#"SELECT lease_token, attempts, invalidated_at_utc
             FROM "FinalScoreboardMaterializations" WHERE game_id = $1"#,
    )
    .bind(restart_job.game_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        retained_lease.is_some(),
        "repair must not steal a live lease"
    );
    assert_eq!(reset_attempts, 0);
    assert!(reset_invalidation.is_none());
    sqlx::query(r#"UPDATE "FinalScoreboardMaterializations" SET attempts = 5 WHERE game_id = 19"#)
        .execute(&pool)
        .await
        .unwrap();
    request_repair(&pool, 19).await.unwrap();
    let practice_attempts: i32 = sqlx::query_scalar(
        r#"SELECT attempts FROM "FinalScoreboardMaterializations" WHERE game_id = 19"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(practice_attempts, 5, "practice repair must be a no-op");

    let cache = CountingCache::new();
    let keys = crate::controllers::game::scoreboard_render_cache_keys(first_job.game_id);
    for key in &keys {
        cache.set(key, b"live", None).await;
    }
    crate::controllers::game::evict_scoreboard_render_cache(&cache, first_job.game_id).await;
    assert_eq!(cache.removals.load(Ordering::SeqCst), keys.len());
    assert!(mark_invalidated(&pool, &first_job, first_token, now)
        .await
        .unwrap());

    let builds = AtomicUsize::new(0);
    publish_missing(&cache, first_job.game_id, &builds)
        .await
        .unwrap();
    assert_eq!(builds.load(Ordering::SeqCst), keys.len());
    assert_eq!(
        retry(
            &pool,
            &first_job,
            first_token,
            now,
            "simulated completion outage"
        )
        .await
        .unwrap(),
        Some(false)
    );

    assert!(claim(&pool, now + chrono::Duration::seconds(59))
        .await
        .unwrap()
        .jobs
        .is_empty());
    let retry_at = now + chrono::Duration::seconds(61);
    let retried = claim(&pool, retry_at).await.unwrap();
    let retried_job = retried
        .jobs
        .iter()
        .find(|job| job.game_id == first_job.game_id)
        .cloned()
        .unwrap();
    assert!(retried_job.invalidated_at_utc.is_some());
    publish_missing(&cache, first_job.game_id, &builds)
        .await
        .unwrap();
    assert_eq!(builds.load(Ordering::SeqCst), keys.len());
    assert_eq!(cache.removals.load(Ordering::SeqCst), keys.len());
    assert!(complete(&pool, &retried_job, retried.lease_token, retry_at)
        .await
        .unwrap());

    let reader_hits = futures::future::join_all((0..256).map(|index| {
        let key = keys[index % keys.len()].clone();
        let cache = &cache;
        async move { cache.get(&key).await.is_some() }
    }))
    .await;
    assert!(reader_hits.into_iter().all(|hit| hit));
    assert_eq!(builds.load(Ordering::SeqCst), keys.len());

    // The second job models a process crash after claim. A different replica
    // recovers it after the bounded lease without manual database changes.
    let restart_at = now + chrono::Duration::seconds(LEASE_SECONDS + 1);
    let restarted = claim(&pool, restart_at).await.unwrap();
    let restarted_job = restarted
        .jobs
        .iter()
        .find(|job| job.game_id == restart_job.game_id)
        .cloned()
        .unwrap();
    let restart_keys =
        crate::controllers::game::scoreboard_render_cache_keys(restarted_job.game_id);
    for key in &restart_keys {
        cache.set(key, b"live", None).await;
    }
    crate::controllers::game::evict_scoreboard_render_cache(&cache, restarted_job.game_id).await;
    assert!(
        mark_invalidated(&pool, &restarted_job, restarted.lease_token, restart_at)
            .await
            .unwrap()
    );
    publish_missing(&cache, restarted_job.game_id, &builds)
        .await
        .unwrap();
    assert_eq!(cache.removals.load(Ordering::SeqCst), keys.len() * 2);
    assert_eq!(builds.load(Ordering::SeqCst), keys.len() * 2);
    assert!(
        complete(&pool, &restarted_job, restarted.lease_token, restart_at)
            .await
            .unwrap()
    );

    let six_hours_later = now + chrono::Duration::hours(6);
    assert!(claim(&pool, six_hours_later).await.unwrap().jobs.is_empty());

    request_repair(&pool, first_job.game_id).await.unwrap();
    let repaired = claim(&pool, six_hours_later + chrono::Duration::seconds(1))
        .await
        .unwrap();
    let repaired_job = repaired
        .jobs
        .iter()
        .find(|job| job.game_id == first_job.game_id)
        .unwrap();
    assert!(repaired_job.invalidated_at_utc.is_none());
    assert_eq!(repaired_job.attempts, 0);

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn real_builders_retry_failed_eviction_then_materialize_once_for_all_readers() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin_options = crate::migrations::test_pg_connect_options(&database_url);
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(admin_options)
        .await
        .unwrap();
    let schema = format!("rsctf_final_real_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = crate::migrations::test_pg_connect_options(&database_url)
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(12)
        .connect_with(options)
        .await
        .unwrap();
    let database = SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone());
    crate::migrations::Migrator::up(&database, None)
        .await
        .unwrap();

    let storage_root = std::env::temp_dir().join(format!(
        "rsctf-final-scoreboard-{}",
        Uuid::new_v4().simple()
    ));
    let mut config = AppConfig::default();
    config.storage_root = storage_root.to_string_lossy().into_owned();
    config.jwt_secret = "0123456789abcdef0123456789abcdef".to_owned();
    let cache = Arc::new(CountingCache::new());
    let state = AppState::new(
        database,
        Arc::new(config),
        cache.clone(),
        Arc::new(LocalBlobStorage::new(storage_root.join("blobs"))),
        TokenService::new("0123456789abcdef0123456789abcdef", 60),
        Arc::new(NoopContainerManager),
    );
    let now = Utc::now();
    async fn insert_final_game(state: &AppState, now: DateTime<Utc>, hidden: bool) -> game::Model {
        let (public_key, private_key) = crate::utils::crypto_utils::generate_game_keypair();
        game::ActiveModel {
            title: Set(format!(
                "Final scoreboard integration ({})",
                if hidden { "hidden" } else { "public" }
            )),
            public_key: Set(public_key),
            private_key: Set(private_key),
            summary: Set(String::new()),
            content: Set(String::new()),
            hidden: Set(hidden),
            practice_mode: Set(false),
            accept_without_review: Set(false),
            allow_user_submissions: Set(false),
            writeup_required: Set(false),
            invite_code: Set(None),
            team_member_count_limit: Set(0),
            container_count_limit: Set(3),
            start_time_utc: Set(now - chrono::Duration::hours(2)),
            end_time_utc: Set(now - chrono::Duration::minutes(1)),
            writeup_deadline: Set(now + chrono::Duration::hours(1)),
            writeup_note: Set(String::new()),
            blood_bonus_value: Set(0),
            ad_allow_snapshot_download: Set(true),
            ad_scoring_paused: Set(false),
            ad_epoch_ticks: Set(8),
            koth_epoch_ticks: Set(12),
            koth_cycle_ticks: Set(3),
            koth_champion_cooldown_ticks: Set(1),
            koth_claim_confirmation_ticks: Set(2),
            ..Default::default()
        }
        .insert(&state.db)
        .await
        .unwrap()
    }

    async fn insert_ad_and_koth_challenges(pool: &PgPool, game_id: i32) {
        sqlx::query(
            r#"INSERT INTO "GameChallenges" (
                 game_id, title, content, category, "Type", is_enabled,
                 submission_limit, accepted_count, submission_count, review_status,
                 build_status, enable_traffic_capture, enable_shared_container,
                 disable_blood_bonus, original_score, min_score_rate, difficulty,
                 score_curve, ad_allow_egress, ad_allow_self_reset,
                 ad_ssh_requires_flag, ad_self_hosted, ad_scoring_weight
               ) VALUES
                 ($1, 'Final A&D service', '', 2, 4, TRUE,
                  0, 0, 0, 0, 0, FALSE, FALSE, FALSE, 1000, 0.25, 5.0,
                  0, FALSE, FALSE, FALSE, TRUE, 1.0),
                 ($1, 'Final KotH hill', '', 0, 5, TRUE,
                  0, 0, 0, 0, 0, FALSE, FALSE, FALSE, 1000, 0.25, 5.0,
                  0, FALSE, FALSE, FALSE, FALSE, 1.0)"#,
        )
        .bind(game_id)
        .execute(pool)
        .await
        .unwrap();
    }

    let public_game = insert_final_game(&state, now, false).await;
    let hidden_game = insert_final_game(&state, now, true).await;
    insert_ad_and_koth_challenges(&pool, public_game.id).await;
    insert_ad_and_koth_challenges(&pool, hidden_game.id).await;

    cache.reject_removals(true);
    let failed = materialize_pending(&state).await.unwrap();
    assert_eq!(
        (failed.claimed, failed.retried, failed.completed),
        (2, 2, 0)
    );
    assert_eq!(cache.removals.load(Ordering::SeqCst), 24);
    assert_eq!(cache.sets.load(Ordering::SeqCst), 0);
    let invalidated: Vec<Option<DateTime<Utc>>> = sqlx::query_scalar(
        r#"SELECT invalidated_at_utc FROM "FinalScoreboardMaterializations"
            WHERE game_id = ANY($1) ORDER BY game_id"#,
    )
    .bind(vec![public_game.id, hidden_game.id])
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(invalidated, vec![None, None]);

    cache.reject_removals(false);
    sqlx::query(
        r#"UPDATE "FinalScoreboardMaterializations"
              SET available_at_utc = clock_timestamp()
            WHERE game_id = ANY($1)"#,
    )
    .bind(vec![public_game.id, hidden_game.id])
    .execute(&pool)
    .await
    .unwrap();
    let (left, right) = tokio::join!(materialize_pending(&state), materialize_pending(&state));
    let reports = [left.unwrap(), right.unwrap()];
    assert_eq!(
        reports.iter().map(|report| report.claimed).sum::<usize>(),
        2
    );
    assert_eq!(
        reports.iter().map(|report| report.completed).sum::<usize>(),
        2
    );
    assert_eq!(cache.removals.load(Ordering::SeqCst), 48);
    assert_eq!(cache.sets.load(Ordering::SeqCst), 18);

    let public_keys = crate::controllers::game::scoreboard_render_cache_keys(public_game.id);
    for key in &public_keys {
        assert!(
            cache.get_authoritative(key).await.is_some(),
            "missing {key}"
        );
    }
    let hidden_keys = crate::controllers::game::scoreboard_render_cache_keys(hidden_game.id);
    for (index, key) in hidden_keys.iter().enumerate() {
        let should_exist = matches!(index, 0 | 2 | 3 | 6 | 8 | 10);
        assert_eq!(
            cache.get_authoritative(key).await.is_some(),
            should_exist,
            "wrong hidden-event publication state for {key}"
        );
    }
    let readable_keys: Vec<_> = public_keys
        .iter()
        .cloned()
        .chain(
            hidden_keys
                .iter()
                .enumerate()
                .filter(|(index, _)| matches!(index, 0 | 2 | 3 | 6 | 8 | 10))
                .map(|(_, key)| key.clone()),
        )
        .collect();
    let hits = futures::future::join_all((0..256).map(|index| {
        let cache = cache.clone();
        let key = readable_keys[index % readable_keys.len()].clone();
        async move { cache.get(&key).await.is_some() }
    }))
    .await;
    assert!(hits.into_iter().all(|hit| hit));
    assert_eq!(cache.sets.load(Ordering::SeqCst), 18);
    assert_eq!(materialize_pending(&state).await.unwrap().claimed, 0);

    drop(state);
    pool.close().await;
    if tokio::fs::try_exists(&storage_root).await.unwrap_or(false) {
        tokio::fs::remove_dir_all(&storage_root).await.unwrap();
    }
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
}

#[tokio::test]
async fn redis_replicas_share_and_evict_the_same_final_render_family() {
    let Ok(url) = std::env::var("RSCTF_TEST_REDIS_URL") else {
        return;
    };
    let replica_a = RedisCache::connect(&url).await.unwrap();
    let replica_b = RedisCache::connect(&url).await.unwrap();
    let nonce = (Uuid::new_v4().as_u128() % 500_000_000) as i32;
    let game_id = -1_000_000_000 - nonce;
    let keys = crate::controllers::game::scoreboard_render_cache_keys(game_id);
    for key in &keys {
        replica_a.remove(key).await;
        replica_a
            .set(
                key,
                b"immutable-final-board",
                Some(crate::controllers::game::FINAL_SCOREBOARD_CACHE_TTL),
            )
            .await;
    }
    for key in &keys {
        assert_eq!(
            replica_b.get(key).await.as_deref(),
            Some(b"immutable-final-board".as_slice())
        );
    }
    crate::controllers::game::evict_scoreboard_render_cache(&replica_b, game_id).await;
    for key in &keys {
        assert!(
            replica_a.get(key).await.is_none(),
            "{key} survived Redis eviction"
        );
    }
}
