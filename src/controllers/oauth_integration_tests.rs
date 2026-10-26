//! Exercise the actual Discord exchange, one-time state and issued session
//! against disposable PostgreSQL and a local provider, without real credentials.

use super::*;
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::{Form, Json};
use sea_orm::SqlxPostgresConnector;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tower::ServiceExt;

use crate::app_state::AppState;
use crate::migrations::{test_process_application_name, Migrator, MigratorTrait};
use crate::models::internal::configs::AppConfig;
use crate::services::cache::InMemoryCache;
use crate::services::container::NoopContainerManager;
use crate::services::token::TokenService;
use crate::storage::LocalBlobStorage;

async fn begin(st: &SharedState) -> (HeaderMap, CallbackQuery) {
    let response = start(
        State(st.clone()),
        Path("discord".into()),
        Query(StartQuery {
            return_url: Some("/games/1".into()),
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let location =
        reqwest::Url::parse(response.headers()[header::LOCATION].to_str().unwrap()).unwrap();
    let state = location
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .into_owned();
    let mut headers = HeaderMap::new();
    headers.append(header::COOKIE, "preferences=compact".parse().unwrap());
    headers.append(
        header::COOKIE,
        format!("{OAUTH_STATE_COOKIE}={state}").parse().unwrap(),
    );
    (
        headers,
        CallbackQuery {
            code: Some("test-discord-code".into()),
            state: Some(state),
        },
    )
}

fn provider(address: SocketAddr) -> Provider {
    Provider {
        name: "discord",
        authorize: format!("http://{address}/authorize"),
        token: format!("http://{address}/token"),
        userinfo: format!("http://{address}/userinfo"),
        scope: "identify email",
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn discord_split_cookies_issue_a_live_session_and_rejections_never_do() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .application_name(test_process_application_name());
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options.clone())
        .await
        .unwrap();
    let schema = format!("rsctf_oauth_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(6)
        .connect_with(options.options([("search_path", schema.as_str())]))
        .await
        .unwrap();
    let db = SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone());
    Migrator::up(&db, None).await.unwrap();
    let st = AppState::new(
        db,
        Arc::new(AppConfig::default()),
        Arc::new(InMemoryCache::new()),
        Arc::new(LocalBlobStorage::new(std::env::temp_dir().join(&schema))),
        TokenService::new("0123456789abcdef0123456789abcdef", 60),
        Arc::new(NoopContainerManager),
    );
    let user_id = Uuid::new_v4();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('rsctf.identity_neutral_insert', '1', true)")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "AspNetUsers"
        (id, user_name, normalized_user_name, email, normalized_email, email_confirmed,
         security_stamp, phone_number_confirmed, two_factor_enabled, lockout_enabled,
         access_failed_count, role, ip, last_signed_in_utc, last_visited_utc,
         register_time_utc, bio, real_name, std_number, exercise_visible)
        VALUES ($1, 'oauth-user', 'OAUTH-USER', 'oauth@example.test', 'OAUTH@EXAMPLE.TEST',
                TRUE, 'stamp', FALSE, FALSE, FALSE, 0, 1, '', NOW(), NOW(), NOW(), '', '', '', TRUE)"#)
        .bind(user_id).execute(&mut *tx).await.unwrap();
    sqlx::query(
        r#"INSERT INTO "Configs" (config_key, value) VALUES
        ('OAuthConfig:DiscordClientId', 'test-client-id'),
        ('OAuthConfig:DiscordClientSecret', 'test-client-secret')"#,
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let profile_mode = Arc::new(AtomicUsize::new(0));
    let token_requests = Arc::new(AtomicUsize::new(0));
    let calls = token_requests.clone();
    let mode = profile_mode.clone();
    let mock = Router::new()
        .route(
            "/token",
            axum::routing::post(
                move |Form(form): Form<std::collections::HashMap<String, String>>| {
                    let calls = calls.clone();
                    async move {
                        calls.fetch_add(1, Ordering::Relaxed);
                        assert_eq!(form["grant_type"], "authorization_code");
                        assert_eq!(form["code"], "test-discord-code");
                        assert!(form["code_verifier"].len() >= 43);
                        Json(serde_json::json!({ "access_token": "test-provider-token" }))
                    }
                },
            ),
        )
        .route(
            "/userinfo",
            get(move || {
                let mode = mode.clone();
                async move {
                    Json(
                        serde_json::json!({ "email": "oauth@example.test", "username": "oauth-user",
                    "verified": mode.load(Ordering::Relaxed) == 0 }),
                    )
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let peer = "192.0.2.1:12345".parse().unwrap();

    let (headers, query) = begin(&st).await;
    let replay_headers = headers.clone();
    let replay_query = CallbackQuery {
        code: query.code.clone(),
        state: query.state.clone(),
    };
    let result = callback_with_provider(st.clone(), provider(address), headers, query, peer).await;
    let response = response::callback_response(result, "discord", true);
    assert_eq!(response.headers()[header::LOCATION], "/games/1");
    let cookies: Vec<_> = response
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .map(|value| value.to_str().unwrap())
        .collect();
    assert_eq!(cookies.len(), 2);
    let session = cookies
        .iter()
        .find_map(|cookie| cookie.strip_prefix("RSCTF_Token="))
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    assert!(!session.is_empty());
    assert_eq!(st.token.verify(session).unwrap().sub, user_id.to_string());
    assert!(cookies
        .iter()
        .any(|cookie| cookie.starts_with("RSCTF_OAuthState=;") && cookie.contains("Max-Age=0")));

    // The issued cookie must authenticate the very next browser request even
    // when another Cookie field precedes it.
    let profile = crate::controllers::account::router()
        .with_state(st.clone())
        .oneshot(
            Request::builder()
                .uri("/api/account/profile")
                .header(header::COOKIE, "preferences=compact")
                .header(header::COOKIE, format!("RSCTF_Token={session}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(profile.status(), StatusCode::OK);

    let replay = callback_with_provider(
        st.clone(),
        provider(address),
        replay_headers,
        replay_query,
        peer,
    )
    .await;
    let replay = response::callback_response(replay, "discord", true);
    assert_eq!(
        replay.headers()[header::LOCATION],
        "/account/login?error=oauth_state_expired"
    );
    assert_eq!(token_requests.load(Ordering::Relaxed), 1);

    let (mut headers, query) = begin(&st).await;
    headers.insert(header::COOKIE, "preferences=compact".parse().unwrap());
    let rejected =
        callback_with_provider(st.clone(), provider(address), headers, query, peer).await;
    let rejected = response::callback_response(rejected, "discord", true);
    assert_eq!(
        rejected.headers()[header::LOCATION],
        "/account/login?error=oauth_state_mismatch"
    );
    assert_eq!(token_requests.load(Ordering::Relaxed), 1);

    profile_mode.store(1, Ordering::Relaxed);
    let (headers, query) = begin(&st).await;
    let rejected =
        callback_with_provider(st.clone(), provider(address), headers, query, peer).await;
    let rejected = response::callback_response(rejected, "discord", true);
    assert_eq!(
        rejected.headers()[header::LOCATION],
        "/account/login?error=oauth_email_unverified"
    );
    assert_eq!(rejected.headers().get_all(SET_COOKIE).iter().count(), 1);

    profile_mode.store(0, Ordering::Relaxed);
    sqlx::query(r#"UPDATE "AspNetUsers" SET email_confirmed=FALSE WHERE id=$1"#)
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
    let (headers, query) = begin(&st).await;
    let rejected =
        callback_with_provider(st.clone(), provider(address), headers, query, peer).await;
    let rejected = response::callback_response(rejected, "discord", true);
    assert_eq!(
        rejected.headers()[header::LOCATION],
        "/account/login?error=oauth_email_conflict"
    );
    assert_eq!(rejected.headers().get_all(SET_COOKIE).iter().count(), 1);
    let confirmed: bool =
        sqlx::query_scalar(r#"SELECT email_confirmed FROM "AspNetUsers" WHERE id=$1"#)
            .bind(user_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!confirmed);

    server.abort();
    let _ = server.await;
    drop(st);
    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
