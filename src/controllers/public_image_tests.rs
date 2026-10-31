//! Real HTTP, PostgreSQL and filesystem regressions for public image publication.
use std::{io::Cursor, net::SocketAddr, sync::Arc};

use axum::{
    body::{to_bytes, Body},
    extract::ConnectInfo,
    http::{header, Request, StatusCode},
    response::Response,
    routing::{get, post, put},
    Router,
};
use sea_orm::SqlxPostgresConnector;
use tower::ServiceExt;
use uuid::Uuid;

use crate::{
    app_state::AppState,
    migrations::{Migrator, MigratorTrait},
    models::internal::configs::AppConfig,
    services::{cache::InMemoryCache, container::NoopContainerManager, token::TokenService},
    storage::LocalBlobStorage,
    utils::{enums::Role, test_database::TestDatabase},
};

fn png(color: u8) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(2, 2, image::Rgb([color, 0, 0])))
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}

fn upload(path: &str, method: &str, token: &str, operation: Uuid, bytes: &[u8]) -> Request<Body> {
    let mut body = format!("--image-test\r\nContent-Disposition: form-data; name=\"operationId\"\r\n\r\n{operation}\r\n--image-test\r\nContent-Disposition: form-data; name=\"profileRevision\"\r\n\r\n0\r\n--image-test\r\nContent-Disposition: form-data; name=\"file\"; filename=\"pretend.png\"\r\nContent-Type: image/png\r\n\r\n").into_bytes();
    body.extend_from_slice(bytes);
    body.extend_from_slice(b"\r\n--image-test--\r\n");
    Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header("x-rsctf-operation-id", operation.to_string())
        .header(
            header::CONTENT_TYPE,
            "multipart/form-data; boundary=image-test",
        )
        .body(Body::from(body))
        .unwrap()
}

async fn get_asset(router: &Router, hash: &str, range: bool) -> Response {
    let mut request = Request::builder()
        .uri(format!("/assets/{hash}/avatar"))
        .extension(ConnectInfo("192.0.2.1:1234".parse::<SocketAddr>().unwrap()));
    if range {
        request = request.header(header::RANGE, "bytes=8-15");
    }
    router
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn json_ok(response: Response) -> serde_json::Value {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1_048_576).await.unwrap();
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn public_image_http_validation_types_and_branding_publication() {
    let fixture = TestDatabase::new("public_images").await;
    let db = SqlxPostgresConnector::from_sqlx_postgres_pool(fixture.pool.clone());
    Migrator::up(&db, None).await.unwrap();
    let root = std::env::temp_dir().join(format!("rsctf-public-images-{}", Uuid::new_v4()));
    let st = AppState::new(
        db,
        Arc::new(AppConfig::default()),
        Arc::new(InMemoryCache::new()),
        Arc::new(LocalBlobStorage::new(root.clone())),
        TokenService::new("0123456789abcdef0123456789abcdef", 3600),
        Arc::new(NoopContainerManager),
    );
    let user_id = Uuid::new_v4();
    let mut tx = fixture.pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('rsctf.identity_neutral_insert', '1', true)")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "AspNetUsers"
        (id, user_name, normalized_user_name, email_confirmed, security_stamp,
         phone_number_confirmed, two_factor_enabled, lockout_enabled, access_failed_count,
         role, ip, last_signed_in_utc, last_visited_utc, register_time_utc, bio, real_name,
         std_number, exercise_visible)
        VALUES ($1, 'fixture', 'FIXTURE', TRUE, 'stamp', FALSE, FALSE, FALSE, 0, 3, '',
            now(), now(), now(), '', '', '', TRUE)"#,
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let token = st
        .token
        .issue(user_id, Role::Admin, "fixture", "stamp")
        .unwrap();
    let team: i32 = sqlx::query_scalar(
        r#"INSERT INTO "Teams" (name, locked, invite_token, captain_id)
        VALUES ('fixture', FALSE, 'fixture', $1) RETURNING id"#,
    )
    .bind(user_id)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    let game: i32 = sqlx::query_scalar(r#"INSERT INTO "Games"
        (title, public_key, private_key, hidden, practice_mode, summary, content,
         accept_without_review, allow_user_submissions, writeup_required,
         team_member_count_limit, container_count_limit, start_time_utc, end_time_utc,
         writeup_deadline, writeup_note, blood_bonus_value, ad_allow_snapshot_download, ad_scoring_paused)
        VALUES ('fixture', '', '', TRUE, TRUE, '', '', TRUE, FALSE, FALSE, 0, 3,
            now() + interval '1 hour', now() + interval '2 hours', now(), '', 0, TRUE, FALSE)
        RETURNING id"#).fetch_one(&fixture.pool).await.unwrap();
    let router = Router::new()
        .route("/api/account/avatar", put(super::account::avatar))
        .route("/api/team/{id}/avatar", put(super::team::avatar))
        .route(
            "/api/edit/games/{id}/poster",
            put(super::edit::update_poster),
        )
        .route("/api/admin/config/logo", post(super::admin::logo_upload))
        .route(
            "/api/admin/config/logo/stage/{operation_id}",
            post(super::admin::stage_branding),
        )
        .route("/api/admin/config", put(super::admin::update_config))
        .route("/assets/{hash}/{filename}", get(super::assets::download))
        .with_state(st.clone());
    let stage_id = Uuid::new_v4();
    let paths = [
        ("/api/account/avatar".to_string(), "PUT"),
        (format!("/api/team/{team}/avatar"), "PUT"),
        (format!("/api/edit/games/{game}/poster"), "PUT"),
        ("/api/admin/config/logo".to_string(), "POST"),
        (format!("/api/admin/config/logo/stage/{stage_id}"), "POST"),
    ];
    for (index, (path, method)) in paths.iter().enumerate() {
        let bad = router
            .clone()
            .oneshot(upload(
                path,
                method,
                &token,
                Uuid::new_v4(),
                b"not an image",
            ))
            .await
            .unwrap();
        assert_eq!(bad.status(), StatusCode::BAD_REQUEST, "{path}");
        let bytes = png(index as u8);
        let hash = crate::utils::codec::sha256_hex(&bytes);
        let invalid_hash = crate::utils::codec::sha256_hex(b"not an image");
        assert!(!st.storage.exists(&invalid_hash).await);
        let response = router
            .clone()
            .oneshot(upload(path, method, &token, Uuid::new_v4(), &bytes))
            .await
            .unwrap();
        json_ok(response).await;
        if index == 4 {
            // Cache the staged asset's denial before the settings commit.
            assert_eq!(
                get_asset(&router, &hash, false).await.status(),
                StatusCode::FORBIDDEN
            );
            let revision: i64 = sqlx::query_scalar(
                r#"SELECT revision FROM "PlatformSettingsState" WHERE singleton=1"#,
            )
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
            let payload = serde_json::json!({"operationId":stage_id,"expectedRevision":revision,"brandingAction":"Set"});
            for _ in 0..2 {
                let response = router
                    .clone()
                    .oneshot(
                        Request::builder()
                            .method("PUT")
                            .uri("/api/admin/config")
                            .header(header::AUTHORIZATION, format!("Bearer {token}"))
                            .header(header::CONTENT_TYPE, "application/json")
                            .body(Body::from(payload.to_string()))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                json_ok(response).await;
                assert_eq!(
                    get_asset(&router, &hash, false).await.status(),
                    StatusCode::OK
                );
            }
        }
        for ranged in [false, true] {
            let response = get_asset(&router, &hash, ranged).await;
            assert_eq!(
                response.status(),
                if ranged {
                    StatusCode::PARTIAL_CONTENT
                } else {
                    StatusCode::OK
                }
            );
            assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
            assert_eq!(response.headers()[header::CONTENT_DISPOSITION], "inline");
            assert_eq!(response.headers()["x-content-type-options"], "nosniff");
            let actual = to_bytes(response.into_body(), 1_048_576).await.unwrap();
            assert_eq!(&actual[..], if ranged { &bytes[8..16] } else { &bytes[..] });
        }
    }
    let mut anonymous = upload(&paths[0].0, "PUT", &token, Uuid::new_v4(), &png(6));
    anonymous.headers_mut().remove(header::AUTHORIZATION);
    assert_eq!(
        router.clone().oneshot(anonymous).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    // Clearing branding must retire the previously public authorization too.
    let revision: i64 =
        sqlx::query_scalar(r#"SELECT revision FROM "PlatformSettingsState" WHERE singleton=1"#)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    let clear = serde_json::json!({"operationId":Uuid::new_v4(),"expectedRevision":revision,"brandingAction":"Clear"});
    json_ok(
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/admin/config")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(clear.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        get_asset(&router, &crate::utils::codec::sha256_hex(&png(4)), false)
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    drop(router);
    drop(st);
    fixture.close().await;
    std::fs::remove_dir_all(root).unwrap();
}
