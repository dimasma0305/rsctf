use super::*;
use sea_orm_migration::MigratorTrait;
use std::sync::Arc;

#[test]
fn simple_challenge_input_is_bounded_and_flag_debug_is_redacted() {
    let valid = serde_json::json!({"title":"First", "content":"Find it", "initialFlag":"flag{hello}", "originalScore":100});
    let model: ChallengeInfoModel = serde_json::from_value(valid.clone()).unwrap();
    mutation_sql::validate_create(&model).unwrap();
    assert!(!format!("{model:?}").contains("flag{hello}"));
    for (key, value) in [
        ("title", serde_json::json!(" ")),
        ("initialFlag", serde_json::json!("flag{hello} ")),
        ("initialFlag", serde_json::json!("a".repeat(128))),
        ("content", serde_json::json!("a".repeat(65_537))),
        ("originalScore", serde_json::json!(0)),
        ("type", serde_json::json!("DynamicContainer")),
    ] {
        let mut invalid = valid.clone();
        invalid[key] = value;
        assert!(mutation_sql::validate_create(&serde_json::from_value(invalid).unwrap()).is_err());
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn simple_challenge_create_is_atomic_authorized_and_retryable() {
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    let db = crate::utils::test_database::TestDatabase::new("simple_create").await;
    let database = sea_orm::SqlxPostgresConnector::from_sqlx_postgres_pool(db.pool.clone());
    crate::migrations::Migrator::up(&database, None)
        .await
        .unwrap();
    let root = std::env::temp_dir().join(format!("rsctf-simple-{}", Uuid::new_v4()));
    let st = crate::app_state::AppState::new(
        database,
        Arc::new(crate::models::internal::configs::AppConfig::default()),
        Arc::new(crate::services::cache::InMemoryCache::new()),
        Arc::new(crate::storage::LocalBlobStorage::new(root.clone())),
        crate::services::token::TokenService::new("0123456789abcdef0123456789abcdef", 60),
        Arc::new(crate::services::container::NoopContainerManager),
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
        VALUES ('First game', '', '', TRUE, TRUE, '', '', TRUE, FALSE, FALSE, 0, 3,
            now() + interval '1 hour', now() + interval '2 hours', now(), '', 0, TRUE, FALSE)
        RETURNING id"#).fetch_one(&db.pool).await.unwrap();
    let router = crate::controllers::edit::router().with_state(st.clone());
    let payload = serde_json::json!({"title":"First", "content":"Find it", "initialFlag":"flag{hello}", "originalScore":100, "operationId":Uuid::new_v4()});
    let request = |principal: Option<CurrentUser>, payload: serde_json::Value| {
        let router = router.clone();
        async move {
            let mut request = Request::builder()
                .method("POST")
                .uri(format!("/api/edit/games/{game_id}/challenges"))
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap();
            if let Some(principal) = principal {
                request.extensions_mut().insert(principal);
            }
            router.oneshot(request).await.unwrap()
        }
    };
    assert_eq!(
        request(None, payload.clone()).await.status(),
        StatusCode::UNAUTHORIZED
    );
    for role in [Role::User, Role::Monitor, Role::Banned] {
        let mut outsider = actor.clone();
        outsider.id = Uuid::new_v4();
        outsider.role = role;
        assert_eq!(
            request(Some(outsider), payload.clone()).await.status(),
            StatusCode::FORBIDDEN
        );
    }
    // A database error after challenge insertion must roll back both the row and operation receipt.
    sqlx::raw_sql(r#"CREATE FUNCTION fail_first_flag() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN RAISE EXCEPTION 'injected flag failure'; END; $$;
        CREATE TRIGGER reject_flag BEFORE INSERT ON "FlagContexts" FOR EACH ROW EXECUTE FUNCTION fail_first_flag();"#)
        .execute(&db.pool).await.unwrap();
    assert_eq!(
        request(Some(actor.clone()), payload.clone()).await.status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(r#"SELECT count(*) FROM "GameChallenges" WHERE game_id=$1"#)
            .bind(game_id)
            .fetch_one(&db.pool)
            .await
            .unwrap(),
        0
    );
    sqlx::raw_sql(
        r#"DROP TRIGGER reject_flag ON "FlagContexts"; DROP FUNCTION fail_first_flag();"#,
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let (mut first, mut retry) = tokio::join!(
        request(Some(actor.clone()), payload.clone()),
        request(Some(actor.clone()), payload.clone())
    );
    // The shared game-control lock intentionally rejects concurrent writers
    // with 503; the same operation identity must then safely replay.
    for response in [&mut first, &mut retry] {
        for _ in 0..20 {
            if response.status() != StatusCode::SERVICE_UNAVAILABLE {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            *response = request(Some(actor.clone()), payload.clone()).await;
        }
    }
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(retry.status(), StatusCode::OK);
    let first: serde_json::Value =
        serde_json::from_slice(&to_bytes(first.into_body(), 1_048_576).await.unwrap()).unwrap();
    let retry: serde_json::Value =
        serde_json::from_slice(&to_bytes(retry.into_body(), 1_048_576).await.unwrap()).unwrap();
    assert_eq!(first["id"], retry["id"]);
    assert_eq!(first["isEnabled"], false);
    assert_eq!(first["content"], "Find it");
    assert_eq!(first["originalScore"], 100);
    assert_eq!(first["flags"][0]["flag"], "flag{hello}");
    let mut changed = payload.clone();
    changed["initialFlag"] = serde_json::json!("flag{different}");
    assert_eq!(
        request(Some(actor), changed).await.status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(r#"SELECT count(*) FROM "FlagContexts""#)
            .fetch_one(&db.pool)
            .await
            .unwrap(),
        1
    );
    drop(router);
    drop(st);
    db.close().await;
    if root.exists() {
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
