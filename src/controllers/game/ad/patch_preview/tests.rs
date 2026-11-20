use super::*;
use axum::{
    body::{to_bytes, Body},
    http::StatusCode,
    Router,
};
use sea_orm::SqlxPostgresConnector;
use std::io::{Cursor, Write};
use tower::ServiceExt;
use uuid::Uuid;

fn zip(extra: bool) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default();
    z.start_file("Dockerfile", opts).unwrap();
    z.write_all(b"FROM scratch\n").unwrap();
    if extra {
        z.start_file("src/version.txt", opts).unwrap();
        z.write_all(b"private candidate contents").unwrap();
    }
    z.finish().unwrap().into_inner()
}

async fn request(
    router: &Router,
    method: &str,
    challenge: i32,
    resource: &str,
    user: Option<&CurrentUser>,
    body: Body,
) -> Response {
    let mut req = Request::builder()
        .method(method)
        .uri(format!("/api/game/1/ad/challenges/{challenge}/{resource}"))
        .header(header::CONTENT_TYPE, "application/zip")
        .body(body)
        .unwrap();
    if let Some(user) = user {
        req.extensions_mut().insert(user.clone());
    }
    router.clone().oneshot(req).await.unwrap()
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn player_download_preview_and_revocation_during_upload() {
    let db = crate::utils::test_database::TestDatabase::new("ad_patch_preview").await;
    sqlx::raw_sql(r#"
        CREATE TABLE "Games" (id INTEGER PRIMARY KEY, deletion_pending BOOLEAN, practice_mode BOOLEAN, start_time_utc TIMESTAMPTZ, end_time_utc TIMESTAMPTZ);
        CREATE TABLE "Teams" (id INTEGER PRIMARY KEY, captain_id UUID, deletion_pending BOOLEAN DEFAULT false);
        CREATE TABLE "TeamMembers" (team_id INTEGER, user_id UUID);
        CREATE TABLE "AspNetUsers" (id UUID PRIMARY KEY, role SMALLINT, email_confirmed BOOLEAN, security_stamp TEXT);
        CREATE TABLE "UserParticipations" (user_id UUID, game_id INTEGER, team_id INTEGER, participation_id INTEGER);
        CREATE TABLE "Participations" (id INTEGER PRIMARY KEY, game_id INTEGER, team_id INTEGER, status SMALLINT, token TEXT DEFAULT 'fixture', writeup_id INTEGER, division_id INTEGER, suspicion_score INTEGER DEFAULT 0);
        CREATE TABLE "IdentityObservations" (user_id UUID, game_id INTEGER, team_id INTEGER, participation_id INTEGER, observed_at_utc TIMESTAMPTZ);
        CREATE TABLE "GameChallenges" (id INTEGER PRIMARY KEY, game_id INTEGER, "Type" SMALLINT, ad_self_hosted BOOLEAN, is_enabled BOOLEAN DEFAULT true, review_status SMALLINT DEFAULT 0, deletion_pending BOOLEAN DEFAULT false);
        CREATE TABLE "AdTeamServices" (id INTEGER PRIMARY KEY, game_id INTEGER, participation_id INTEGER, challenge_id INTEGER);
        CREATE TABLE "Divisions" (id INTEGER PRIMARY KEY, game_id INTEGER, default_permissions INTEGER);
        CREATE TABLE "DivisionChallengeConfigs" (division_id INTEGER, challenge_id INTEGER, permissions INTEGER);
        INSERT INTO "Games" VALUES (1,false,true,now()-interval '1 hour',now()+interval '1 hour');
        INSERT INTO "Divisions" VALUES (7,1,256);
        INSERT INTO "GameChallenges" (id,game_id,"Type",ad_self_hosted) VALUES (10,1,4,false),(11,1,4,true),(12,1,4,false),(13,1,4,false);
        INSERT INTO "Participations" (id,game_id,team_id,status,division_id) VALUES (3,1,2,1,7),(6,1,5,1,7);
        INSERT INTO "AdTeamServices" VALUES (20,1,3,10),(21,1,3,11),(22,1,6,12),(23,1,3,13);
    "#).execute(&db.pool).await.unwrap();
    sqlx::raw_sql(crate::migrations::m0366_ad_patch_starters::UP_SQL)
        .execute(&db.pool)
        .await
        .unwrap();
    let user = CurrentUser {
        id: Uuid::new_v4(),
        name: "player".into(),
        role: crate::utils::enums::Role::User,
        security_stamp: "stamp".into(),
    };
    for sql in [
        r#"INSERT INTO "Teams" (id,captain_id) VALUES (2,$1)"#,
        r#"INSERT INTO "AspNetUsers" VALUES ($1,1,true,'stamp')"#,
        r#"INSERT INTO "UserParticipations" VALUES ($1,1,2,3)"#,
        r#"INSERT INTO "IdentityObservations" VALUES ($1,1,2,3,now())"#,
    ] {
        sqlx::query(sql)
            .bind(user.id)
            .execute(&db.pool)
            .await
            .unwrap();
    }
    let source = zip(false);
    let original = PatchManifest::from_zip(&source).unwrap().tree_id();
    sqlx::query(r#"INSERT INTO "AdPatchStarters" (challenge_id,tree_id,archive) SELECT id,$1,$2 FROM "GameChallenges" WHERE id<>13"#)
        .bind(&original).bind(&source).execute(&db.pool).await.unwrap();
    let st = crate::app_state::AppState::new(
        SqlxPostgresConnector::from_sqlx_postgres_pool(db.pool.clone()),
        Arc::new(crate::models::internal::configs::AppConfig::default()),
        Arc::new(crate::services::cache::InMemoryCache::new()),
        Arc::new(crate::storage::LocalBlobStorage::new(
            std::env::temp_dir().join(Uuid::new_v4().to_string()),
        )),
        crate::services::token::TokenService::new("0123456789abcdef0123456789abcdef", 60),
        Arc::new(crate::services::container::NoopContainerManager),
    );
    let router = super::super::web_router().with_state(st);
    let get = |challenge, resource| {
        request(
            &router,
            "GET",
            challenge,
            resource,
            Some(&user),
            Body::empty(),
        )
    };
    assert_eq!(
        request(&router, "GET", 10, "patch-starter", None, Body::empty())
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let download = get(10, "patch-starter").await;
    assert_eq!(download.status(), StatusCode::OK);
    assert_eq!(
        download.headers()[header::CACHE_CONTROL],
        "private, no-store"
    );
    assert_eq!(
        to_bytes(download.into_body(), 8192).await.unwrap().as_ref(),
        source.as_slice()
    );
    let candidate = zip(true);
    simultaneous_team_uploads(&db.pool, &router, &user, &candidate).await;
    let report = request(
        &router,
        "POST",
        10,
        "patch-preview",
        Some(&user),
        Body::from(candidate.clone()),
    )
    .await;
    assert_eq!(report.status(), StatusCode::OK);
    let json: serde_json::Value =
        serde_json::from_slice(&to_bytes(report.into_body(), 8192).await.unwrap()).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"starterTree":original,"candidateTree":PatchManifest::from_zip(&candidate).unwrap().tree_id(),"changes":[{"path":"src/version.txt","kind":"Added"}]})
    );
    for cid in [11, 12] {
        assert_eq!(
            get(cid, "patch-starter").await.status(),
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(get(13, "patch-preview").await.status(), StatusCode::OK);
    assert_eq!(
        get(13, "patch-starter").await.status(),
        StatusCode::NOT_FOUND
    );
    sqlx::raw_sql(r#"INSERT INTO "DivisionChallengeConfigs" VALUES (7,10,0)"#)
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        get(10, "patch-starter").await.status(),
        StatusCode::NOT_FOUND
    );
    sqlx::raw_sql(r#"DELETE FROM "DivisionChallengeConfigs"; UPDATE "GameChallenges" SET is_enabled=false WHERE id=10"#).execute(&db.pool).await.unwrap();
    assert_eq!(
        get(10, "patch-starter").await.status(),
        StatusCode::NOT_FOUND
    );
    sqlx::raw_sql(r#"UPDATE "GameChallenges" SET is_enabled=true; UPDATE "Games" SET end_time_utc=now()-interval '1 minute'"#).execute(&db.pool).await.unwrap();
    assert_eq!(
        get(10, "patch-starter").await.status(),
        StatusCode::NOT_FOUND
    );
    sqlx::raw_sql(r#"UPDATE "Games" SET end_time_utc=now()+interval '1 day',start_time_utc=now()+interval '1 hour'"#).execute(&db.pool).await.unwrap();
    assert_eq!(
        get(10, "patch-starter").await.status(),
        StatusCode::NOT_FOUND
    );
    sqlx::raw_sql(r#"UPDATE "Games" SET start_time_utc=now()-interval '1 hour'"#)
        .execute(&db.pool)
        .await
        .unwrap();

    // Revoke membership only once the request has passed initial admission and
    // begun reading the ZIP. A completed comparison must still be withheld.
    let pool = db.pool.clone();
    let stream = futures::stream::once(async move {
        sqlx::query(r#"UPDATE "Teams" SET captain_id = $1 WHERE id=2"#)
            .bind(Uuid::new_v4())
            .execute(&pool)
            .await
            .unwrap();
        Ok::<_, std::io::Error>(bytes::Bytes::from(candidate))
    });
    let revoked = request(
        &router,
        "POST",
        10,
        "patch-preview",
        Some(&user),
        Body::from_stream(stream),
    )
    .await;
    assert_eq!(revoked.status(), StatusCode::FORBIDDEN);
    drop(router);
    db.close().await;
}

async fn simultaneous_team_uploads(
    pool: &PgPool,
    router: &Router,
    captain: &CurrentUser,
    candidate: &[u8],
) {
    let mut users = vec![captain.clone()];
    for team in 30..34 {
        let user = CurrentUser {
            id: Uuid::new_v4(),
            ..captain.clone()
        };
        let mut tx = pool.begin().await.unwrap();
        for sql in [
            r#"INSERT INTO "Teams" (id,captain_id) VALUES ($1,$2)"#,
            r#"INSERT INTO "UserParticipations" VALUES ($2,1,$1,$1)"#,
            r#"INSERT INTO "IdentityObservations" VALUES ($2,1,$1,$1,now())"#,
        ] {
            sqlx::query(sql)
                .bind(team)
                .bind(user.id)
                .execute(&mut *tx)
                .await
                .unwrap();
        }
        sqlx::query(r#"INSERT INTO "AspNetUsers" VALUES ($1,1,true,'stamp')"#)
            .bind(user.id)
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO "Participations" (id, game_id,team_id,status) VALUES ($1,1,$1,1)"#,
        )
        .bind(team)
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query(r#"INSERT INTO "AdTeamServices" VALUES ($1,1,$1,10)"#)
            .bind(team)
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        users.push(user);
    }
    let barrier = Arc::new(tokio::sync::Barrier::new(users.len()));
    let requests = users.iter().map(|user| {
        let barrier = Arc::clone(&barrier);
        async move {
            let bytes = bytes::Bytes::copy_from_slice(candidate);
            let len = bytes.len();
            let stream = futures::stream::once(async move {
                barrier.wait().await;
                Ok::<_, std::io::Error>(bytes)
            });
            let mut req = Request::post("/api/game/1/ad/challenges/10/patch-preview")
                .header(header::CONTENT_TYPE, "application/zip")
                .header(header::CONTENT_LENGTH, len)
                .body(Body::from_stream(stream))
                .unwrap();
            req.extensions_mut().insert(user.clone());
            assert_eq!(
                router.clone().oneshot(req).await.unwrap().status(),
                StatusCode::OK
            );
        }
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        futures::future::join_all(requests),
    )
    .await
    .expect("small previews for five teams should be admitted together");
}
