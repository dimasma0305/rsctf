use super::*;
use crate::{
    app_state::AppState,
    migrations::{Migrator, MigratorTrait},
    models::internal::configs::AppConfig,
    services::{cache::InMemoryCache, container::NoopContainerManager, token::TokenService},
    storage::LocalBlobStorage,
    utils::test_database::TestDatabase,
};
use axum::{
    body::{to_bytes, Body},
    http::Request,
    routing::get,
    Router,
};
use sea_orm::SqlxPostgresConnector;
use tower::ServiceExt;

#[test]
fn head_replacement_escapes_values_and_keeps_the_application_assets() {
    let template = include_str!("../../../web/index.html");
    let meta = Metadata {
        title: "\"</title><script>alert(1)</script>".into(),
        description: "<img src=x> & 'quoted'".into(),
        canonical: Some("https://ctf.example/posts/a%26bcdefg".into()),
        image: Some("https://ctf.example/assets/logo".into()),
        indexable: true,
        status: StatusCode::OK,
    };
    let html = render(template, &meta);
    assert_eq!(html.matches("<title>").count(), 1);
    assert_eq!(html.matches("name=\"description\"").count(), 1);
    assert!(!html.contains("<script>alert"));
    assert!(html.contains("&lt;script&gt;"));
    assert!(html.contains("&amp; &#39;quoted&#39;"));
    assert!(html.contains("/src/main.tsx"));
    assert!(html.contains("rel=\"icon\""));
    assert!(html.contains("name=\"robots\" content=\"index, follow\""));
    assert_eq!(detail_path("posts", "a&bcdefg"), "/posts/a&bcdefg");
}

async fn read(router: &Router, path: &str) -> (StatusCode, axum::http::HeaderMap, String) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header("host", "attacker.example")
                .header("x-forwarded-host", "attacker.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = String::from_utf8(
        to_bytes(response.into_body(), 1_000_000)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    (status, headers, body)
}

#[tokio::test]
async fn seo_api_responses_are_fetchable_but_not_search_results() {
    let router = Router::new()
        .route(
            "/api/config",
            get(|| async { axum::Json(serde_json::json!({"title":"CTF"})) }),
        )
        .route("/apiary", get(|| async { "public page" }))
        .layer(axum::middleware::from_fn(exclude_api_indexing));
    let (status, headers, body) = read(&router, "/api/config").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["x-robots-tag"], "noindex, nofollow");
    assert!(body.contains("CTF"));
    assert!(!read(&router, "/apiary")
        .await
        .1
        .contains_key("x-robots-tag"));
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn seo_http_public_metadata_crawl_policy_and_sitemap_visibility() {
    let fixture = TestDatabase::new("seo_http").await;
    let db = SqlxPostgresConnector::from_sqlx_postgres_pool(fixture.pool.clone());
    Migrator::up(&db, None).await.unwrap();
    let root = std::env::temp_dir().join(format!("rsctf-seo-{}", uuid::Uuid::new_v4()));
    let mut config = AppConfig::default();
    config.public_url = Some("https://ctf.example/".into());
    let state = AppState::new(
        db,
        Arc::new(config),
        Arc::new(InMemoryCache::new()),
        Arc::new(LocalBlobStorage::new(root)),
        TokenService::new("0123456789abcdef0123456789abcdef", 3600),
        Arc::new(NoopContainerManager),
    );
    for (key, value) in [
        ("GlobalConfig:Title", "Example & Friends"),
        ("GlobalConfig:Description", "Learn <security> together"),
        ("GlobalConfig:LogoHash", "brandhash"),
        ("Registry:Password", "NEVER-PUBLIC"),
    ] {
        sqlx::query(r#"INSERT INTO "Configs" (config_key, value) VALUES ($1, $2) ON CONFLICT (config_key) DO UPDATE SET value = EXCLUDED.value"#).bind(key).bind(value).execute(&fixture.pool).await.unwrap();
    }
    let game: i32 = sqlx::query_scalar(r#"INSERT INTO "Games"
        (title, public_key, private_key, hidden, practice_mode, summary, content,
         accept_without_review, allow_user_submissions, writeup_required,
         team_member_count_limit, container_count_limit, start_time_utc, end_time_utc,
         writeup_deadline, writeup_note, blood_bonus_value, ad_allow_snapshot_download, ad_scoring_paused)
        VALUES ('Open CTF', '', 'NEVER-PUBLIC', FALSE, TRUE, 'Play and learn', 'PRIVATE CHALLENGE DATA', TRUE, FALSE, FALSE, 0, 3,
            now() + interval '1 hour', now() + interval '2 hours', now(), '', 0, TRUE, FALSE)
        RETURNING id"#).fetch_one(&fixture.pool).await.unwrap();
    // Use the actual schema without adding user/author data to SEO projections.
    sqlx::query(r#"INSERT INTO "Posts" (id, title, summary, content, is_pinned, update_time_utc) VALUES ('a&bcdefg', 'News <script>bad</script>', 'Public update', 'Full post', FALSE, now())"#).execute(&fixture.pool).await.unwrap();
    let template = Arc::new(include_str!("../../../web/index.html").to_owned());
    let page_state = state.clone();
    let router = Router::new()
        .route("/robots.txt", get(robots))
        .route("/sitemap.xml", get(sitemap))
        .fallback(get(move |uri: Uri| {
            page(page_state.clone(), template.clone(), uri)
        }))
        .with_state(state.clone());

    let (status, headers, home) = read(&router, "/?token=never-in-canonical").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert!(home.contains("Example &amp; Friends::CTF"));
    assert!(home.contains("Learn &lt;security&gt; together"));
    assert!(home.contains("href=\"https://ctf.example/\""));
    assert!(home.contains("https://ctf.example/assets/brandhash/logo"));
    assert!(!home.contains("attacker.example"));
    assert!(!home.contains("never-in-canonical"));
    let (_, _, event) = read(&router, &format!("/games/{game}")).await;
    assert!(event.contains("Open CTF - Example"));
    assert!(event.contains("Play and learn"));
    assert!(!event.contains("NEVER-PUBLIC"));
    assert!(!event.contains("PRIVATE CHALLENGE DATA"));
    let (status, _, post) = read(&router, "/posts/a%26bcdefg").await;
    assert_eq!(status, StatusCode::OK);
    assert!(post.contains("News &lt;script&gt;bad&lt;/script&gt;"));

    let (status, _, xml) = read(&router, "/sitemap.xml").await;
    assert_eq!(status, StatusCode::OK);
    assert!(xml.contains(&format!("https://ctf.example/games/{game}")));
    assert!(xml.contains("https://ctf.example/posts/a&amp;bcdefg"));
    assert!(!xml.contains("/admin"));
    let (_, _, robots) = read(&router, "/robots.txt").await;
    assert!(robots.contains("Sitemap: https://ctf.example/sitemap.xml"));
    assert!(
        !robots.contains("Disallow: /api"),
        "public API reads are needed to render the SPA"
    );
    assert!(
        !robots.contains("Disallow: /admin"),
        "noindex must remain crawlable"
    );
    for path in [
        "/admin/settings",
        "/account/reset?token=secret",
        "/teams",
        "/games/1/monitor/submissions",
    ] {
        let (status, headers, body) = read(&router, path).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(headers["x-robots-tag"], "noindex, nofollow");
        assert!(!body.contains("rel=\"canonical\""));
        assert!(!body.contains("NEVER-PUBLIC"));
    }
    // Visibility changes take effect immediately, without stale metadata caches.
    sqlx::query(r#"UPDATE "Games" SET hidden = TRUE WHERE id = $1"#)
        .bind(game)
        .execute(&fixture.pool)
        .await
        .unwrap();
    let (status, headers, hidden) = read(&router, &format!("/games/{game}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(headers["x-robots-tag"], "noindex, nofollow");
    assert!(!hidden.contains("Open CTF"));
    assert!(!read(&router, "/sitemap.xml")
        .await
        .2
        .contains(&format!("/games/{game}")));
    assert_eq!(
        read(&router, "/does-not-exist").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        read(&router, "/posts/missing1").await.0,
        StatusCode::NOT_FOUND
    );
    sqlx::query(r#"DROP TABLE "Configs""#)
        .execute(&fixture.pool)
        .await
        .unwrap();
    let (status, headers, _) = read(&router, "/").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(headers["x-robots-tag"], "noindex, nofollow");
    drop(router);
    drop(state);
    fixture.close().await;
}
