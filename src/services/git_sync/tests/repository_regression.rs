use super::super::*;
use super::{import_with_game_lock, repository_concurrency};
use crate::app_state::AppState;
use crate::models::data::{
    first_solve, game_instance, participation, repo_binding, submission, team, user,
};
use crate::models::internal::configs::AppConfig;
use crate::services::cache::InMemoryCache;
use crate::services::container::NoopContainerManager;
use crate::services::token::TokenService;
use crate::storage::LocalBlobStorage;
use crate::utils::enums::{
    AnswerResult, ChallengeCategory, ParticipationStatus, RepoWatchStatus, Role,
};
use sea_orm::SqlxPostgresConnector;
use sea_orm_migration::MigratorTrait;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;
use std::sync::Arc;

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn repository_update_preserves_challenge_solves_and_refreshes_content() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect test database");
    let schema = format!("rsctf_repo_sync_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin_pool)
        .await
        .expect("create isolated schema");
    let options = PgConnectOptions::from_str(&database_url)
        .expect("parse test database URL")
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect_with(options)
        .await
        .expect("connect isolated pool");
    let database = SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone());
    crate::migrations::Migrator::up(&database, None)
        .await
        .expect("migrate isolated schema");

    let root = std::env::temp_dir().join(format!("rsctf-repo-rescan-{}", uuid::Uuid::new_v4()));
    let mut config = AppConfig::default();
    config.storage_root = root.to_string_lossy().into_owned();
    config.jwt_secret = "0123456789abcdef0123456789abcdef".to_string();
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
        // Pre-start practice alone must not freeze repository grading.
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
    .expect("insert game");
    let challenge_dir = root
        .join("repos")
        .join(binding.id.to_string())
        .join("event/web");
    tokio::fs::create_dir_all(&challenge_dir)
        .await
        .expect("create challenge directory");
    let manifest = challenge_dir.join("challenge.yaml");
    let handout = challenge_dir.join("handout.txt");
    tokio::fs::write(&handout, b"first handout")
        .await
        .expect("write first handout");
    tokio::fs::write(
        &manifest,
        "name: Original\ndescription: before sync\ntype: StaticAttachment\ncategory: Misc\nprovide: handout.txt\nflags:\n  - flag{old}\n",
    )
    .await
    .expect("write first manifest");

    let first = import_with_game_lock(&state, game.id, &manifest)
        .await
        .expect("initial import");
    assert!(first.created);
    let challenge_id = first.challenge_id;
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            r#"SELECT source_yaml_path FROM "GameChallenges" WHERE id = $1"#,
        )
        .bind(challenge_id)
        .fetch_one(state.pg())
        .await
        .unwrap(),
        format!("binding/{}/event/web/challenge.yaml", binding.id)
    );
    let (first_attachment_id, first_attachment_hash) = sqlx::query_as::<_, (i32, String)>(
        r#"SELECT attachment.id, file.hash
             FROM "GameChallenges" challenge
             JOIN "Attachments" attachment ON attachment.id = challenge.attachment_id
             JOIN "Files" file ON file.id = attachment.local_file_id
            WHERE challenge.id = $1"#,
    )
    .bind(challenge_id)
    .fetch_one(state.pg())
    .await
    .expect("load first attachment");

    let user_id = uuid::Uuid::new_v4();
    user::ActiveModel {
        id: Set(user_id),
        user_name: Set(Some("solver".to_string())),
        normalized_user_name: Set(Some("SOLVER".to_string())),
        email: Set(Some("solver@example.test".to_string())),
        normalized_email: Set(Some("SOLVER@EXAMPLE.TEST".to_string())),
        email_confirmed: Set(true),
        password_hash: Set(None),
        security_stamp: Set(Some("stamp".to_string())),
        concurrency_stamp: Set(None),
        phone_number: Set(None),
        phone_number_confirmed: Set(false),
        two_factor_enabled: Set(false),
        lockout_end: Set(None),
        lockout_enabled: Set(false),
        access_failed_count: Set(0),
        role: Set(Role::User),
        ip: Set(String::new()),
        browser_fingerprint: Set(None),
        last_signed_in_utc: Set(now),
        last_visited_utc: Set(now),
        register_time_utc: Set(now),
        bio: Set(String::new()),
        real_name: Set(String::new()),
        std_number: Set(String::new()),
        exercise_visible: Set(true),
        avatar_hash: Set(None),
    }
    .insert(&state.db)
    .await
    .expect("insert solver");
    let team = team::ActiveModel {
        name: Set("solvers".to_string()),
        bio: Set(None),
        avatar_hash: Set(None),
        locked: Set(false),
        deletion_pending: Set(false),
        invite_token: Set("invite".to_string()),
        captain_id: Set(user_id),
        ..Default::default()
    }
    .insert(&state.db)
    .await
    .expect("insert team");
    let participation = participation::ActiveModel {
        status: Set(ParticipationStatus::Accepted),
        token: Set("participant-token".to_string()),
        writeup_id: Set(None),
        game_id: Set(game.id),
        team_id: Set(team.id),
        division_id: Set(None),
        suspicion_score: Set(0),
        ..Default::default()
    }
    .insert(&state.db)
    .await
    .expect("insert participation");
    let solved = submission::ActiveModel {
        answer: Set("flag{old}".to_string()),
        status: Set(AnswerResult::Accepted),
        submit_time_utc: Set(now),
        user_id: Set(Some(user_id)),
        team_id: Set(team.id),
        participation_id: Set(participation.id),
        game_id: Set(game.id),
        challenge_id: Set(challenge_id),
        ..Default::default()
    }
    .insert(&state.db)
    .await
    .expect("insert accepted submission");
    first_solve::ActiveModel {
        participation_id: Set(participation.id),
        challenge_id: Set(challenge_id),
        submission_id: Set(solved.id),
    }
    .insert(&state.db)
    .await
    .expect("insert first solve");
    sqlx::query(
        r#"UPDATE "GameChallenges"
              SET accepted_count = 1,
                  submission_count = 4,
                  is_enabled = TRUE,
                  submission_limit = 7,
                  disable_blood_bonus = TRUE,
                  original_score = 777,
                  min_score_rate = 0.4,
                  difficulty = 9.0
            WHERE id = $1"#,
    )
    .bind(challenge_id)
    .execute(state.pg())
    .await
    .expect("record challenge progress");
    // A legacy replica path must migrate in place without losing solve history.
    let legacy_replica_path = format!(
        "/different-node/storage/repos/{}/event/web/challenge.yaml",
        binding.id
    );
    sqlx::query(r#"UPDATE "GameChallenges" SET source_yaml_path = $2 WHERE id = $1"#)
        .bind(challenge_id)
        .bind(legacy_replica_path)
        .execute(state.pg())
        .await
        .unwrap();

    tokio::fs::write(&handout, b"replacement handout")
        .await
        .expect("write replacement handout");
    tokio::fs::write(
        &manifest,
        "name: Renamed\ndescription: after sync\ntype: StaticAttachment\ncategory: Misc\nprovide: handout.txt\nsubmissionLimit: 99\nminScoreRate: 0.1\ndifficulty: 2\nflags:\n  - flag{new}\n  - flag{new}\n",
    )
    .await
    .expect("write updated manifest");
    sqlx::query(
        r#"CREATE FUNCTION reject_attachment_delete() RETURNS trigger
             LANGUAGE plpgsql AS $$
             BEGIN
                 RAISE EXCEPTION 'injected attachment cleanup failure';
             END
             $$"#,
    )
    .execute(state.pg())
    .await
    .unwrap();
    sqlx::query(
        r#"CREATE TRIGGER reject_attachment_delete
             BEFORE DELETE ON "Attachments"
             FOR EACH ROW EXECUTE FUNCTION reject_attachment_delete()"#,
    )
    .execute(state.pg())
    .await
    .unwrap();
    let atomic_failure = import_with_game_lock(&state, game.id, &manifest)
        .await
        .expect("attachment failure remains a retryable import result");
    assert!(!atomic_failure.attachment_synced);
    assert_eq!(
        sqlx::query_scalar::<_, Option<i32>>(
            r#"SELECT attachment_id FROM "GameChallenges" WHERE id = $1"#,
        )
        .bind(challenge_id)
        .fetch_one(state.pg())
        .await
        .unwrap(),
        Some(first_attachment_id)
    );
    let unpublished_hash = crate::utils::codec::sha256_hex(b"replacement handout");
    assert_eq!(
        sqlx::query_scalar::<_, i64>(r#"SELECT COUNT(*) FROM "Files" WHERE hash = $1"#)
            .bind(&unpublished_hash)
            .fetch_one(state.pg())
            .await
            .unwrap(),
        0
    );
    sqlx::query(r#"DROP TRIGGER reject_attachment_delete ON "Attachments""#)
        .execute(state.pg())
        .await
        .unwrap();
    sqlx::query(r#"DROP FUNCTION reject_attachment_delete()"#)
        .execute(state.pg())
        .await
        .unwrap();
    let second = import_with_game_lock(&state, game.id, &manifest)
        .await
        .expect("repository update");
    assert_eq!(
        second,
        ManifestImportResult {
            challenge_id,
            created: false,
            build_queued: false,
            runtime_update_deferred: false,
            grading_update_deferred: true,
            attachment_synced: true,
        }
    );

    let row = game_challenge::Entity::find_by_id(challenge_id)
        .one(&state.db)
        .await
        .expect("load updated challenge")
        .expect("challenge retained");
    assert_eq!(row.id, challenge_id);
    assert_eq!(row.title, "Renamed");
    assert!(row.content.contains("after sync"));
    assert_eq!(row.accepted_count, 1);
    assert_eq!(row.submission_count, 4);
    assert!(row.is_enabled);
    assert_eq!(row.submission_limit, 7);
    assert!(row.disable_blood_bonus);
    assert_eq!(row.original_score, 777);
    assert_eq!(row.min_score_rate, 0.4);
    assert_eq!(row.difficulty, 9.0);
    assert_eq!(
        row.source_yaml_path,
        Some(format!("binding/{}/event/web/challenge.yaml", binding.id))
    );
    let (replacement_attachment_id, replacement_attachment_hash) =
        sqlx::query_as::<_, (i32, String)>(
            r#"SELECT attachment.id, file.hash
                 FROM "GameChallenges" challenge
                 JOIN "Attachments" attachment ON attachment.id = challenge.attachment_id
                 JOIN "Files" file ON file.id = attachment.local_file_id
                WHERE challenge.id = $1"#,
        )
        .bind(challenge_id)
        .fetch_one(state.pg())
        .await
        .expect("load replacement attachment");
    assert_ne!(replacement_attachment_id, first_attachment_id);
    assert_ne!(replacement_attachment_hash, first_attachment_hash);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(r#"SELECT COUNT(*) FROM "Attachments" WHERE id = $1"#)
            .bind(first_attachment_id)
            .fetch_one(state.pg())
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            r#"SELECT COUNT(*) FROM "Submissions" WHERE challenge_id = $1"#,
        )
        .bind(challenge_id)
        .fetch_one(state.pg())
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            r#"SELECT COUNT(*) FROM "FirstSolves" WHERE challenge_id = $1"#,
        )
        .bind(challenge_id)
        .fetch_one(state.pg())
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            r#"SELECT flag FROM "FlagContexts" WHERE challenge_id = $1"#,
        )
        .bind(challenge_id)
        .fetch_all(state.pg())
        .await
        .unwrap(),
        vec!["flag{old}".to_string()]
    );

    tokio::fs::write(
        &manifest,
        "name: Renamed\ndescription: transient artifact failure\ntype: StaticAttachment\ncategory: Misc\nprovide: missing.txt\nflags:\n  - flag{new}\n",
    )
    .await
    .unwrap();
    let failed_attachment = import_with_game_lock(&state, game.id, &manifest)
        .await
        .expect("metadata remains retryable when attachment packaging fails");
    assert!(!failed_attachment.attachment_synced);
    assert_eq!(
        sqlx::query_scalar::<_, Option<i32>>(
            r#"SELECT attachment_id FROM "GameChallenges" WHERE id = $1"#,
        )
        .bind(challenge_id)
        .fetch_one(state.pg())
        .await
        .unwrap(),
        Some(replacement_attachment_id)
    );
    // Removing both attachment sources must stop serving stale repository data.
    tokio::fs::write(
        &manifest,
        "name: Renamed\ndescription: attachment removed\ntype: StaticAttachment\ncategory: Misc\nflags:\n  - flag{new}\n",
    )
    .await
    .unwrap();
    import_with_game_lock(&state, game.id, &manifest)
        .await
        .expect("remove repository attachment");
    assert_eq!(
        sqlx::query_scalar::<_, Option<i32>>(
            r#"SELECT attachment_id FROM "GameChallenges" WHERE id = $1"#,
        )
        .bind(challenge_id)
        .fetch_one(state.pg())
        .await
        .unwrap(),
        None
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(r#"SELECT COUNT(*) FROM "Attachments" WHERE id = $1"#)
            .bind(replacement_attachment_id)
            .fetch_one(state.pg())
            .await
            .unwrap(),
        0
    );
    // Reject domain-type mutation at a stable historical identity.
    tokio::fs::write(
        &manifest,
        "name: Renamed\ntype: DynamicAttachment\ncategory: Misc\n",
    )
    .await
    .unwrap();
    assert!(import_with_game_lock(&state, game.id, &manifest)
        .await
        .is_err());
    assert!(game_challenge::Entity::find_by_id(challenge_id)
        .one(&state.db)
        .await
        .unwrap()
        .is_some());

    let race_id = repository_concurrency::assert_submission_evidence_fence(
        &state,
        &root,
        binding.id,
        game.id,
        user_id,
        team.id,
        participation.id,
    )
    .await;
    // Disabled runtime refresh must retain flags already leased to an instance.
    let dynamic_dir = root
        .join("repos")
        .join(binding.id.to_string())
        .join("event/pwn/dynamic");
    tokio::fs::create_dir_all(dynamic_dir.join("src"))
        .await
        .unwrap();
    let dynamic_manifest = dynamic_dir.join("challenge.yaml");
    let dockerfile = dynamic_dir.join("src/Dockerfile");
    tokio::fs::write(&dockerfile, b"FROM scratch\n# revision one\n")
        .await
        .unwrap();
    tokio::fs::write(
        &dynamic_manifest,
        "name: Dynamic runtime\ndescription: revision one\ntype: DynamicContainer\ncategory: Pwn\nflagTemplate: 'rsctf{one_[TEAM_HASH]}'\ncontainer:\n  memoryLimit: 64\n  storageLimit: 128\n  cpuCount: 1\n  exposePort: 8080\n  enableTrafficCapture: false\n",
    )
    .await
    .unwrap();
    let dynamic_first = import_with_game_lock(&state, game.id, &dynamic_manifest)
        .await
        .expect("import dynamic challenge");
    assert!(dynamic_first.created);
    assert!(dynamic_first.build_queued);
    let dynamic_id = dynamic_first.challenge_id;
    let first_archive = game_challenge::Entity::find_by_id(dynamic_id)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap()
        .original_archive_blob_path
        .expect("local build retains source archive");
    let runtime_flag = flag_context::ActiveModel {
        flag: Set("rsctf{leased_runtime_flag}".to_string()),
        is_occupied: Set(true),
        challenge_id: Set(Some(dynamic_id)),
        ..Default::default()
    }
    .insert(&state.db)
    .await
    .expect("insert leased runtime flag");
    let runtime_instance = game_instance::ActiveModel {
        challenge_id: Set(dynamic_id),
        participation_id: Set(participation.id),
        is_loaded: Set(true),
        last_container_operation: Set(now),
        flag_id: Set(Some(runtime_flag.id)),
        container_id: Set(None),
        ..Default::default()
    }
    .insert(&state.db)
    .await
    .expect("insert runtime owner");

    tokio::fs::write(&dockerfile, b"FROM scratch\n# revision two\n")
        .await
        .unwrap();
    tokio::fs::write(
        &dynamic_manifest,
        "name: Dynamic runtime v2\ndescription: revision two\ntype: DynamicContainer\ncategory: Pwn\nflagTemplate: 'rsctf{two_[TEAM_HASH]}'\ncontainer:\n  memoryLimit: 96\n  storageLimit: 256\n  cpuCount: 2\n  exposePort: 9090\n  enableTrafficCapture: true\n",
    )
    .await
    .unwrap();
    let dynamic_second = import_with_game_lock(&state, game.id, &dynamic_manifest)
        .await
        .expect("refresh disabled dynamic challenge");
    assert_eq!(dynamic_second.challenge_id, dynamic_id);
    assert!(!dynamic_second.created);
    assert!(dynamic_second.build_queued);
    assert!(!dynamic_second.runtime_update_deferred);
    let disabled_refresh = game_challenge::Entity::find_by_id(dynamic_id)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(disabled_refresh.memory_limit, Some(96));
    assert_eq!(disabled_refresh.storage_limit, Some(256));
    assert_eq!(disabled_refresh.cpu_count, Some(2));
    assert_eq!(disabled_refresh.expose_port, Some(9090));
    assert_eq!(
        disabled_refresh.flag_template.as_deref(),
        Some("rsctf{two_[TEAM_HASH]}")
    );
    assert!(disabled_refresh.enable_traffic_capture);
    assert!(flag_context::Entity::find_by_id(runtime_flag.id)
        .one(&state.db)
        .await
        .unwrap()
        .is_some());
    assert!(game_instance::Entity::find_by_id(runtime_instance.id)
        .one(&state.db)
        .await
        .unwrap()
        .is_some());
    let second_archive = disabled_refresh
        .original_archive_blob_path
        .clone()
        .expect("updated build archive");
    assert_ne!(second_archive, first_archive);
    let old_archive_refs =
        sqlx::query_scalar::<_, i64>(r#"SELECT reference_count FROM "Files" WHERE hash = $1"#)
            .bind(&first_archive)
            .fetch_optional(state.pg())
            .await
            .unwrap()
            .unwrap_or(0);
    assert_eq!(old_archive_refs, 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(r#"SELECT reference_count FROM "Files" WHERE hash = $1"#)
            .bind(&second_archive)
            .fetch_one(state.pg())
            .await
            .unwrap(),
        1
    );

    let immutable_digest = format!("sha256:{}", "a".repeat(64));
    sqlx::query(
        r#"UPDATE "GameChallenges"
              SET is_enabled = TRUE,
                  build_status = $2,
                  build_image_digest = $3,
                  last_build_log = 'published immutable runtime'
            WHERE id = $1"#,
    )
    .bind(dynamic_id)
    .bind(ChallengeBuildStatus::Success as i16)
    .bind(&immutable_digest)
    .execute(state.pg())
    .await
    .expect("publish dynamic runtime");
    let published = game_challenge::Entity::find_by_id(dynamic_id)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();

    tokio::fs::write(
        &dynamic_manifest,
        "name: Dynamic runtime v2\ndescription: metadata-only live edit\ntype: DynamicContainer\ncategory: Web\nflagTemplate: 'rsctf{two_[TEAM_HASH]}'\ncontainer:\n  memoryLimit: 96\n  storageLimit: 256\n  cpuCount: 2\n  exposePort: 9090\n  enableTrafficCapture: true\n",
    )
    .await
    .unwrap();
    let metadata_only = import_with_game_lock(&state, game.id, &dynamic_manifest)
        .await
        .expect("apply live metadata-only refresh");
    assert!(!metadata_only.runtime_update_deferred);
    assert!(!metadata_only.build_queued);

    tokio::fs::write(&dockerfile, b"FROM scratch\n# revision three\n")
        .await
        .unwrap();
    tokio::fs::write(
        &dynamic_manifest,
        "name: Dynamic metadata v3\ndescription: metadata may change while live\ntype: DynamicContainer\ncategory: Web\nflagTemplate: 'rsctf{unsafe_new_[TEAM_HASH]}'\ncontainer:\n  containerImage: repository/runtime:unsafe-new\n  memoryLimit: 999\n  storageLimit: 999\n  cpuCount: 9\n  exposePort: 9999\n  enableTrafficCapture: true\n",
    )
    .await
    .unwrap();
    let deferred = import_with_game_lock(&state, game.id, &dynamic_manifest)
        .await
        .expect("refresh enabled container metadata");
    assert!(deferred.runtime_update_deferred);
    assert!(!deferred.build_queued);
    let live_refresh = game_challenge::Entity::find_by_id(dynamic_id)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(live_refresh.title, "Dynamic metadata v3");
    assert!(live_refresh.content.contains("metadata may change"));
    assert_eq!(live_refresh.category, ChallengeCategory::Web);
    assert_eq!(live_refresh.container_image, published.container_image);
    assert_eq!(live_refresh.memory_limit, published.memory_limit);
    assert_eq!(live_refresh.storage_limit, published.storage_limit);
    assert_eq!(live_refresh.cpu_count, published.cpu_count);
    assert_eq!(live_refresh.expose_port, published.expose_port);
    assert_eq!(live_refresh.flag_template, published.flag_template);
    assert_eq!(live_refresh.build_status, published.build_status);
    assert_eq!(
        live_refresh.build_image_digest,
        published.build_image_digest
    );
    assert_eq!(live_refresh.last_build_log, published.last_build_log);
    assert_eq!(
        live_refresh.original_archive_blob_path,
        published.original_archive_blob_path
    );
    assert_eq!(
        live_refresh.build_context_subdir,
        published.build_context_subdir
    );
    assert_eq!(
        live_refresh.enable_traffic_capture,
        published.enable_traffic_capture
    );
    assert!(flag_context::Entity::find_by_id(runtime_flag.id)
        .one(&state.db)
        .await
        .unwrap()
        .is_some());
    // Defer a live static flag change until explicit disable.
    let static_dir = root
        .join("repos")
        .join(binding.id.to_string())
        .join("event/pwn/static");
    tokio::fs::create_dir_all(&static_dir).await.unwrap();
    let static_manifest = static_dir.join("challenge.yaml");
    let static_digest = format!("registry.example/team/static@sha256:{}", "b".repeat(64));
    tokio::fs::write(
        &static_manifest,
        format!(
            "name: Static runtime\ntype: StaticContainer\ncategory: Pwn\nflags:\n  - flag{{static_old}}\ncontainer:\n  containerImage: {static_digest}\n  exposePort: 8080\n  enableSharedContainer: true\n"
        ),
    )
    .await
    .unwrap();
    let static_id = import_with_game_lock(&state, game.id, &static_manifest)
        .await
        .unwrap()
        .challenge_id;
    sqlx::query(
        r#"UPDATE "GameChallenges"
              SET is_enabled = TRUE, build_status = $2, build_image_digest = $3
            WHERE id = $1"#,
    )
    .bind(static_id)
    .bind(ChallengeBuildStatus::Success as i16)
    .bind(&static_digest)
    .execute(state.pg())
    .await
    .unwrap();
    tokio::fs::write(
        &static_manifest,
        format!(
            "name: Static runtime metadata\ntype: StaticContainer\ncategory: Web\nflags:\n  - flag{{static_new}}\ncontainer:\n  containerImage: {static_digest}\n  exposePort: 8080\n  enableSharedContainer: true\n"
        ),
    )
    .await
    .unwrap();
    let live_static = import_with_game_lock(&state, game.id, &static_manifest)
        .await
        .unwrap();
    assert!(live_static.runtime_update_deferred);
    assert!(!live_static.grading_update_deferred);
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            r#"SELECT flag FROM "FlagContexts" WHERE challenge_id = $1"#,
        )
        .bind(static_id)
        .fetch_all(state.pg())
        .await
        .unwrap(),
        vec!["flag{static_old}".to_string()]
    );
    sqlx::query(r#"UPDATE "GameChallenges" SET is_enabled = FALSE WHERE id = $1"#)
        .bind(static_id)
        .execute(state.pg())
        .await
        .unwrap();
    let disabled_static = import_with_game_lock(&state, game.id, &static_manifest)
        .await
        .unwrap();
    assert!(!disabled_static.runtime_update_deferred);
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            r#"SELECT flag FROM "FlagContexts" WHERE challenge_id = $1"#,
        )
        .bind(static_id)
        .fetch_all(state.pg())
        .await
        .unwrap(),
        vec!["flag{static_new}".to_string()]
    );

    repository_concurrency::assert_cleanup_reenable_transition(&state, static_id).await;
    repository_concurrency::assert_pending_deletion_rejects_repository_mutation(
        &state,
        game.id,
        static_id,
        &static_manifest,
        &[challenge_id, race_id, dynamic_id],
    )
    .await;
    // A missing declared checker source cannot become applied while live.
    let ad_dir = root
        .join("repos")
        .join(binding.id.to_string())
        .join("event/pwn/ad");
    tokio::fs::create_dir_all(&ad_dir).await.unwrap();
    let ad_manifest = ad_dir.join("challenge.yaml");
    tokio::fs::write(
        &ad_manifest,
        format!(
            "name: AD service\ntype: AttackDefense\ncategory: Pwn\ncontainer:\n  containerImage: {static_digest}\n  exposePort: 9000\n"
        ),
    )
    .await
    .unwrap();
    let ad_id = import_with_game_lock(&state, game.id, &ad_manifest)
        .await
        .unwrap()
        .challenge_id;
    sqlx::query(r#"UPDATE "GameChallenges" SET is_enabled = TRUE WHERE id = $1"#)
        .bind(ad_id)
        .execute(state.pg())
        .await
        .unwrap();
    tokio::fs::write(
        &ad_manifest,
        format!(
            "name: AD service\ntype: AttackDefense\ncategory: Pwn\ncontainer:\n  containerImage: {static_digest}\n  exposePort: 9000\nad:\n  checkerImage: '{{{{.slug}}}}'\n"
        ),
    )
    .await
    .unwrap();
    assert!(import_with_game_lock(&state, game.id, &ad_manifest)
        .await
        .is_err());
    let game_lock = crate::services::ad_engine::acquire_ad_game_lock(&state.db, game.id)
        .await
        .unwrap();
    let protected_ad = tombstone_missing_challenges(
        &state,
        game.id,
        &[challenge_id, race_id, dynamic_id, static_id],
    )
    .await;
    game_lock.release().await.unwrap();
    assert!(matches!(protected_ad, Err(AppError::Conflict(_))));
    assert!(
        game_challenge::Entity::find_by_id(ad_id)
            .one(&state.db)
            .await
            .unwrap()
            .unwrap()
            .is_enabled
    );

    repository_concurrency::assert_definition_contention_is_nonblocking(
        &state,
        game.id,
        dynamic_id,
        &dynamic_manifest,
    )
    .await;
    // Tombstoning retains solve and first-solve history.
    let retired_dir = root
        .join("repos")
        .join(binding.id.to_string())
        .join("event/crypto/retired");
    tokio::fs::create_dir_all(&retired_dir).await.unwrap();
    let retired_manifest = retired_dir.join("challenge.yaml");
    tokio::fs::write(
        &retired_manifest,
        "name: Retired challenge\ntype: StaticAttachment\ncategory: Crypto\nflags:\n  - flag{retired}\n",
    )
    .await
    .unwrap();
    let retired = import_with_game_lock(&state, game.id, &retired_manifest)
        .await
        .unwrap();
    sqlx::query(r#"UPDATE "GameChallenges" SET is_enabled = TRUE WHERE id = $1"#)
        .bind(retired.challenge_id)
        .execute(state.pg())
        .await
        .unwrap();
    let retired_submission = submission::ActiveModel {
        answer: Set("flag{retired}".to_string()),
        status: Set(AnswerResult::Accepted),
        submit_time_utc: Set(now),
        user_id: Set(Some(user_id)),
        team_id: Set(team.id),
        participation_id: Set(participation.id),
        game_id: Set(game.id),
        challenge_id: Set(retired.challenge_id),
        ..Default::default()
    }
    .insert(&state.db)
    .await
    .unwrap();
    first_solve::ActiveModel {
        participation_id: Set(participation.id),
        challenge_id: Set(retired.challenge_id),
        submission_id: Set(retired_submission.id),
    }
    .insert(&state.db)
    .await
    .unwrap();
    sqlx::query(
        r#"UPDATE "Games"
              SET start_time_utc = $2, end_time_utc = $3
            WHERE id = $1"#,
    )
    .bind(game.id)
    .bind(now - chrono::Duration::hours(2))
    .bind(now - chrono::Duration::hours(1))
    .execute(state.pg())
    .await
    .unwrap();
    repository_concurrency::assert_ad_closeout_tombstone(
        &state,
        game.id,
        ad_id,
        &[
            challenge_id,
            race_id,
            dynamic_id,
            static_id,
            retired.challenge_id,
        ],
    )
    .await;
    let game_lock = crate::services::ad_engine::acquire_ad_game_lock(&state.db, game.id)
        .await
        .unwrap();
    let protected_removal = tombstone_missing_challenges(
        &state,
        game.id,
        &[challenge_id, race_id, dynamic_id, static_id, ad_id],
    )
    .await;
    game_lock.release().await.unwrap();
    assert!(matches!(protected_removal, Err(AppError::Conflict(_))));
    assert!(
        game_challenge::Entity::find_by_id(retired.challenge_id)
            .one(&state.db)
            .await
            .unwrap()
            .unwrap()
            .is_enabled
    );
    sqlx::query(r#"UPDATE "GameChallenges" SET is_enabled = FALSE WHERE id = $1"#)
        .bind(retired.challenge_id)
        .execute(state.pg())
        .await
        .unwrap();
    let game_lock = crate::services::ad_engine::acquire_ad_game_lock(&state.db, game.id)
        .await
        .unwrap();
    let tombstoned = tombstone_missing_challenges(
        &state,
        game.id,
        &[challenge_id, race_id, dynamic_id, static_id, ad_id],
    )
    .await
    .unwrap();
    game_lock.release().await.unwrap();
    assert_eq!(tombstoned, vec![retired.challenge_id]);
    assert!(
        !game_challenge::Entity::find_by_id(retired.challenge_id)
            .one(&state.db)
            .await
            .unwrap()
            .unwrap()
            .is_enabled
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            r#"SELECT COUNT(*) FROM "Submissions" WHERE challenge_id = $1"#,
        )
        .bind(retired.challenge_id)
        .fetch_one(state.pg())
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            r#"SELECT COUNT(*) FROM "FirstSolves" WHERE challenge_id = $1"#,
        )
        .bind(retired.challenge_id)
        .fetch_one(state.pg())
        .await
        .unwrap(),
        1
    );
    let game_lock = crate::services::ad_engine::acquire_ad_game_lock(&state.db, game.id)
        .await
        .unwrap();
    let retry = tombstone_missing_challenges(
        &state,
        game.id,
        &[challenge_id, race_id, dynamic_id, static_id, ad_id],
    )
    .await
    .unwrap();
    assert_eq!(retry, vec![retired.challenge_id]);
    game_lock.release().await.unwrap();

    repository_concurrency::assert_queued_pushback_remote_head(
        &state, &root, binding.id, game.id, static_id,
    )
    .await;
    repository_concurrency::assert_binding_update_and_delete_fences(
        &state,
        &root,
        binding.id,
        game.id,
        challenge_id,
    )
    .await;

    drop(state);
    pool.close().await;
    let _ = tokio::fs::remove_dir_all(&root).await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin_pool)
        .await
        .expect("drop isolated schema");
}
