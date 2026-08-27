use std::collections::HashSet;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use sea_orm::SqlxPostgresConnector;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use super::deletion::{delete_expected_team_container_locked, DeleteContainerOutcome};
use super::extension::extend_expected_team_container_locked;
use crate::app_state::{AppState, SharedState};
use crate::models::internal::configs::AppConfig;
use crate::services::cache::InMemoryCache;
use crate::services::container::{
    ContainerInfo, ContainerManager, ContainerSpec, ContainerStatus as RuntimeStatus,
};
use crate::services::token::TokenService;
use crate::storage::LocalBlobStorage;
use crate::utils::enums::{ContainerStatus, NetworkMode};
use crate::utils::error::{AppError, AppResult};

#[derive(Default)]
struct RecordingContainerManager {
    live: Mutex<HashSet<String>>,
    destroyed: Mutex<Vec<String>>,
}

impl RecordingContainerManager {
    fn publish(&self, id: &str) {
        self.live.lock().unwrap().insert(id.to_string());
    }

    fn destroyed(&self) -> Vec<String> {
        self.destroyed.lock().unwrap().clone()
    }
}

#[async_trait]
impl ContainerManager for RecordingContainerManager {
    async fn create(&self, _spec: ContainerSpec) -> AppResult<ContainerInfo> {
        Err(AppError::bad_request("test backend does not create"))
    }

    async fn destroy(&self, id: &str) -> AppResult<()> {
        self.live.lock().unwrap().remove(id);
        self.destroyed.lock().unwrap().push(id.to_string());
        Ok(())
    }

    async fn query(&self, id: &str) -> AppResult<RuntimeStatus> {
        if !self.live.lock().unwrap().contains(id) {
            return Err(AppError::not_found("test runtime not found"));
        }
        Ok(RuntimeStatus {
            id: id.to_string(),
            status: "running".to_string(),
            memory_bytes: None,
            cpu_usage: None,
        })
    }
}

struct Harness {
    admin: sqlx::PgPool,
    pool: sqlx::PgPool,
    schema: String,
    state: SharedState,
}

impl Harness {
    async fn new(containers: Arc<dyn ContainerManager>) -> Self {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!(
            "rsctf_test_conditional_delete_{}",
            uuid::Uuid::new_v4().simple()
        );
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "GameInstances" (
                id INTEGER PRIMARY KEY,
                challenge_id INTEGER NOT NULL,
                participation_id INTEGER NOT NULL,
                is_loaded BOOLEAN NOT NULL,
                last_container_operation TIMESTAMPTZ NOT NULL,
                flag_id INTEGER,
                container_id UUID
            );
            CREATE UNIQUE INDEX "uq_conditional_delete_owner"
                ON "GameInstances" (participation_id, challenge_id);
            CREATE TABLE "Containers" (
                id UUID PRIMARY KEY,
                image TEXT NOT NULL,
                container_id TEXT NOT NULL,
                status SMALLINT NOT NULL,
                started_at TIMESTAMPTZ NOT NULL,
                expect_stop_at TIMESTAMPTZ NOT NULL,
                is_proxy BOOLEAN NOT NULL,
                ip TEXT NOT NULL,
                port INTEGER NOT NULL,
                public_ip TEXT,
                public_port INTEGER,
                game_instance_id INTEGER,
                exercise_instance_id INTEGER,
                ad_team_service_id INTEGER
            );
            CREATE TABLE "GameChallenges" (
                id INTEGER PRIMARY KEY,
                game_id INTEGER NOT NULL,
                enable_traffic_capture BOOLEAN NOT NULL DEFAULT FALSE,
                ad_self_hosted BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TABLE "AdTeamServices" (
                id INTEGER PRIMARY KEY,
                game_id INTEGER NOT NULL,
                challenge_id INTEGER NOT NULL,
                container_id TEXT,
                host TEXT,
                port INTEGER NOT NULL DEFAULT 0
            );
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let database = SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone());
        let storage_root = std::env::temp_dir().join(format!(
            "rsctf-conditional-delete-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let mut config = AppConfig::default();
        config.storage_root = storage_root.to_string_lossy().into_owned();
        config.jwt_secret = "0123456789abcdef0123456789abcdef".to_string();
        let state = AppState::new(
            database,
            Arc::new(config),
            Arc::new(InMemoryCache::new()),
            Arc::new(LocalBlobStorage::new(storage_root.join("blobs"))),
            TokenService::new("0123456789abcdef0123456789abcdef", 60),
            containers,
        );
        Self {
            admin,
            pool,
            schema,
            state,
        }
    }

    async fn insert_container(&self, id: uuid::Uuid, backend_id: &str) {
        sqlx::query(
            r#"INSERT INTO "Containers"
                (id, image, container_id, status, started_at, expect_stop_at,
                 is_proxy, ip, port, public_ip, public_port, game_instance_id,
                 exercise_instance_id, ad_team_service_id)
               VALUES ($1, 'sha256:test', $2, $3, clock_timestamp(),
                       clock_timestamp() + interval '1 hour', FALSE,
                       '127.0.0.1', 31337, NULL, NULL, NULL, NULL, NULL)"#,
        )
        .bind(id)
        .bind(backend_id)
        .bind(ContainerStatus::Running as i16)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    async fn insert_instance(
        &self,
        id: i32,
        participation_id: i32,
        challenge_id: i32,
        container_id: uuid::Uuid,
    ) {
        sqlx::query(
            r#"INSERT INTO "GameInstances"
                (id, challenge_id, participation_id, is_loaded,
                 last_container_operation, flag_id, container_id)
               VALUES ($1, $2, $3, TRUE,
                       clock_timestamp() - interval '1 minute', NULL, $4)"#,
        )
        .bind(id)
        .bind(challenge_id)
        .bind(participation_id)
        .bind(container_id)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    async fn replace(&self, instance_id: i32, container_id: uuid::Uuid) {
        sqlx::query(
            r#"UPDATE "GameInstances"
                  SET container_id = $2,
                      is_loaded = TRUE,
                      last_container_operation = clock_timestamp() - interval '1 minute'
                WHERE id = $1"#,
        )
        .bind(instance_id)
        .bind(container_id)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    async fn current(&self, instance_id: i32) -> Option<uuid::Uuid> {
        sqlx::query_scalar(r#"SELECT container_id FROM "GameInstances" WHERE id = $1"#)
            .bind(instance_id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn set_extension_eligible(
        &self,
        container_id: uuid::Uuid,
    ) -> chrono::DateTime<chrono::Utc> {
        sqlx::query_scalar(
            r#"UPDATE "Containers"
                  SET expect_stop_at = clock_timestamp() + interval '1 minute'
                WHERE id = $1
            RETURNING expect_stop_at"#,
        )
        .bind(container_id)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn expect_stop_at(&self, container_id: uuid::Uuid) -> chrono::DateTime<chrono::Utc> {
        sqlx::query_scalar(r#"SELECT expect_stop_at FROM "Containers" WHERE id = $1"#)
            .bind(container_id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn cleanup(self) {
        let Self {
            admin,
            pool,
            schema,
            state,
        } = self;
        drop(state);
        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}

async fn locked_delete(
    state: &SharedState,
    participation_id: i32,
    challenge_id: i32,
    expected_container_id: uuid::Uuid,
) -> AppResult<DeleteContainerOutcome> {
    let key = format!("game-container:{participation_id}");
    let lock = crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(state.pg(), &key)
        .await
        .map_err(AppError::from)?;
    let result = delete_expected_team_container_locked(
        state,
        participation_id,
        challenge_id,
        expected_container_id,
    )
    .await;
    let released = lock.release().await.map_err(AppError::from);
    match (result, released) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(outcome), Ok(())) => Ok(outcome),
    }
}

async fn locked_extend(
    state: &SharedState,
    participation_id: i32,
    challenge_id: i32,
    expected_container_id: uuid::Uuid,
) -> AppResult<super::ContainerInfoModel> {
    let key = format!("game-container:{participation_id}");
    let lock = crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(state.pg(), &key)
        .await
        .map_err(AppError::from)?;
    let result = extend_expected_team_container_locked(
        state,
        participation_id,
        challenge_id,
        expected_container_id,
        &crate::services::container_policy::ContainerPolicy::default(),
    )
    .await;
    let released = lock.release().await.map_err(AppError::from);
    match (result, released) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(container), Ok(())) => Ok(container),
    }
}

#[test]
fn lifecycle_contract_requires_one_camel_case_container_uuid() {
    let id = uuid::Uuid::new_v4();
    let uri = format!("/?expectedContainerId={id}").parse().unwrap();
    let axum::extract::Query(parsed) =
        axum::extract::Query::<super::ExpectedContainerQuery>::try_from_uri(&uri).unwrap();
    assert_eq!(parsed.expected_container_id, id);
    assert!(
        axum::extract::Query::<super::ExpectedContainerQuery>::try_from_uri(&"/".parse().unwrap())
            .is_err()
    );
    assert!(
        axum::extract::Query::<super::ExpectedContainerQuery>::try_from_uri(
            &"/?expectedContainerId=not-a-uuid".parse().unwrap()
        )
        .is_err()
    );
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn delayed_extension_cannot_extend_a_replacement_after_waiting_for_its_lock() {
    let manager = Arc::new(RecordingContainerManager::default());
    let harness = Harness::new(manager).await;
    let stale = uuid::Uuid::new_v4();
    let replacement = uuid::Uuid::new_v4();
    harness
        .insert_container(stale, "runtime-stale-extend")
        .await;
    harness
        .insert_container(replacement, "runtime-replacement-extend")
        .await;
    harness.insert_instance(31, 81, 91, stale).await;
    let replacement_expiry = harness.set_extension_eligible(replacement).await;

    // Model request A arriving while the replacement owner holds the exact
    // provisioning lock. The owner publishes B before releasing it; A must then
    // compare its immutable precondition against the post-lock row.
    let key = "game-container:81";
    let replacement_owner =
        crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(harness.state.pg(), key)
            .await
            .unwrap();
    let (request_started_tx, request_started_rx) = tokio::sync::oneshot::channel();
    let delayed = tokio::spawn({
        let state = harness.state.clone();
        async move {
            request_started_tx.send(()).unwrap();
            locked_extend(&state, 81, 91, stale).await
        }
    });
    request_started_rx.await.unwrap();
    harness.replace(31, replacement).await;
    replacement_owner.release().await.unwrap();

    let error = tokio::time::timeout(std::time::Duration::from_secs(5), delayed)
        .await
        .expect("delayed extension did not resume after replacement")
        .unwrap()
        .unwrap_err();
    assert!(matches!(error, AppError::Conflict(_)));
    assert_eq!(harness.current(31).await, Some(replacement));
    assert_eq!(
        harness.expect_stop_at(replacement).await,
        replacement_expiry,
        "the stale request extended replacement B"
    );
    harness.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn conditional_delete_preserves_replacement_and_is_successful_idempotent_and_concurrent() {
    let manager = Arc::new(RecordingContainerManager::default());
    let harness = Harness::new(manager.clone()).await;

    let stale = uuid::Uuid::new_v4();
    let replacement = uuid::Uuid::new_v4();
    manager.publish("runtime-stale");
    manager.publish("runtime-replacement");
    harness.insert_container(stale, "runtime-stale").await;
    harness
        .insert_container(replacement, "runtime-replacement")
        .await;
    harness.insert_instance(11, 41, 51, stale).await;
    harness.replace(11, replacement).await;

    let stale_error = locked_delete(&harness.state, 41, 51, stale)
        .await
        .unwrap_err();
    assert!(matches!(stale_error, AppError::Conflict(_)));
    assert_eq!(harness.current(11).await, Some(replacement));
    assert!(manager.destroyed().is_empty());

    let successful = uuid::Uuid::new_v4();
    manager.publish("runtime-successful");
    harness
        .insert_container(successful, "runtime-successful")
        .await;
    harness.insert_instance(12, 42, 52, successful).await;
    assert!(matches!(
        locked_delete(&harness.state, 42, 52, successful)
            .await
            .unwrap(),
        DeleteContainerOutcome::Destroyed { .. }
    ));
    assert_eq!(
        locked_delete(&harness.state, 42, 52, successful)
            .await
            .unwrap(),
        DeleteContainerOutcome::AlreadyAbsent
    );

    let concurrent = uuid::Uuid::new_v4();
    manager.publish("runtime-concurrent");
    harness
        .insert_container(concurrent, "runtime-concurrent")
        .await;
    harness.insert_instance(13, 43, 53, concurrent).await;
    let first = tokio::spawn({
        let state = harness.state.clone();
        async move { locked_delete(&state, 43, 53, concurrent).await }
    });
    let second = tokio::spawn({
        let state = harness.state.clone();
        async move { locked_delete(&state, 43, 53, concurrent).await }
    });
    let (first, second) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(first, second)
    })
    .await
    .expect("concurrent conditional deletes did not converge");
    let outcomes = [first.unwrap().unwrap(), second.unwrap().unwrap()];
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, DeleteContainerOutcome::Destroyed { .. }))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, DeleteContainerOutcome::AlreadyAbsent))
            .count(),
        1
    );

    let destroyed = manager.destroyed();
    assert_eq!(
        destroyed
            .iter()
            .filter(|id| id.as_str() == "runtime-successful")
            .count(),
        1
    );
    assert_eq!(
        destroyed
            .iter()
            .filter(|id| id.as_str() == "runtime-concurrent")
            .count(),
        1
    );
    assert!(!destroyed.iter().any(|id| id == "runtime-replacement"));
    harness.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL, local Docker, and RSCTF_TEST_CONTAINER_IMAGE"]
async fn stale_conditional_delete_cannot_destroy_a_real_replacement_runtime() {
    let image = std::env::var("RSCTF_TEST_CONTAINER_IMAGE")
        .expect("RSCTF_TEST_CONTAINER_IMAGE must be an immutable local image ID");
    let manager = crate::services::container::from_env_required().unwrap();
    let operation = uuid::Uuid::new_v4();
    let spec = |suffix: &str| ContainerSpec {
        game_kind: rsctf_worker_protocol::GameKind::Jeopardy,
        image: image.clone(),
        memory_limit: 64,
        cpu_count: 1,
        storage_limit: 32,
        expose_port: 8080,
        publish_port: false,
        proxy_only: false,
        env: Vec::new(),
        flag: None,
        ad_network: None,
        allow_egress: false,
        network_mode: NetworkMode::Isolated,
        operation_id: Some(format!("conditional-delete-{operation}-{suffix}")),
    };
    let stale_runtime = manager.create(spec("stale")).await.unwrap();
    let replacement_runtime = match manager.create(spec("replacement")).await {
        Ok(runtime) => runtime,
        Err(error) => {
            manager.destroy(&stale_runtime.id).await.unwrap();
            panic!("replacement runtime creation failed: {error}");
        }
    };
    let harness = Harness::new(manager.clone()).await;
    let stale = uuid::Uuid::new_v4();
    let replacement = uuid::Uuid::new_v4();
    harness.insert_container(stale, &stale_runtime.id).await;
    harness
        .insert_container(replacement, &replacement_runtime.id)
        .await;
    harness.insert_instance(21, 61, 71, stale).await;
    harness.replace(21, replacement).await;

    let result = locked_delete(&harness.state, 61, 71, stale).await;
    let replacement_status = manager.query(&replacement_runtime.id).await;
    let stale_status = manager.query(&stale_runtime.id).await;
    manager.destroy(&stale_runtime.id).await.unwrap();
    manager.destroy(&replacement_runtime.id).await.unwrap();
    harness.cleanup().await;

    assert!(matches!(result, Err(AppError::Conflict(_))));
    assert_eq!(replacement_status.unwrap().status, "running");
    assert_eq!(stale_status.unwrap().status, "running");
}
