use super::*;
use axum::{
    body::{to_bytes, Body},
    http::{header, Request as HttpRequest, StatusCode},
    Router,
};
use sea_orm::SqlxPostgresConnector;
use std::{
    io::{Cursor, Write},
    sync::Arc,
};
use tower::ServiceExt;
use uuid::Uuid;

fn zip(text: &[u8], compression: zip::CompressionMethod) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file(
        "Dockerfile",
        zip::write::SimpleFileOptions::default().compression_method(compression),
    )
    .unwrap();
    zip.write_all(text).unwrap();
    zip.finish().unwrap().into_inner()
}

async fn request(
    router: &Router,
    method: &str,
    challenge: i32,
    user: Option<&CurrentUser>,
    bytes: Vec<u8>,
    expected: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut req = HttpRequest::builder()
        .method(method)
        .uri(format!(
            "/api/edit/games/1/challenges/{challenge}/patch-starter"
        ))
        .header(header::CONTENT_TYPE, "application/zip");
    if let Some(tree) = expected {
        req = req.header("x-rsctf-expected-tree", tree);
    }
    let mut req = req.body(Body::from(bytes)).unwrap();
    if let Some(user) = user {
        req.extensions_mut().insert(user.clone());
    }
    let res = router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = to_bytes(res.into_body(), 8192).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap_or_default())
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn starter_http_scope_replay_and_concurrent_registration() {
    let db = crate::utils::test_database::TestDatabase::new("ad_patch_starter").await;
    sqlx::raw_sql(r#"
        CREATE TABLE "GameManagers" (id SERIAL PRIMARY KEY, game_id INTEGER, user_id UUID);
        CREATE TABLE "Games" (id INTEGER PRIMARY KEY, start_time_utc TIMESTAMPTZ, deletion_pending BOOLEAN);
        CREATE TABLE "GameChallenges" (id INTEGER PRIMARY KEY, game_id INTEGER, "Type" SMALLINT, ad_self_hosted BOOLEAN, deletion_pending BOOLEAN);
        INSERT INTO "Games" VALUES (1, now()+interval '1 day', false), (2, now()+interval '1 day', false);
        INSERT INTO "GameChallenges" VALUES (10,1,4,false,false),(11,1,4,true,false),(12,2,4,false,false),(13,1,0,false,false),(14,1,4,false,false);
    "#).execute(&db.pool).await.unwrap();
    for _ in 0..2 {
        sqlx::raw_sql(crate::migrations::m0366_ad_patch_starters::UP_SQL)
            .execute(&db.pool)
            .await
            .unwrap();
    }
    let user = CurrentUser {
        id: Uuid::new_v4(),
        name: "organizer".into(),
        role: crate::utils::enums::Role::User,
        security_stamp: "test".into(),
    };
    sqlx::query(r#"INSERT INTO "GameManagers" (game_id,user_id) VALUES (1,$1)"#)
        .bind(user.id)
        .execute(&db.pool)
        .await
        .unwrap();
    let outsider = CurrentUser {
        id: Uuid::new_v4(),
        ..user.clone()
    };
    let root = std::env::temp_dir().join(format!("patch-starter-{}", Uuid::new_v4()));
    let st = crate::app_state::AppState::new(
        SqlxPostgresConnector::from_sqlx_postgres_pool(db.pool.clone()),
        Arc::new(crate::models::internal::configs::AppConfig::default()),
        Arc::new(crate::services::cache::InMemoryCache::new()),
        Arc::new(crate::storage::LocalBlobStorage::new(root)),
        crate::services::token::TokenService::new("0123456789abcdef0123456789abcdef", 60),
        Arc::new(crate::services::container::NoopContainerManager),
    );
    let router = crate::controllers::edit::router().with_state(st);
    let v1 = zip(b"FROM scratch\n", zip::CompressionMethod::Stored);
    let v2 = zip(b"FROM scratch\n# v2\n", zip::CompressionMethod::Stored);
    for method in ["GET", "PUT"] {
        assert_eq!(
            request(&router, method, 10, None, v1.clone(), None).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(&router, method, 10, Some(&outsider), v1.clone(), None)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        for challenge in [11, 12, 13] {
            assert_eq!(
                request(&router, method, challenge, Some(&user), v1.clone(), None)
                    .await
                    .0,
                StatusCode::NOT_FOUND
            );
        }
    }
    assert_eq!(
        request(&router, "GET", 10, Some(&user), vec![], None).await,
        (StatusCode::OK, serde_json::Value::Null)
    );
    assert_eq!(
        request(&router, "PUT", 10, Some(&user), vec![0], None)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let (ok, original) = request(&router, "PUT", 10, Some(&user), v1.clone(), None).await;
    assert_eq!(ok, StatusCode::OK, "{original}");
    let tree = PatchManifest::from_zip(&v1).unwrap().tree_id();
    assert_eq!(
        original,
        serde_json::json!({"treeId":tree, "archiveBytes":v1.len()})
    );
    let repacked = zip(b"FROM scratch\n", zip::CompressionMethod::Deflated);
    assert_eq!(
        request(&router, "PUT", 10, Some(&user), repacked, None).await,
        (StatusCode::OK, original.clone())
    );
    let stored: Vec<u8> =
        sqlx::query_scalar(r#"SELECT archive FROM "AdPatchStarters" WHERE challenge_id=10"#)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        stored, v1,
        "equivalent repack must preserve the original ZIP"
    );
    assert_eq!(
        request(&router, "PUT", 10, Some(&user), v2.clone(), None)
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(&router, "PUT", 10, Some(&user), v2.clone(), Some(&tree))
            .await
            .0,
        StatusCode::OK
    );
    let second = PatchManifest::from_zip(&v2).unwrap().tree_id();
    let (a, b) = tokio::join!(
        register(&db.pool, &user, 1, 14, &v1, &tree, None),
        register(&db.pool, &user, 1, 14, &v2, &second, None),
    );
    assert!(matches!(
        (&a, &b),
        (Ok(_), Err(AppError::Conflict(_))) | (Err(AppError::Conflict(_)), Ok(_))
    ));
    sqlx::query(r#"UPDATE "Games" SET start_time_utc=now()-interval '1 day' WHERE id=1"#)
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        request(&router, "PUT", 10, Some(&user), v1, Some(&second))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(&router, "GET", 10, Some(&user), vec![], None)
            .await
            .1["treeId"],
        second
    );
    sqlx::query(r#"DELETE FROM "GameChallenges" WHERE id=10"#)
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            r#"SELECT COUNT(*) FROM "AdPatchStarters" WHERE challenge_id=10"#
        )
        .fetch_one(&db.pool)
        .await
        .unwrap(),
        0
    );
    drop(router);
    db.close().await;
}
