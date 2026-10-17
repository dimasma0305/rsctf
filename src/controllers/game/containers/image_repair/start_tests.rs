//! Exercise the first player start through real image repair and publication.
use std::io::{Cursor, Write};
use std::sync::Arc;

use sea_orm::SqlxPostgresConnector;
use sqlx::postgres::PgPoolOptions;

use super::*;
use crate::app_state::AppState;
use crate::migrations::{Migrator, MigratorTrait};
use crate::models::internal::configs::AppConfig;
use crate::services::cache::InMemoryCache;
use crate::services::container::DockerContainerManager;
use crate::services::token::TokenService;
use crate::storage::{BlobStorage, LocalBlobStorage};
use crate::utils::enums::Role;

#[tokio::test]
async fn first_start_publishes_repaired_image_without_a_retry() {
    let Ok(base_image) = std::env::var("RSCTF_REAL_DOCKER_RETRY_IMAGE") else {
        return;
    };
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("real image-repair test requires disposable PostgreSQL");
    let docker = bollard::Docker::connect_with_local_defaults().unwrap();
    docker.inspect_image(&base_image).await.unwrap();
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(crate::migrations::test_pg_connect_options(&database_url))
        .await
        .unwrap();
    let schema = format!("image_start_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(12)
        .connect_with(
            crate::migrations::test_pg_connect_options(&database_url)
                .options([("search_path", schema.as_str())]),
        )
        .await
        .unwrap();
    let db = SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone());
    Migrator::up(&db, None).await.unwrap();
    let root = std::env::temp_dir().join(&schema);
    let storage = Arc::new(LocalBlobStorage::new(root.join("blobs")));
    let mut config = AppConfig::default();
    config.storage_root = root.to_string_lossy().into_owned();
    config.jwt_secret = "image-repair-test-secret-0123456789abcdef".into();
    let st = AppState::new(
        db,
        Arc::new(config),
        Arc::new(InMemoryCache::new()),
        storage.clone(),
        TokenService::new("image-repair-test-secret-0123456789abcdef", 60),
        Arc::new(DockerContainerManager::connect().unwrap()),
    );
    let actor = CurrentUser {
        id: Uuid::new_v4(),
        role: Role::User,
        name: schema.clone(),
        security_stamp: "stamp".into(),
    };
    sqlx::query(
        r#"INSERT INTO "AspNetUsers"
           (id, user_name, normalized_user_name, email_confirmed, security_stamp,
            phone_number_confirmed, two_factor_enabled, lockout_enabled, access_failed_count,
            role, ip, last_signed_in_utc, last_visited_utc, register_time_utc,
            bio, real_name, std_number, exercise_visible)
           VALUES ($1, $2, upper($2), TRUE, 'stamp', FALSE, FALSE, FALSE, 0, 1,
                   '', now(), now(), now(), '', '', '', TRUE)"#,
    )
    .bind(actor.id)
    .bind(&schema)
    .execute(&pool)
    .await
    .unwrap();
    let (public_key, private_key) = crate::utils::crypto_utils::generate_game_keypair();
    // Game contexts are process-cached by id; avoid other test schemas' ids.
    let game_id = 1_000_000 + (rand::random::<u32>() % 1_000_000_000) as i32;
    sqlx::query(
        r#"INSERT INTO "Games"
           (id, title, public_key, private_key, hidden, practice_mode, summary, content,
            accept_without_review, allow_user_submissions, writeup_required,
            team_member_count_limit, container_count_limit, start_time_utc, end_time_utc,
            writeup_deadline, writeup_note, blood_bonus_value, ad_allow_snapshot_download,
            ad_scoring_paused)
           VALUES ($1, $2, $3, $4, TRUE, FALSE, '', '', TRUE, FALSE, FALSE, 0, 0,
                   now() - interval '1 hour', now() + interval '1 hour',
                   now() + interval '2 hours', '', 0, TRUE, FALSE)"#,
    )
    .bind(game_id)
    .bind(&schema)
    .bind(public_key)
    .bind(private_key)
    .execute(&pool)
    .await
    .unwrap();
    let team_id: i32 = sqlx::query_scalar(
        r#"INSERT INTO "Teams" (name, locked, invite_token, captain_id)
           VALUES ($1, FALSE, $2, $3) RETURNING id"#,
    )
    .bind(&schema)
    .bind(Uuid::new_v4().to_string())
    .bind(actor.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES ($1, $2)"#)
        .bind(team_id)
        .bind(actor.id)
        .execute(&pool)
        .await
        .unwrap();
    let participation_id: i32 = sqlx::query_scalar(
        r#"INSERT INTO "Participations" (status, token, game_id, team_id, suspicion_score)
           VALUES (1, 'test', $1, $2, 0) RETURNING id"#,
    )
    .bind(game_id)
    .bind(team_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "UserParticipations" (user_id, game_id, team_id, participation_id)
           VALUES ($1, $2, $3, $4)"#,
    )
    .bind(actor.id)
    .bind(game_id)
    .bind(team_id)
    .bind(participation_id)
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        r#"INSERT INTO "IdentityObservations"
           (user_id, game_id, team_id, participation_id, kind, value_hash,
            value_hint, source, observed_at_utc)
           VALUES ($1, $2, $3, $4, 'Fingerprint', $5, 'image-repair-test',
                   'GameJoin', clock_timestamp())"#,
    )
    .bind(actor.id)
    .bind(game_id)
    .bind(team_id)
    .bind(participation_id)
    .bind(vec![42_u8; 32])
    .execute(&pool)
    .await
    .unwrap();

    let mut failures = Vec::new();
    for shared in [false, true] {
        let image_tag = format!("rsctf-test/{schema}-{shared}:latest");
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        archive
            .start_file("Dockerfile", zip::write::SimpleFileOptions::default())
            .unwrap();
        writeln!(
            archive,
            "FROM {base_image}\nLABEL test.repair={schema}-{shared}"
        )
        .unwrap();
        let bytes = archive.finish().unwrap().into_inner();
        let blob = storage.store("source.zip", &bytes).await.unwrap();
        let missing_image = format!("sha256:{}", "a".repeat(64));
        assert!(docker.inspect_image(&missing_image).await.is_err());
        let cid: i32 = sqlx::query_scalar(
            r#"INSERT INTO "GameChallenges"
               (game_id, title, content, category, "Type", is_enabled, submission_limit,
                accepted_count, submission_count, review_status, build_status,
                enable_traffic_capture, enable_shared_container, original_score, min_score_rate,
                difficulty, score_curve, ad_allow_egress, ad_allow_self_reset,
                ad_ssh_requires_flag, ad_self_hosted, container_image, build_image_digest,
                original_archive_blob_path, build_context_subdir, expose_port,
                memory_limit, cpu_count, storage_limit)
               VALUES ($1, $2, '', 0, 1, TRUE, 0, 0, 0, 0, 1, FALSE, $3, 500, 0.25,
                       5, 0, FALSE, FALSE, FALSE, FALSE, $4, $5, $6, '.', 8080, 64, 1, 32)
               RETURNING id"#,
        )
        .bind(game_id)
        .bind(format!("repair-{shared}"))
        .bind(shared)
        .bind(&image_tag)
        .bind(&missing_image)
        .bind(&blob.hash)
        .fetch_one(&pool)
        .await
        .unwrap();
        let operation_id = Uuid::new_v4();
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-rsctf-operation-id",
            operation_id.to_string().parse().unwrap(),
        );
        let result = create_container(
            State(st.clone()),
            actor.clone(),
            headers,
            Path((game_id, cid)),
        )
        .await;
        let operation: Option<(String, Option<String>)> = sqlx::query_as(
            r#"SELECT state, definition_fence FROM "PlayerContainerOperations"
                WHERE operation_id = $1"#,
        )
        .bind(operation_id)
        .fetch_optional(&pool)
        .await
        .unwrap();
        let challenge = load_playable_challenge(&st, game_id, cid).await.unwrap();
        let runtime =
            crate::services::challenge_workloads::resolve_runtime(&st, &challenge).unwrap();
        if let Err(error) = result {
            failures.push(format!("shared={shared}: first start failed: {error}"));
        } else if operation != Some(("Succeeded".into(), Some(runtime.publication_fence))) {
            failures.push(format!(
                "shared={shared}: first start recorded a stale definition"
            ));
        }
        let backends: Vec<String> = sqlx::query_scalar(r#"SELECT container_id FROM "Containers""#)
            .fetch_all(&pool)
            .await
            .unwrap();
        for backend in backends {
            st.containers.destroy(&backend).await.unwrap();
        }
        sqlx::query(r#"DELETE FROM "Containers""#)
            .execute(&pool)
            .await
            .unwrap();
        if docker.inspect_image(&image_tag).await.is_ok() {
            docker.remove_image(&image_tag, None, None).await.unwrap();
        }
    }
    drop(st);
    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    tokio::fs::remove_dir_all(&root).await.unwrap();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
