use super::super::*;
use super::import_with_game_lock;
use crate::app_state::AppState;
use crate::models::data::{game, repo_binding};
use crate::models::internal::configs::AppConfig;
use crate::services::cache::InMemoryCache;
use crate::services::container::NoopContainerManager;
use crate::services::token::TokenService;
use crate::storage::LocalBlobStorage;
use crate::utils::enums::{ChallengeBuildStatus, RepoWatchStatus};
use sea_orm::SqlxPostgresConnector;
use sea_orm_migration::MigratorTrait;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn repository_generator_source_queues_once_and_freezes_at_event_start() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(crate::migrations::test_pg_connect_options(&database_url))
        .await
        .expect("connect test database");
    let schema = format!("variant_generator_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .expect("create isolated schema");
    let options = crate::migrations::test_pg_connect_options(&database_url)
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(6)
        .connect_with(options)
        .await
        .expect("connect isolated pool");
    let database = SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone());
    crate::migrations::Migrator::up(&database, None)
        .await
        .expect("migrate isolated schema");

    let root = std::env::temp_dir().join(format!(
        "rsctf-generator-regression-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let mut config = AppConfig::default();
    config.storage_root = root.to_string_lossy().into_owned();
    config.jwt_secret = "0123456789abcdef0123456789abcdef".to_string();
    config.event_vpn_credential_key = "0123456789abcdef0123456789abcdef".to_string();
    let state = AppState::new(
        database,
        Arc::new(config),
        Arc::new(InMemoryCache::new()),
        Arc::new(LocalBlobStorage::new(root.join("blobs"))),
        TokenService::new("0123456789abcdef0123456789abcdef", 60),
        Arc::new(NoopContainerManager),
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
    .expect("insert binding");
    let (public_key, private_key) = crate::utils::crypto_utils::generate_game_keypair();
    let now = Utc::now();
    let game = game::ActiveModel {
        title: Set("Generator event".to_string()),
        public_key: Set(public_key),
        private_key: Set(private_key),
        summary: Set(String::new()),
        content: Set(String::new()),
        hidden: Set(false),
        practice_mode: Set(false),
        accept_without_review: Set(false),
        allow_user_submissions: Set(false),
        writeup_required: Set(false),
        invite_code: Set(None),
        team_member_count_limit: Set(0),
        container_count_limit: Set(0),
        start_time_utc: Set(now + chrono::Duration::hours(1)),
        end_time_utc: Set(now + chrono::Duration::hours(2)),
        writeup_deadline: Set(now + chrono::Duration::hours(2)),
        writeup_note: Set(String::new()),
        blood_bonus_value: Set(0),
        repo_binding_id: Set(Some(binding.id)),
        event_manifest_path: Set(Some("event/.gzevent".to_string())),
        challenge_configuration_revision: Set(1),
        configuration_revision: Set(0),
        ad_allow_snapshot_download: Set(false),
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

    let challenge_dir = root
        .join("repos")
        .join(binding.id.to_string())
        .join("event/misc/generated");
    tokio::fs::create_dir_all(challenge_dir.join("generator"))
        .await
        .unwrap();
    tokio::fs::create_dir_all(challenge_dir.join("dist"))
        .await
        .unwrap();
    tokio::fs::write(challenge_dir.join("dist/README.txt"), "generated per team")
        .await
        .unwrap();
    tokio::fs::write(
        challenge_dir.join("generator/Dockerfile"),
        "FROM scratch\nCOPY generate /generate\nENTRYPOINT [\"/generate\"]\n",
    )
    .await
    .unwrap();
    let generator = challenge_dir.join("generator/generate");
    tokio::fs::write(&generator, "revision-one\n")
        .await
        .unwrap();
    let manifest = challenge_dir.join("challenge.yaml");
    tokio::fs::write(
        &manifest,
        "name: Generated\ntype: StaticAttachment\ncategory: Misc\nprovide: dist\nvariantMode: PerParticipation\nsolveReceiptMode: Disabled\n",
    )
    .await
    .unwrap();

    let first = import_with_game_lock(&state, game.id, &manifest)
        .await
        .expect("import auto-built generator");
    assert!(first.created);
    assert!(!first.build_queued);
    assert!(first.generator_build_queued);
    let challenge_id = first.challenge_id;
    let queued = game_challenge::Entity::find_by_id(challenge_id)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        queued.variant_generator_build_context_subdir.as_deref(),
        Some(GENERATOR_CONTEXT_SUBDIR)
    );
    assert_eq!(
        queued.variant_generator_build_status,
        ChallengeBuildStatus::Queued
    );
    assert_eq!(queued.variant_generator_image, None);
    assert_eq!(queued.variant_generator_digest, None);
    assert!(queued.original_archive_blob_path.is_some());

    let digest = format!("sha256:{}", "a".repeat(64));
    sqlx::query(
        r#"UPDATE "GameChallenges"
              SET variant_generator_build_status = $2,
                  variant_generator_image = $3,
                  variant_generator_digest = $3,
                  variant_generator_last_build_log = 'contract passed'
            WHERE id = $1"#,
    )
    .bind(challenge_id)
    .bind(ChallengeBuildStatus::Success as i16)
    .bind(&digest)
    .execute(state.pg())
    .await
    .unwrap();
    let unchanged = import_with_game_lock(&state, game.id, &manifest)
        .await
        .expect("unchanged generator reuses immutable identity");
    assert!(!unchanged.generator_build_queued);
    let stable = game_challenge::Entity::find_by_id(challenge_id)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stable.variant_generator_digest.as_deref(), Some(&*digest));

    tokio::fs::write(&generator, "revision-two\n")
        .await
        .unwrap();
    let changed = import_with_game_lock(&state, game.id, &manifest)
        .await
        .expect("changed generator queues a new build");
    assert!(changed.generator_build_queued);
    let changed_row = game_challenge::Entity::find_by_id(challenge_id)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        changed_row.variant_generator_build_status,
        ChallengeBuildStatus::Queued
    );
    assert_eq!(changed_row.variant_generator_digest, None);

    sqlx::query(
        r#"UPDATE "GameChallenges"
              SET variant_generator_build_status = $2,
                  variant_generator_image = $3,
                  variant_generator_digest = $3
            WHERE id = $1"#,
    )
    .bind(challenge_id)
    .bind(ChallengeBuildStatus::Success as i16)
    .bind(&digest)
    .execute(state.pg())
    .await
    .unwrap();
    sqlx::query(r#"UPDATE "Games" SET start_time_utc = clock_timestamp() - INTERVAL '1 second' WHERE id = $1"#)
        .bind(game.id)
        .execute(state.pg())
        .await
        .unwrap();
    tokio::fs::write(&generator, "revision-three\n")
        .await
        .unwrap();
    assert!(import_with_game_lock(&state, game.id, &manifest)
        .await
        .unwrap_err()
        .to_string()
        .contains("frozen at event start"));
    let frozen = game_challenge::Entity::find_by_id(challenge_id)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        frozen.variant_generator_build_status,
        ChallengeBuildStatus::Success
    );
    assert_eq!(frozen.variant_generator_digest.as_deref(), Some(&*digest));

    drop(state);
    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    let _ = tokio::fs::remove_dir_all(root).await;
}
