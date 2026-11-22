use crate::app_state::{AppState, SharedState};
use crate::models::internal::configs::AppConfig;
use crate::services::cache::InMemoryCache;
use crate::services::container::{capacity, ContainerManager, DockerContainerManager};
use crate::services::token::TokenService;
use crate::storage::LocalBlobStorage;
use sea_orm::SqlxPostgresConnector;
use std::{process::Command, sync::Arc};
use uuid::Uuid;

pub(crate) struct LiveService(pub(crate) String);

impl Drop for LiveService {
    fn drop(&mut self) {
        let _ = Command::new("docker").args(["rm", "-fv", &self.0]).output();
    }
}

pub(crate) fn docker(args: &[&str]) -> String {
    let output = Command::new("docker").args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

pub(crate) fn docker_state(pool: &sqlx::PgPool) -> SharedState {
    let manager = capacity::CapacityGatedDockerManager::from_env(
        DockerContainerManager::connect().unwrap(),
        pool.clone(),
    )
    .unwrap();
    runtime_state(pool, Arc::new(manager))
}

pub(crate) fn runtime_state(
    pool: &sqlx::PgPool,
    manager: Arc<dyn ContainerManager>,
) -> SharedState {
    AppState::new(
        SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone()),
        Arc::new(AppConfig::default()),
        Arc::new(InMemoryCache::new()),
        Arc::new(LocalBlobStorage::new(
            std::env::temp_dir().join(format!("rsctf-reset-{}", Uuid::new_v4())),
        )),
        // The live flows span several image replacements.
        TokenService::new("0123456789abcdef0123456789abcdef", 15 * 60),
        manager,
    )
}
