use std::str::FromStr;
use std::sync::Arc;

use sea_orm::SqlxPostgresConnector;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use uuid::Uuid;

use super::authorize_request;
use crate::app_state::AppState;
use crate::models::data::participation;
use crate::models::internal::configs::AppConfig;
use crate::services::ad::api_token::VerifiedTeamToken;
use crate::services::cache::InMemoryCache;
use crate::services::container::NoopContainerManager;
use crate::services::token::TokenService;
use crate::storage::LocalBlobStorage;
use crate::utils::enums::ParticipationStatus;
use crate::utils::error::AppError;

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn automation_token_bypasses_browser_proof_only_for_its_event() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("event_vpn_automation_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"
        CREATE TABLE "Games" (
            id INTEGER PRIMARY KEY,
            vpn_access_required BOOLEAN NOT NULL,
            vpn_behavior_telemetry_enabled BOOLEAN NOT NULL,
            vpn_flag_scan_enabled BOOLEAN NOT NULL,
            vpn_provider_dns_telemetry_enabled BOOLEAN NOT NULL,
            vpn_source_asn_telemetry_enabled BOOLEAN NOT NULL,
            vpn_device_sharing_telemetry_enabled BOOLEAN NOT NULL,
            vpn_policy_revision BIGINT NOT NULL,
            start_time_utc TIMESTAMPTZ NOT NULL,
            end_time_utc TIMESTAMPTZ NOT NULL,
            deletion_pending BOOLEAN NOT NULL
        );
        CREATE TABLE "Teams" (
            id INTEGER PRIMARY KEY,
            captain_id UUID NOT NULL,
            deletion_pending BOOLEAN NOT NULL
        );
        CREATE TABLE "Participations" (
            id INTEGER PRIMARY KEY,
            game_id INTEGER NOT NULL,
            team_id INTEGER NOT NULL,
            status SMALLINT NOT NULL
        );
        CREATE TABLE "AspNetUsers" (
            id UUID PRIMARY KEY,
            email_confirmed BOOLEAN NOT NULL,
            role SMALLINT NOT NULL,
            security_stamp TEXT NOT NULL
        );
        CREATE TABLE "TeamMembers" (team_id INTEGER NOT NULL, user_id UUID NOT NULL);
        CREATE TABLE "UserParticipations" (
            user_id UUID NOT NULL,
            game_id INTEGER NOT NULL,
            team_id INTEGER NOT NULL,
            participation_id INTEGER NOT NULL
        );
        CREATE TABLE "EventVpnUserPeers" (
            id UUID PRIMARY KEY,
            game_id INTEGER NOT NULL,
            user_id UUID NOT NULL,
            participation_id INTEGER NOT NULL,
            public_key TEXT NOT NULL,
            address TEXT NOT NULL,
            generation INTEGER NOT NULL,
            revoked_at_utc TIMESTAMPTZ
        );
        CREATE TABLE "EventVpnGateOverrides" (
            game_id INTEGER NOT NULL,
            created_at_utc TIMESTAMPTZ NOT NULL,
            expires_at_utc TIMESTAMPTZ NOT NULL,
            revoked_at_utc TIMESTAMPTZ
        );
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let user_id = Uuid::new_v4();
    let peer_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "Games" VALUES (
               7, TRUE, FALSE, FALSE, FALSE, FALSE, FALSE, 3,
               clock_timestamp() - interval '1 hour',
               clock_timestamp() + interval '1 hour', FALSE
           )"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "Teams" VALUES (11, $1, FALSE)"#)
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "Participations" VALUES (29, 7, 11, 1)"#)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "AspNetUsers" VALUES ($1, TRUE, 1, 'stamp')"#)
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "UserParticipations" VALUES ($1, 7, 11, 29)"#)
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "EventVpnUserPeers" VALUES (
               $2, 7, $1, 29, 'public-key', '10.13.42.17', 1, NULL
           )"#,
    )
    .bind(user_id)
    .bind(peer_id)
    .execute(&pool)
    .await
    .unwrap();

    let database = SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone());
    let root = std::env::temp_dir().join(format!("rsctf-event-vpn-{}", Uuid::new_v4().simple()));
    let state = AppState::new(
        database,
        Arc::new(AppConfig::default()),
        Arc::new(InMemoryCache::new()),
        Arc::new(LocalBlobStorage::new(root)),
        TokenService::new("0123456789abcdef0123456789abcdef", 60),
        Arc::new(NoopContainerManager),
    );
    let token = VerifiedTeamToken {
        participation: participation::Model {
            id: 29,
            status: ParticipationStatus::Accepted,
            token: String::new(),
            writeup_id: None,
            game_id: 7,
            team_id: 11,
            division_id: None,
            suspicion_score: 0,
            competitive_admitted_at_utc: None,
        },
        partition_key: "ad:test".to_string(),
    };
    let headers = axum::http::HeaderMap::new();
    assert!(
        authorize_request(&state, &headers, Some(token.clone()), false, 7,)
            .await
            .unwrap()
            .is_none()
    );
    let mut wrong_event_token = token;
    wrong_event_token.participation.game_id = 8;
    assert!(matches!(
        authorize_request(&state, &headers, Some(wrong_event_token), false, 7,).await,
        Err(AppError::Unauthorized)
    ));

    drop(state);
    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
}
