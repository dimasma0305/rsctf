use super::*;
use crate::services::ad::patches::PatchManifest;
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
    archive(&[("Dockerfile", text)], compression)
}

fn archive(entries: &[(&str, &[u8])], compression: zip::CompressionMethod) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for &(name, bytes) in entries {
        zip.start_file(
            name,
            zip::write::SimpleFileOptions::default().compression_method(compression),
        )
        .unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn ssh_starter(registration: &[u8]) -> Vec<u8> {
    archive(
        &[
            ("Dockerfile", b"FROM scratch\n"),
            ("src/main.py", b"print(1)\n"),
            (".rsctf-ssh.json", registration),
        ],
        zip::CompressionMethod::Stored,
    )
}

#[test]
fn data_directory_is_an_optional_single_normalized_application_path() {
    for (bytes, valid) in [
        (b"/data\r\n".as_slice(), true),
        (b"/var/lib/rehearsal", true),
        (b"/data/../root", false),
        (b"/", false),
        (b"/var/lib/a\n/var/lib/b", false),
    ] {
        let zip = archive(
            &[("Dockerfile", b"FROM scratch"), (".rsctf-data-dir", bytes)],
            zip::CompressionMethod::Stored,
        );
        assert_eq!(Registration::from_zip(&zip).is_ok(), valid);
    }
    assert!(
        Registration::from_zip(&zip(b"FROM scratch", zip::CompressionMethod::Stored))
            .unwrap()
            .data_mount
            .is_none()
    );
}

#[test]
fn ssh_registration_requires_unique_starter_files_and_normalized_container_paths() {
    let mapping = |source: &str, target: &str| serde_json::json!({"sourcePath":source,"containerPath":target});
    let valid = mapping("src/main.py", "/srv/game/main.py");
    for entries in [
        serde_json::json!([mapping("absent.py", "/srv/file")]),
        serde_json::json!([mapping(".rsctf-ssh.json", "/srv/file")]),
        serde_json::json!([mapping("src/main.py", "srv/main.py")]),
        serde_json::json!([mapping("src/main.py", "/srv/../etc/config")]),
        serde_json::json!([mapping("src/main.py", "/proc/self/environ")]),
        serde_json::json!([mapping("src/main.py", "/srv//main.py")]),
        serde_json::json!([valid, mapping("Dockerfile", "/srv/game/main.py")]),
        serde_json::json!([valid, mapping("src/main.py", "/srv/other.py")]),
    ] {
        assert!(
            Registration::from_zip(&ssh_starter(entries.to_string().as_bytes())).is_err(),
            "{entries}"
        );
    }
    assert!(Registration::from_zip(&ssh_starter(&vec![b' '; 16 * 1024 + 1])).is_err());
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
        CREATE TABLE "AdTeamServices" (id INTEGER PRIMARY KEY, game_id INTEGER, challenge_id INTEGER, participation_id INTEGER);
        CREATE TABLE "Participations" (id INTEGER PRIMARY KEY);
        INSERT INTO "Games" VALUES (1, now()+interval '1 day', false), (2, now()+interval '1 day', false);
        INSERT INTO "GameChallenges" VALUES (10,1,4,false,false),(11,1,4,true,false),(12,2,4,false,false),(13,1,0,false,false),(14,1,4,false,false);
        ALTER TABLE "Games" ADD COLUMN end_time_utc TIMESTAMPTZ DEFAULT now()+interval '2 days';
        ALTER TABLE "GameChallenges" ADD COLUMN build_status SMALLINT NOT NULL DEFAULT 0,
            ADD COLUMN build_image_digest TEXT;
        INSERT INTO "GameChallenges" (id,game_id,"Type",ad_self_hosted,deletion_pending)
            VALUES (15,1,4,false,false),(17,1,4,false,false);
        INSERT INTO "Participations" VALUES (1),(2);
    "#).execute(&db.pool).await.unwrap();
    for _ in 0..2 {
        sqlx::raw_sql(crate::migrations::m0366_ad_patch_starters::UP_SQL)
            .execute(&db.pool)
            .await
            .unwrap();
        sqlx::raw_sql(crate::migrations::m0367_ad_patch_revisions::UP_SQL)
            .execute(&db.pool)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO "AdPatchStarters"(challenge_id,tree_id,archive)
            VALUES(15,repeat('f',64),'legacy') ON CONFLICT DO NOTHING"#,
        )
        .execute(&db.pool)
        .await
        .unwrap();
        sqlx::raw_sql(crate::migrations::m0371_ad_patch_starter_images::UP_SQL)
            .execute(&db.pool)
            .await
            .unwrap();
        sqlx::raw_sql(crate::migrations::m0378_ad_ssh_sources::UP_SQL)
            .execute(&db.pool)
            .await
            .unwrap();
        sqlx::raw_sql(crate::migrations::m0381_ad_team_storage::UP_SQL)
            .execute(&db.pool)
            .await
            .unwrap();
        sqlx::raw_sql(crate::migrations::m0382_ad_team_endpoints::UP_SQL)
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
        Arc::new(crate::services::k8s::mock_storage_manager()),
    );
    let router = crate::controllers::edit::router().with_state(st.clone());
    let v1 = zip(b"FROM scratch\n", zip::CompressionMethod::Stored);
    let v2 = zip(b"FROM scratch\n# v2\n", zip::CompressionMethod::Stored);
    let with_data = archive(
        &[
            ("Dockerfile", b"FROM scratch"),
            (".rsctf-data-dir", b"/data\n"),
        ],
        zip::CompressionMethod::Stored,
    );
    let (status, registered) = request(&router, "PUT", 17, Some(&user), with_data, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(registered["dataMount"], "/data");
    assert_eq!(
        request(&router, "GET", 17, Some(&user), vec![], None)
            .await
            .1["dataMount"],
        "/data"
    );
    use crate::services::container::{ContainerResourceLimits, ContainerSpec, TeamStorage};
    let spec = ContainerSpec::ad_service(
        format!("example.test/app@sha256:{}", "a".repeat(64)),
        ContainerResourceLimits {
            memory_limit: 128,
            cpu_count: 1,
            storage_limit: 64,
        },
        8000,
        1,
        false,
        String::new(),
    );
    let configure = |mut spec: ContainerSpec, participation| {
        let state = &st;
        async move {
            TeamStorage::configure(state, &mut spec, 1, participation, 17)
                .await
                .unwrap();
            spec.team_storage.unwrap()
        }
    };
    let (first, retry, other) = tokio::join!(
        configure(spec.clone(), 1),
        configure(spec.clone(), 1),
        configure(spec, 2)
    );
    assert!(first.claim_uid.is_some());
    assert_eq!(first.allocation_id, retry.allocation_id);
    assert_eq!(first.claim_uid, retry.claim_uid);
    assert_ne!(first.allocation_id, other.allocation_id);
    assert_ne!(first.claim_uid, other.claim_uid);
    assert!(first.endpoint_uid.is_some());
    assert_eq!(first.endpoint_uid, retry.endpoint_uid);
    assert_ne!(first.endpoint_uid, other.endpoint_uid);
    sqlx::query(r#"INSERT INTO "AdTeamServices" VALUES (44,1,17,1)"#)
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(
        TeamStorage::supports_patch_activation(&st, 44, None, Some("team-pod"))
            .await
            .unwrap()
    );
    assert!(
        !TeamStorage::supports_patch_activation(&st, 45, None, Some("team-pod"))
            .await
            .unwrap()
    );
    assert_eq!(
        request(
            &router,
            "PUT",
            17,
            Some(&user),
            v2.clone(),
            registered["treeId"].as_str()
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
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
    let starter_ref = || {
        sqlx::query_as::<_, (Option<String>, bool)>(
            r#"SELECT runtime_image_ref,awaiting_image_build FROM "AdPatchStarters" WHERE challenge_id=10"#,
        ).fetch_one(&db.pool)
    };
    assert_eq!(starter_ref().await.unwrap(), (None, true));
    let first_image = format!("sha256:{}", "a".repeat(64));
    let next_image = format!("sha256:{}", "b".repeat(64));
    sqlx::query(r#"UPDATE "GameChallenges" SET build_status=1,build_image_digest=$1 WHERE id=15"#)
        .bind(&first_image)
        .execute(&db.pool)
        .await
        .unwrap();
    let legacy = request(&router, "GET", 15, Some(&user), vec![], None).await;
    assert_eq!(legacy.0, StatusCode::OK);
    assert!(legacy.1["runtimeImageRef"].is_null());
    assert_eq!(legacy.1["awaitingImageBuild"], false);
    assert!(legacy.1.get("sshSources").is_none());
    let paths =
        serde_json::json!([{"sourcePath":"src/main.py","containerPath":"/srv/game/main.py"}]);
    let registered = ssh_starter(paths.to_string().as_bytes());
    let previous = legacy.1["treeId"].as_str();
    let saved = request(
        &router,
        "PUT",
        15,
        Some(&user),
        registered.clone(),
        previous,
    )
    .await;
    assert_eq!(saved.0, StatusCode::OK, "{}", saved.1);
    assert_eq!(saved.1["sshSources"], paths);
    assert_eq!(
        request(&router, "GET", 15, Some(&user), vec![], None)
            .await
            .1["sshSources"],
        paths
    );
    sqlx::query(r#"DELETE FROM "GameChallenges" WHERE id=15"#)
        .execute(&db.pool)
        .await
        .unwrap();
    for image in [&first_image, &next_image] {
        sqlx::query(
            r#"UPDATE "GameChallenges" SET build_status=1,build_image_digest=$1 WHERE id=10"#,
        )
        .bind(image)
        .execute(&db.pool)
        .await
        .unwrap();
        assert_eq!(
            starter_ref().await.unwrap(),
            (Some(first_image.clone()), false)
        );
    }
    let protected = |image: String| {
        sqlx::query_scalar::<_, bool>(
            r#"SELECT EXISTS(SELECT 1 FROM "AdProtectedLocalImages" WHERE image_id=$1)"#,
        )
        .bind(image)
        .fetch_one(&db.pool)
    };
    assert!(protected(first_image.clone()).await.unwrap());
    assert_eq!(
        original,
        serde_json::json!({"treeId":tree, "archiveBytes":v1.len(),"runtimeImageRef":null,"awaitingImageBuild":true})
    );
    let repacked = zip(b"FROM scratch\n", zip::CompressionMethod::Deflated);
    assert_eq!(
        request(&router, "PUT", 10, Some(&user), repacked, None).await,
        (
            StatusCode::OK,
            serde_json::json!({"treeId":tree, "archiveBytes":v1.len(),"runtimeImageRef":first_image,"awaitingImageBuild":false})
        )
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
    assert_eq!(
        starter_ref().await.unwrap(),
        (Some(next_image.clone()), false)
    );
    assert!(!protected(first_image.clone()).await.unwrap());
    assert!(protected(next_image.clone()).await.unwrap());
    let r1 = Registration::from_zip(&v1).unwrap();
    let r2 = Registration::from_zip(&v2).unwrap();
    let (a, b) = tokio::join!(
        register(&db.pool, &user, 1, 14, &v1, &r1, None),
        register(&db.pool, &user, 1, 14, &v2, &r2, None),
    );
    assert!(matches!(
        (&a, &b),
        (Ok(_), Err(AppError::Conflict(_))) | (Err(AppError::Conflict(_)), Ok(_))
    ));
    sqlx::query(r#"UPDATE "Games" SET start_time_utc=now()-interval '1 day' WHERE id=1"#)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "GameChallenges"(id,game_id,"Type",ad_self_hosted,deletion_pending) VALUES(16,1,4,false,false)"#)
        .execute(&db.pool).await.unwrap();
    assert_eq!(
        request(&router, "PUT", 16, Some(&user), registered, None)
            .await
            .0,
        StatusCode::CONFLICT
    );
    sqlx::query(r#"UPDATE "GameChallenges" SET build_status=1,build_image_digest=$1 WHERE id=14"#)
        .bind(&first_image)
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(sqlx::query_as::<_,(Option<String>,bool)>(
        r#"SELECT runtime_image_ref,awaiting_image_build FROM "AdPatchStarters" WHERE challenge_id=14"#,
    ).fetch_one(&db.pool).await.unwrap(), (None,false));
    assert_eq!(
        request(&router, "PUT", 10, Some(&user), v1.clone(), Some(&second))
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
    // Rescheduling an event must not invalidate the shared original behind
    // a team's persisted source manifests.
    sqlx::raw_sql(
        r#"UPDATE "Games" SET start_time_utc=now()+interval '1 hour' WHERE id=1;
        INSERT INTO "AdTeamServices" VALUES (42,1,10);
        INSERT INTO "AdPatchSourceUsage" VALUES (42,512);"#,
    )
    .execute(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        request(&router, "PUT", 10, Some(&user), v1, Some(&second))
            .await
            .0,
        StatusCode::CONFLICT
    );
    sqlx::query(
        r#"UPDATE "Games" SET end_time_utc=clock_timestamp()-interval '1 second' WHERE id=1"#,
    )
    .execute(&db.pool)
    .await
    .unwrap();
    assert!(!protected(next_image).await.unwrap());
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
