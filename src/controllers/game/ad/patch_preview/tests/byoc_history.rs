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
}
