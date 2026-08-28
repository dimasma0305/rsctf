use std::str::FromStr;
use std::sync::Arc;

use axum::body::to_bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, Request, StatusCode};
use axum::routing::get;
use axum::Router;
use sea_orm::SqlxPostgresConnector;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tower::ServiceExt;
use uuid::Uuid;

use super::*;
use crate::app_state::{AppState, SharedState};
use crate::models::internal::configs::AppConfig;
use crate::services::cache::InMemoryCache;
use crate::services::container::NoopContainerManager;
use crate::services::token::TokenService;
use crate::storage::LocalBlobStorage;

struct ApiTokenFixture {
    admin: sqlx::PgPool,
    pool: sqlx::PgPool,
    schema: String,
    owner_id: Uuid,
    security_stamp: String,
}

impl ApiTokenFixture {
    async fn new() -> Self {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("connect API-token test database");
        let schema = format!("api_tokens_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .expect("create API-token test schema");
        let options = PgConnectOptions::from_str(&database_url)
            .expect("parse API-token test database URL")
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(12)
            .connect_with(options)
            .await
            .expect("connect scoped API-token database");
        sqlx::raw_sql(
            r#"
            CREATE TABLE "AspNetUsers" (
                id UUID PRIMARY KEY,
                role SMALLINT NOT NULL,
                user_name TEXT,
                security_stamp TEXT
            );
            CREATE TABLE "ApiTokens" (
                id UUID PRIMARY KEY,
                name TEXT NOT NULL,
                token_hash TEXT NOT NULL,
                creator_id UUID REFERENCES "AspNetUsers"(id) ON DELETE CASCADE,
                created_at TIMESTAMPTZ NOT NULL,
                expires_at TIMESTAMPTZ,
                last_used_at TIMESTAMPTZ,
                is_revoked BOOLEAN NOT NULL DEFAULT FALSE,
                audience TEXT NOT NULL DEFAULT 'admin_api',
                security_stamp_hash TEXT
            );
            CREATE UNIQUE INDEX ux_apitokens_token_hash
                ON "ApiTokens"(token_hash) WHERE is_revoked = FALSE;
            "#,
        )
        .execute(&pool)
        .await
        .expect("create API-token fixture tables");
        let owner_id = Uuid::new_v4();
        let security_stamp = "live-admin-security-stamp".to_string();
        sqlx::query(
            r#"INSERT INTO "AspNetUsers" (id, role, user_name, security_stamp)
               VALUES ($1, $2, 'automation-admin', $3)"#,
        )
        .bind(owner_id)
        .bind(Role::Admin as i16)
        .bind(&security_stamp)
        .execute(&pool)
        .await
        .unwrap();
        Self {
            admin,
            pool,
            schema,
            owner_id,
            security_stamp,
        }
    }

    fn state(&self) -> SharedState {
        AppState::new(
            SqlxPostgresConnector::from_sqlx_postgres_pool(self.pool.clone()),
            Arc::new(AppConfig::default()),
            Arc::new(InMemoryCache::new()),
            Arc::new(LocalBlobStorage::new(
                std::env::temp_dir().join(format!("rsctf-api-token-test-{}", self.schema)),
            )),
            TokenService::new("0123456789abcdef0123456789abcdef", 60),
            Arc::new(NoopContainerManager),
        )
    }

    fn owner(&self) -> CurrentUser {
        CurrentUser {
            id: self.owner_id,
            role: Role::Admin,
            name: "automation-admin".to_string(),
            security_stamp: self.security_stamp.clone(),
        }
    }

    async fn insert_token(
        &self,
        token: &str,
        audience: &str,
        stamp: &str,
        revoked: bool,
        expired: bool,
    ) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO "ApiTokens"
                 (id, name, token_hash, creator_id, created_at, expires_at,
                  last_used_at, is_revoked, audience, security_stamp_hash)
               VALUES ($1, 'fixture', $2, $3, clock_timestamp(),
                       CASE WHEN $4 THEN clock_timestamp() - interval '1 second' END,
                       NULL, $5, $6, $7)"#,
        )
        .bind(id)
        .bind(sha256_str(token))
        .bind(self.owner_id)
        .bind(expired)
        .bind(revoked)
        .bind(audience)
        .bind(sha256_str(stamp))
        .execute(&self.pool)
        .await
        .unwrap();
        id
    }

    async fn cleanup(self) {
        self.pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{}" CASCADE"#, self.schema))
            .execute(&self.admin)
            .await
            .expect("drop API-token test schema");
    }
}

fn token(fill: char) -> String {
    format!(
        "{PERSONAL_TOKEN_PREFIX}{}",
        fill.to_string().repeat(PERSONAL_TOKEN_SECRET_CHARS)
    )
}

fn assert_unauthorized<T>(result: AppResult<T>) {
    match result {
        Err(AppError::Unauthorized) => {}
        Err(error) => panic!("expected unauthorized, got {error:?}"),
        Ok(_) => panic!("invalid managed token authenticated"),
    }
}

/// Run explicitly with `RSCTF_TEST_DATABASE_URL=postgres://... cargo test
/// generated_managed_token_authenticates_and_obeys_live_fences -- --ignored
/// --nocapture`.
#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn generated_managed_token_authenticates_and_obeys_live_fences() {
    let fixture = ApiTokenFixture::new().await;
    let first_state = fixture.state();
    let second_state = fixture.state();

    let response = generate_token(
        State(first_state.clone()),
        AdminUser(fixture.owner()),
        Json(ApiTokenCreateModel {
            name: "CI automation".to_string(),
            expires_in: Some(30),
        }),
    )
    .await
    .expect("generate managed token");
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL).unwrap(),
        "private, no-store"
    );
    assert_eq!(response.headers().get(header::PRAGMA).unwrap(), "no-cache");
    let body = to_bytes(response.into_body(), 16 * 1024).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let plaintext = payload["token"].as_str().unwrap().to_string();
    assert!(is_well_formed(&plaintext));
    sqlx::query(
        r#"INSERT INTO "ApiTokens"
             (id, name, token_hash, creator_id, created_at, is_revoked,
              audience, security_stamp_hash)
           VALUES ($1, 'revoked duplicate', $2, $3, clock_timestamp(), TRUE,
                   'admin_api', $4)"#,
    )
    .bind(Uuid::new_v4())
    .bind(sha256_str(&plaintext))
    .bind(fixture.owner_id)
    .bind(sha256_str(&fixture.security_stamp))
    .execute(&fixture.pool)
    .await
    .unwrap();

    let app = Router::new()
        .route(
            "/api/admin-token-probe",
            get(|AdminUser(user): AdminUser| async move { user.name }),
        )
        .layer(axum::middleware::from_fn_with_state(
            first_state.clone(),
            crate::middlewares::rate_limiter::global_middleware,
        ))
        .with_state(first_state.clone());
    let end_to_end = app
        .oneshot(
            Request::builder()
                .uri("/api/admin-token-probe")
                .header(header::AUTHORIZATION, format!("Bearer {plaintext}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(end_to_end.status(), StatusCode::OK);
    let probe_body = to_bytes(end_to_end.into_body(), 1_024).await.unwrap();
    assert_eq!(probe_body.as_ref(), b"automation-admin");

    let verified = authenticate(&second_state, &plaintext)
        .await
        .expect("generated token authenticates on another replica");
    assert_eq!(verified.user.id, fixture.owner_id);
    assert_eq!(verified.user.role, Role::Admin);
    assert_eq!(verified.audience, "admin_api");

    // Hot parallel clients may all authenticate, but the guarded update keeps
    // last-used metadata from becoming an update on every request.
    sqlx::query(r#"UPDATE "ApiTokens" SET last_used_at = NULL"#)
        .execute(&fixture.pool)
        .await
        .unwrap();
    let attempts = (0..12).map(|index| {
        let state = if index % 2 == 0 {
            first_state.clone()
        } else {
            second_state.clone()
        };
        let plaintext = plaintext.clone();
        tokio::spawn(async move { authenticate(&state, &plaintext).await })
    });
    for attempt in attempts {
        assert!(attempt.await.unwrap().is_ok());
    }
    assert!(sqlx::query_scalar::<_, Option<chrono::DateTime<Utc>>>(
        r#"SELECT last_used_at FROM "ApiTokens"
            WHERE token_hash = $1 AND is_revoked = FALSE"#,
    )
    .bind(sha256_str(&plaintext))
    .fetch_one(&fixture.pool)
    .await
    .unwrap()
    .is_some());

    sqlx::query(r#"UPDATE "AspNetUsers" SET security_stamp = 'rotated' WHERE id = $1"#)
        .bind(fixture.owner_id)
        .execute(&fixture.pool)
        .await
        .unwrap();
    assert_unauthorized(authenticate(&fixture.state(), &plaintext).await);

    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn managed_tokens_reject_wrong_authority_expiry_revocation_and_deleted_owners() {
    let fixture = ApiTokenFixture::new().await;

    let wrong_audience = token('a');
    fixture
        .insert_token(
            &wrong_audience,
            "player_api",
            &fixture.security_stamp,
            false,
            false,
        )
        .await;
    assert_unauthorized(authenticate(&fixture.state(), &wrong_audience).await);

    let expired = token('b');
    let expired_id = fixture
        .insert_token(&expired, "admin_api", &fixture.security_stamp, false, true)
        .await;
    assert_unauthorized(authenticate(&fixture.state(), &expired).await);

    let revoked = token('c');
    let revoked_id = fixture
        .insert_token(&revoked, "admin_api", &fixture.security_stamp, true, false)
        .await;
    assert_unauthorized(authenticate(&fixture.state(), &revoked).await);
    restore_token(
        State(fixture.state()),
        AdminUser(fixture.owner()),
        Path(revoked_id),
    )
    .await
    .expect("a live owner's non-expired token can be restored");
    assert!(authenticate(&fixture.state(), &revoked).await.is_ok());

    let expired_restore = restore_token(
        State(fixture.state()),
        AdminUser(fixture.owner()),
        Path(expired_id),
    )
    .await;
    match expired_restore {
        Err(AppError::Conflict(_)) => {}
        Err(error) => panic!("expected expired-token conflict, got {error:?}"),
        Ok(_) => panic!("expired managed token was restored"),
    }

    let non_admin = token('d');
    fixture
        .insert_token(
            &non_admin,
            "admin_api",
            &fixture.security_stamp,
            false,
            false,
        )
        .await;
    sqlx::query(r#"UPDATE "AspNetUsers" SET role = $2 WHERE id = $1"#)
        .bind(fixture.owner_id)
        .bind(Role::User as i16)
        .execute(&fixture.pool)
        .await
        .unwrap();
    assert_unauthorized(authenticate(&fixture.state(), &non_admin).await);

    sqlx::query(r#"DELETE FROM "AspNetUsers" WHERE id = $1"#)
        .bind(fixture.owner_id)
        .execute(&fixture.pool)
        .await
        .unwrap();
    assert_unauthorized(authenticate(&fixture.state(), &revoked).await);
    assert_unauthorized(authenticate(&fixture.state(), "rsctf_pat_v1_too-short").await);

    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn managed_token_listing_is_paginated_and_capped() {
    let fixture = ApiTokenFixture::new().await;
    sqlx::query(
        r#"INSERT INTO "ApiTokens"
             (id, name, token_hash, creator_id, created_at, is_revoked,
              audience, security_stamp_hash)
           SELECT md5(value::text)::uuid, 'bulk-' || value::text, md5(value::text), $1,
                  clock_timestamp() - (value * interval '1 second'), FALSE,
                  'admin_api', $2
             FROM generate_series(1, 130) value"#,
    )
    .bind(fixture.owner_id)
    .bind(sha256_str(&fixture.security_stamp))
    .execute(&fixture.pool)
    .await
    .unwrap();
    let page = list_tokens(
        State(fixture.state()),
        AdminUser(fixture.owner()),
        Query(PageParams {
            count: u64::MAX,
            skip: 0,
        }),
    )
    .await
    .unwrap();
    assert_eq!(page.total, 130);
    assert_eq!(page.data.len(), LIST_LIMIT as usize);

    fixture.cleanup().await;
}
