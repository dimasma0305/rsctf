use std::{path::PathBuf, sync::Arc};

use chrono::Utc;
use sea_orm::{ActiveModelTrait, Set, SqlxPostgresConnector};
use sea_orm_migration::MigratorTrait;

use crate::app_state::{AppState, SharedState};
use crate::models::{
    data::{game, repo_binding},
    internal::configs::AppConfig,
};
use crate::services::{cache::InMemoryCache, container::NoopContainerManager, token::TokenService};
use crate::storage::LocalBlobStorage;
use crate::utils::{enums::RepoWatchStatus, test_database::TestDatabase};

pub(super) struct RepositoryFixture {
    pub(super) test_db: TestDatabase,
    pub(super) state: SharedState,
    pub(super) root: PathBuf,
    pub(super) binding: repo_binding::Model,
    pub(super) game: game::Model,
    pub(super) now: chrono::DateTime<Utc>,
}

pub(super) async fn repository_fixture() -> RepositoryFixture {
    let test_db = crate::utils::test_database::TestDatabase::new("repo_sync").await;
    let database = SqlxPostgresConnector::from_sqlx_postgres_pool(test_db.pool.clone());
    crate::migrations::Migrator::up(&database, None)
        .await
        .expect("migrate isolated schema");
    let root = std::env::temp_dir().join(format!("rsctf-repo-rescan-{}", uuid::Uuid::new_v4()));
    let mut config = AppConfig::default();
    config.storage_root = root.to_string_lossy().into_owned();
    config.jwt_secret = "0123456789abcdef0123456789abcdef".to_string();
    config.event_vpn_credential_key = "0123456789abcdef0123456789abcdef".into();
    let state = AppState::new(
        database,
        Arc::new(config),
        Arc::new(InMemoryCache::new()),
        Arc::new(LocalBlobStorage::new(root.join("blobs"))),
        TokenService::new("0123456789abcdef0123456789abcdef", 60),
        Arc::new(crate::services::container::profiles::ProfileRouter::new(
            Arc::new(NoopContainerManager),
            crate::services::container::profiles::config::parse(r#"[
                {"id":"docker","name":"Docker","backend":"docker","isolation":"container"},
                {"id":"microvm","name":"MicroVM","backend":"kubernetes","isolation":"microvm","runtimeClass":"kata"}
            ]"#).unwrap().into_iter().map(|profile| (profile, Arc::new(NoopContainerManager) as Arc<dyn crate::services::container::ContainerManager>)).collect(),
        ).unwrap()),
    );
    let binding = repo_binding::ActiveModel {
        repo_url: Set("https://github.com/example/challenges.git".to_string()),
        git_ref: Set(Some("main".to_string())),
        github_token: Set(None),
        interval_seconds: Set(60),
        status: Set(RepoWatchStatus::Active),
        last_commit_sha: Set(None),
        last_scan_message: Set(None),
        last_scan_utc: Set(None),
        next_scan_utc: Set(None),
        created_at_utc: Set(Utc::now()),
        push_on_edit: Set(false),
        ..Default::default()
    }
    .insert(&state.db)
    .await
    .expect("insert repository binding");
    let (public_key, private_key) = crate::utils::crypto_utils::generate_game_keypair();
    let now = Utc::now();
    let game = game::ActiveModel {
        title: Set("Repository event".to_string()),
        public_key: Set(public_key),
        private_key: Set(private_key),
        summary: Set(String::new()),
        content: Set(String::new()),
        hidden: Set(false),
        practice_mode: Set(true),
        accept_without_review: Set(false),
        allow_user_submissions: Set(false),
        writeup_required: Set(false),
        invite_code: Set(None),
        team_member_count_limit: Set(0),
        container_count_limit: Set(3),
        start_time_utc: Set(now + chrono::Duration::hours(1)),
        end_time_utc: Set(now + chrono::Duration::hours(2)),
        writeup_deadline: Set(now + chrono::Duration::hours(1)),
        writeup_note: Set(String::new()),
        blood_bonus_value: Set(0),
        repo_binding_id: Set(Some(binding.id)),
        event_manifest_path: Set(Some("event/.gzevent".to_string())),
        challenge_configuration_revision: Set(1),
        configuration_revision: Set(0),
        ad_allow_snapshot_download: Set(true),
        ad_scoring_paused: Set(false),
        ad_control_revision: Set(1),
        ad_epoch_ticks: Set(8),
        koth_epoch_ticks: Set(12),
        koth_cycle_ticks: Set(3),
        koth_champion_cooldown_ticks: Set(1),
        koth_claim_confirmation_ticks: Set(2),
        ..Default::default()
    }
    .insert(&state.db)
    .await
    .expect("insert game");
    RepositoryFixture {
        test_db,
        state,
        root,
        binding,
        game,
        now,
    }
}
