//! Real database/controller proof for placement edits and recoverable replacement.
use super::*;
use crate::services::container::{
    profiles::{config, ProfileRouter},
    ContainerBackendKind, ContainerInfo, ContainerManager, ContainerResourceLimits, ContainerSpec,
};
use sea_orm_migration::MigratorTrait;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

struct Runtime {
    backend: ContainerBackendKind,
    fail_destroy: AtomicBool,
    destroyed: Mutex<Vec<String>>,
}

impl Runtime {
    fn new(backend: ContainerBackendKind) -> Self {
        Self {
            backend,
            fail_destroy: AtomicBool::new(false),
            destroyed: Mutex::new(vec![]),
        }
    }
}

#[async_trait::async_trait]
impl ContainerManager for Runtime {
    fn backend_kind(&self) -> ContainerBackendKind {
        self.backend
    }
    async fn create(&self, _: ContainerSpec) -> AppResult<ContainerInfo> {
        Ok(ContainerInfo {
            id: "original-runtime".into(),
            ip: "127.0.0.1".into(),
            port: 80,
            status: "running".into(),
        })
    }
    async fn destroy(&self, id: &str) -> AppResult<()> {
        if self.fail_destroy.swap(false, Ordering::SeqCst) {
            return Err(AppError::unavailable("injected interrupted teardown"));
        }
        self.destroyed.lock().unwrap().push(id.into());
        Ok(())
    }
    async fn query(&self, _: &str) -> AppResult<crate::services::container::ContainerStatus> {
        unreachable!("replacement must destroy through the recorded owner")
    }
}

fn edit(revision: i64, profile: Option<&str>, replace: bool) -> ChallengeUpdateModel {
    serde_json::from_value(serde_json::json!({
        "operationId": Uuid::new_v4(), "expectedRevision": revision,
        "runtimeProfile": profile, "replaceRuntimeInstances": replace,
    }))
    .unwrap()
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn profile_edits_preserve_instances_and_replacement_resumes_after_teardown_failure() {
    let db = crate::utils::test_database::TestDatabase::new("profile_edits").await;
    let database = sea_orm::SqlxPostgresConnector::from_sqlx_postgres_pool(db.pool.clone());
    crate::migrations::Migrator::up(&database, None)
        .await
        .unwrap();
    let docker = Arc::new(Runtime::new(ContainerBackendKind::Docker));
    let vm = Arc::new(Runtime::new(ContainerBackendKind::Kubernetes));
    let profiles = config::parse(r#"[
        {"id":"docker","name":"Docker","backend":"docker","isolation":"container"},
        {"id":"vm","name":"MicroVM","backend":"kubernetes","isolation":"microvm","runtimeClass":"kata"}
    ]"#).unwrap();
    let router = Arc::new(
        ProfileRouter::new(
            docker.clone(),
            vec![
                (profiles[0].clone(), docker.clone()),
                (profiles[1].clone(), vm.clone()),
            ],
        )
        .unwrap(),
    );
    let root = std::env::temp_dir().join(format!("rsctf-profile-edit-{}", Uuid::new_v4()));
    let mut config = crate::models::internal::configs::AppConfig::default();
    config.storage_root = root.to_string_lossy().into_owned();
    let st = crate::app_state::AppState::new(
        database,
        Arc::new(config),
        Arc::new(crate::services::cache::InMemoryCache::new()),
        Arc::new(crate::storage::LocalBlobStorage::new(root.clone())),
        crate::services::token::TokenService::new("0123456789abcdef0123456789abcdef", 60),
        router.clone(),
    );
    let actor = CurrentUser {
        id: Uuid::new_v4(),
        role: Role::Admin,
        name: "fixture".into(),
        security_stamp: "fixture".into(),
    };
    sqlx::query(
        r#"INSERT INTO "AspNetUsers"
        (id, user_name, normalized_user_name, email_confirmed, phone_number_confirmed,
         two_factor_enabled, lockout_enabled, access_failed_count, role, ip,
         last_signed_in_utc, last_visited_utc, register_time_utc, bio, real_name,
         std_number, exercise_visible)
        VALUES ($1, 'fixture', 'FIXTURE', TRUE, FALSE, FALSE, FALSE, 0, 1, '',
            now(), now(), now(), '', '', '', TRUE)"#,
    )
    .bind(actor.id)
    .execute(&db.pool)
    .await
    .unwrap();
    let game_id: i32 = sqlx::query_scalar(r#"INSERT INTO "Games"
        (title, public_key, private_key, hidden, practice_mode, summary, content,
         accept_without_review, allow_user_submissions, writeup_required,
         team_member_count_limit, container_count_limit, start_time_utc, end_time_utc,
         writeup_deadline, writeup_note, blood_bonus_value, ad_allow_snapshot_download, ad_scoring_paused)
        VALUES ('Profiles', '', '', TRUE, TRUE, '', '', TRUE, FALSE, FALSE, 0, 3,
            now() + interval '1 hour', now() + interval '2 hours', now(), '', 0, TRUE, FALSE)
        RETURNING id"#).fetch_one(&db.pool).await.unwrap();
    let challenge = mutation_sql::insert_challenge_locked(
        &mut db.pool.acquire().await.unwrap(),
        game_id,
        &serde_json::from_value(serde_json::json!({"title":"Profiles", "type":"DynamicContainer"}))
            .unwrap(),
    )
    .await
    .unwrap();
    let id = challenge.id;
    let portable = format!("registry.example/challenge@sha256:{}", "a".repeat(64));
    let local = format!("sha256:{}", "b".repeat(64));
    let mut spec = ContainerSpec::ad_service(
        portable.clone(),
        ContainerResourceLimits {
            memory_limit: 128,
            cpu_count: 1,
            storage_limit: 512,
        },
        80,
        game_id,
        false,
        "fixture".into(),
    );
    spec.runtime_profile = Some("docker".into());
    let original = router.create(spec).await.unwrap();
    let container_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "Containers"
        (id, image, container_id, status, started_at, expect_stop_at, is_proxy, ip, port)
        VALUES ($1, $2, $3, 1, now(), now() + interval '1 hour', FALSE, '127.0.0.1', 80)"#,
    )
    .bind(container_id)
    .bind(&portable)
    .bind(&original.id)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(r#"UPDATE "GameChallenges" SET runtime_profile = 'docker', is_enabled = TRUE,
        container_image = $2, build_image_digest = $2, build_status = $3,
        test_container_id = $4, memory_limit = 128, cpu_count = 1, storage_limit = 512,
        expose_port = 80, flag_template = 'flag{[GUID]}', accepted_count = 2, submission_count = 3 WHERE id = $1"#)
        .bind(id).bind(&portable).bind(ChallengeBuildStatus::Success as i16).bind(container_id).execute(&db.pool).await.unwrap();
    let apply = |model| {
        update_challenge(
            State(st.clone()),
            actor.clone(),
            Path((game_id, id)),
            Json(model),
        )
    };
    // HTTP and application authorization both precede any revision or runtime work.
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    for principal in [
        None,
        Some(CurrentUser {
            role: Role::User,
            ..actor.clone()
        }),
    ] {
        let expected = if principal.is_some() {
            StatusCode::FORBIDDEN
        } else {
            StatusCode::UNAUTHORIZED
        };
        let mut request = Request::builder()
            .method("PUT")
            .uri(format!("/api/edit/games/{game_id}/challenges/{id}"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&edit(1, Some("vm"), true)).unwrap(),
            ))
            .unwrap();
        if let Some(user) = principal {
            request.extensions_mut().insert(user);
        }
        let response = super::super::router()
            .with_state(st.clone())
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
    assert!(apply(edit(1, Some("missing"), true)).await.is_err());
    sqlx::query(r#"UPDATE "GameChallenges" SET build_image_digest = $2 WHERE id = $1"#)
        .bind(id)
        .bind(local)
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(
        apply(edit(1, Some("vm"), true)).await.is_err(),
        "a daemon-local image must fail before teardown"
    );
    assert!(docker.destroyed.lock().unwrap().is_empty());
    sqlx::query(r#"UPDATE "GameChallenges" SET build_image_digest = $2 WHERE id = $1"#)
        .bind(id)
        .bind(&portable)
        .execute(&db.pool)
        .await
        .unwrap();
    let saved = apply(edit(1, Some("vm"), false)).await.unwrap().data;
    assert_eq!(saved.runtime_profile.as_deref(), Some("vm"));
    assert!(
        docker.destroyed.lock().unwrap().is_empty(),
        "save retains the original Docker instance"
    );
    assert_eq!(
        apply(edit(1, Some("vm"), true)).await.unwrap_err().status(),
        StatusCode::CONFLICT
    );
    let replacement = edit(saved.revision, Some("vm"), true);
    docker.fail_destroy.store(true, Ordering::SeqCst);
    assert!(apply(replacement.clone()).await.is_err());
    let fenced: bool =
        sqlx::query_scalar(r#"SELECT is_enabled FROM "GameChallenges" WHERE id = $1"#)
            .bind(id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(!fenced, "failed replacement remains durably fenced");
    let recovery = get_challenge(State(st.clone()), actor.clone(), Path((game_id, id)))
        .await
        .unwrap()
        .data;
    assert!(
        recovery.runtime_transition_pending,
        "a reloaded editor must offer recovery"
    );
    let replaced = apply(replacement.clone()).await.unwrap().data;
    assert!(replaced.is_enabled);
    assert_eq!(*docker.destroyed.lock().unwrap(), vec!["original-runtime"]);
    assert!(vm.destroyed.lock().unwrap().is_empty());
    apply(replacement).await.unwrap(); // completed retry cannot destroy twice
    assert_eq!(docker.destroyed.lock().unwrap().len(), 1);
    let history: (i32, i32, Option<Uuid>) = sqlx::query_as(r#"SELECT accepted_count, submission_count, test_container_id FROM "GameChallenges" WHERE id = $1"#).bind(id).fetch_one(&db.pool).await.unwrap();
    assert_eq!(history, (2, 3, None));
    let reset = apply(edit(replaced.revision, None, false))
        .await
        .unwrap()
        .data;
    assert_eq!(
        reset.runtime_profile, None,
        "explicit null selects deployment default"
    );
    for (kind, column) in [
        (ChallengeType::AttackDefense, "ad_scoring_start_round"),
        (ChallengeType::KingOfTheHill, "koth_scoring_start_round"),
    ] {
        sqlx::query(r#"UPDATE "GameChallenges" SET "Type" = $2 WHERE id = $1"#)
            .bind(id)
            .bind(kind as i16)
            .execute(&db.pool)
            .await
            .unwrap();
        sqlx::query(&format!(r#"UPDATE "Games" SET {column} = 1 WHERE id = $1"#))
            .bind(game_id)
            .execute(&db.pool)
            .await
            .unwrap();
        assert_eq!(
            apply(edit(reset.revision, Some("docker"), false))
                .await
                .unwrap_err()
                .status(),
            StatusCode::BAD_REQUEST
        );
        sqlx::query(&format!(
            r#"UPDATE "Games" SET {column} = NULL WHERE id = $1"#
        ))
        .bind(game_id)
        .execute(&db.pool)
        .await
        .unwrap();
    }
    drop(st);
    db.close().await;
    if root.exists() {
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
