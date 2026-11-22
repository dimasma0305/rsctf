use super::*;

async fn get(router: &Router, path: &str, user: Option<&CurrentUser>) -> Response {
    let mut req = Request::get(path).body(Body::empty()).unwrap();
    if let Some(user) = user {
        signed_in(&mut req, user);
    }
    router.clone().oneshot(req).await.unwrap()
}

pub(super) async fn check(pool: &PgPool, router: &Router, user: &CurrentUser) {
    sqlx::raw_sql(crate::migrations::m0368_byoc_source_history::UP_SQL)
        .execute(pool)
        .await
        .unwrap();
    sqlx::raw_sql(r#"INSERT INTO "AdTeamServices" VALUES (24,1,6,11);
        INSERT INTO "AdByocSourceHistory"(service_id,revision,state,files) VALUES (21,1,'observed','{"own.rs":null}'),(24,1,'observed','{"other.rs":null}')"#)
        .execute(pool).await.unwrap();
    let own = "/api/Game/1/Ad/Byoc/Source/11";
    let admin_url = "/api/ad/admin/1/Services/24/ByocSource";
    assert_eq!(
        get(router, own, None).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get(router, admin_url, Some(user)).await.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        get(router, "/api/Game/1/Ad/Byoc/Source/10", Some(user))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    let own = get(router, own, Some(user)).await;
    assert_eq!(own.status(), StatusCode::OK);
    assert_eq!(own.headers()[header::CACHE_CONTROL], "private, no-store");
    let bytes = to_bytes(own.into_body(), 8192).await.unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    assert!(text.contains("own.rs") && !text.contains("other.rs"));
    let admin = CurrentUser {
        role: crate::utils::enums::Role::Admin,
        ..user.clone()
    };
    let response = get(router, admin_url, Some(&admin)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 8192).await.unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    assert!(text.contains("other.rs") && !text.contains("own.rs"));
    assert_eq!(
        get(
            router,
            "/api/ad/admin/2/Services/24/ByocSource",
            Some(&admin)
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    sqlx::raw_sql(crate::migrations::m0378_ad_ssh_sources::UP_SQL)
        .execute(pool)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"INSERT INTO "AdTeamServices" VALUES (25,1,6,10);
      INSERT INTO "AdHostedSshSourceHistory"(service_id,revision,state,files) VALUES
      (20,1,'observed','{"own.py":null}'), (25,1,'observed','{"peer.py":null}')"#,
    )
    .execute(pool)
    .await
    .unwrap();
    let team_route = "/api/game/1/ad/challenges/10/ssh-source";
    assert_eq!(
        get(router, team_route, None).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let response = get(router, team_route, Some(user)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "private, no-store"
    );
    let bytes = to_bytes(response.into_body(), 8192).await.unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    assert!(text.contains("own.py") && !text.contains("peer.py"));
    compare_saved_zip(pool, router, user).await;
    let peer = "/api/ad/admin/1/Services/25/SshSource";
    assert_eq!(
        get(router, peer, Some(user)).await.status(),
        StatusCode::FORBIDDEN
    );
    let response = get(router, peer, Some(&admin)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 8192).await.unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    assert!(text.contains("peer.py") && !text.contains("own.py"));
    assert_eq!(
        get(
            router,
            "/api/ad/admin/2/Services/25/SshSource",
            Some(&admin)
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(
            router,
            "/api/ad/admin/1/Services/21/SshSource",
            Some(&admin)
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
}

async fn compare_saved_zip(pool: &PgPool, router: &Router, user: &CurrentUser) {
    use serde_json::json;
    use sha2::{Digest, Sha256};
    let oid = hex::encode(Sha256::digest(b"blob 4\0test"));
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    zip.start_file("Dockerfile", options).unwrap();
    zip.write_all(b"FROM scratch\n").unwrap();
    for path in ["same", "mode", "modified", "in-zip", "unregistered"] {
        zip.start_file(
            path,
            options.unix_permissions(if path == "mode" { 0o755 } else { 0o644 }),
        )
        .unwrap();
        zip.write_all(b"test").unwrap();
    }
    let archive = zip.finish().unwrap().into_inner();
    let saved = super::history::save(router, user, 0, &archive).await;
    assert_eq!(saved.status(), StatusCode::OK);
    let result: serde_json::Value =
        serde_json::from_slice(&to_bytes(saved.into_body(), 4096).await.unwrap()).unwrap();
    let commit = result["commitId"].as_str().unwrap();
    let regular = json!({"blob":oid,"executable":false});
    let observed = json!({"same":regular,"mode":regular,"in-ssh":regular,"in-zip":null,
        "modified":{"blob":"c".repeat(64),"executable":false},"absent":null});
    sqlx::query(r#"INSERT INTO "AdHostedSshSourceHistory"(service_id,revision,state,files) VALUES(20,2,'observed',$1)"#)
        .bind(observed.to_string()).execute(pool).await.unwrap();
    let path = format!(
        "/api/game/1/ad/challenges/10/patch-revisions/1/ssh-comparison?service=20&commit={commit}"
    );
    assert_eq!(
        get(router, &path, None).await.status(),
        StatusCode::UNAUTHORIZED
    );
    for wrong in [
        path.replace("service=20", "service=25"),
        path.replace(commit, &oid),
        path.replace("challenges/10", "challenges/11"),
    ] {
        assert_eq!(
            get(router, &wrong, Some(user)).await.status(),
            StatusCode::NOT_FOUND
        );
    }
    let response = get(router, &path, Some(user)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "private, no-store"
    );
    let body: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(body["report"]["revision"], 2);
    assert_eq!(body["report"]["trackedFiles"], 6);
    assert_eq!(
        body["report"]["changes"],
        json!([{"path":"in-ssh","kind":"Deleted"},
        {"path":"in-zip","kind":"Added"},{"path":"mode","kind":"Modified"},{"path":"modified","kind":"Modified"}])
    );
    sqlx::query(r#"INSERT INTO "AdHostedSshSourceHistory"(service_id,revision,state,files) VALUES(20,3,'unavailable','{}')"#)
        .execute(pool).await.unwrap();
    let response = get(router, &path, Some(user)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(body, json!({"report":null}));
}
