use std::str::FromStr;
use std::sync::Arc;

use sea_orm::SqlxPostgresConnector;
use serde::{Deserialize, Serialize};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use uuid::Uuid;

use super::*;
use crate::app_state::{AppState, SharedState};
use crate::models::internal::configs::AppConfig;
use crate::services::cache::InMemoryCache;
use crate::services::container::NoopContainerManager;
use crate::services::token::TokenService;
use crate::storage::LocalBlobStorage;

const FIRST_BINDING: &[u8] = b"rotate-token";

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct RecoveredSecret {
    secret: String,
    revision: i64,
}

struct CredentialFixture {
    admin: sqlx::PgPool,
    pool: sqlx::PgPool,
    replica_pool: sqlx::PgPool,
    schema: String,
    actor: Uuid,
    other_actor: Uuid,
}

impl CredentialFixture {
    async fn new() -> Self {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("connect credential-operation test database");
        let schema = format!("credential_operations_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .expect("create credential-operation test schema");
        let options = PgConnectOptions::from_str(&database_url)
            .expect("parse credential-operation test database URL")
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(6)
            .connect_with(options.clone())
            .await
            .expect("connect scoped credential-operation database");
        let replica_pool = PgPoolOptions::new()
            .max_connections(6)
            .connect_with(options)
            .await
            .expect("connect independent replica credential-operation database");
        sqlx::raw_sql(
            r#"
            CREATE TABLE "Participations" (
                game_id INTEGER NOT NULL,
                id INTEGER PRIMARY KEY,
                UNIQUE (game_id, id)
            );
            CREATE TABLE "AspNetUsers" (id UUID PRIMARY KEY);
            CREATE TABLE "AdTeamApiTokens" (
                participation_id INTEGER PRIMARY KEY,
                token_hash BYTEA NOT NULL
            );
            CREATE TABLE "AdSshKeys" (
                participation_id INTEGER PRIMARY KEY,
                fingerprint TEXT NOT NULL
            );
            CREATE TABLE "KothApiTeamTokens" (
                game_id INTEGER NOT NULL,
                challenge_id INTEGER NOT NULL,
                participation_id INTEGER NOT NULL,
                generation INTEGER NOT NULL,
                PRIMARY KEY (game_id, challenge_id, participation_id)
            );
            CREATE TABLE "PlayerCredentialRevisions" (
                participation_id INTEGER NOT NULL REFERENCES "Participations"(id) ON DELETE CASCADE,
                credential_kind VARCHAR(16) NOT NULL,
                challenge_id INTEGER NOT NULL DEFAULT 0,
                revision BIGINT NOT NULL DEFAULT 0 CHECK (revision BETWEEN 0 AND 9007199254740991),
                updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                PRIMARY KEY (participation_id, credential_kind, challenge_id)
            );
            CREATE TABLE "PlayerCredentialOperations" (
                operation_id UUID PRIMARY KEY,
                participation_id INTEGER NOT NULL,
                game_id INTEGER NOT NULL,
                actor_user_id UUID NOT NULL REFERENCES "AspNetUsers"(id) ON DELETE CASCADE,
                credential_kind VARCHAR(16) NOT NULL,
                challenge_id INTEGER NOT NULL DEFAULT 0,
                expected_revision BIGINT NOT NULL,
                request_hash BYTEA NOT NULL CHECK (octet_length(request_hash) = 32),
                result_revision BIGINT,
                result_ciphertext BYTEA,
                result_nonce BYTEA,
                created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                completed_at TIMESTAMPTZ,
                expires_at TIMESTAMPTZ NOT NULL DEFAULT (clock_timestamp() + interval '15 minutes'),
                disclosure_count INTEGER NOT NULL DEFAULT 0,
                last_disclosed_at TIMESTAMPTZ,
                FOREIGN KEY (game_id, participation_id)
                    REFERENCES "Participations"(game_id, id) ON DELETE CASCADE,
                CHECK (expires_at > created_at),
                CHECK ((completed_at IS NULL AND result_revision IS NULL
                        AND result_ciphertext IS NULL AND result_nonce IS NULL
                        AND disclosure_count = 0 AND last_disclosed_at IS NULL)
                    OR (completed_at IS NOT NULL
                        AND result_revision = expected_revision + 1
                        AND octet_length(result_ciphertext) BETWEEN 17 AND 65552
                        AND octet_length(result_nonce) = 12
                        AND disclosure_count >= 1 AND last_disclosed_at IS NOT NULL))
            );
            CREATE INDEX ix_player_credential_operations_expiry
                ON "PlayerCredentialOperations"(expires_at);
            "#,
        )
        .execute(&pool)
        .await
        .expect("create credential-operation fixture tables");
        let actor = Uuid::new_v4();
        let other_actor = Uuid::new_v4();
        sqlx::query(r#"INSERT INTO "AspNetUsers" (id) VALUES ($1), ($2)"#)
            .bind(actor)
            .bind(other_actor)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "Participations" (game_id, id) VALUES (7, 1), (7, 2)"#)
            .execute(&pool)
            .await
            .unwrap();
        Self {
            admin,
            pool,
            replica_pool,
            schema,
            actor,
            other_actor,
        }
    }

    fn state_for(&self, pool: &sqlx::PgPool) -> SharedState {
        let mut config = AppConfig::default();
        config.jwt_secret = "credential-test-key-0123456789abcdef".to_string();
        AppState::new(
            SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone()),
            Arc::new(config),
            Arc::new(InMemoryCache::new()),
            Arc::new(LocalBlobStorage::new(std::env::temp_dir().join(format!(
                "rsctf-credential-operation-test-{}",
                self.schema
            )))),
            TokenService::new("0123456789abcdef0123456789abcdef", 60),
            Arc::new(NoopContainerManager),
        )
    }

    async fn cleanup(self) {
        self.pool.close().await;
        self.replica_pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{}" CASCADE"#, self.schema))
            .execute(&self.admin)
            .await
            .expect("drop credential-operation test schema");
    }
}

fn scope(participation_id: i32, actor_user_id: Uuid) -> CredentialScope {
    CredentialScope {
        participation_id,
        game_id: 7,
        challenge_id: 0,
        actor_user_id,
        kind: CredentialKind::AdToken,
    }
}

fn reservation_error<T>(
    result: AppResult<CredentialReservation<T>>,
    success_message: &str,
) -> AppError {
    match result {
        Err(error) => error,
        Ok(_) => panic!("{success_message}"),
    }
}

/// Run explicitly with `RSCTF_TEST_DATABASE_URL=postgres://... cargo test
/// player_credential_operations_are_retryable_ordered_and_bound -- --ignored`.
#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn player_credential_operations_are_retryable_ordered_and_bound() {
    let fixture = CredentialFixture::new().await;
    let first_state = fixture.state_for(&fixture.pool);
    let second_state = fixture.state_for(&fixture.replica_pool);
    let credential_scope = scope(1, fixture.actor);
    let operation_id = Uuid::new_v4();
    let request = CredentialMutationRequest {
        operation_id,
        expected_revision: 0,
    };
    let secret = RecoveredSecret {
        secret: "one-usable-secret".to_string(),
        revision: 1,
    };

    let mut first = fixture.pool.begin().await.unwrap();
    let CredentialReservation::Fresh(operation) = reserve::<RecoveredSecret>(
        &first_state,
        &mut first,
        credential_scope,
        request,
        FIRST_BINDING,
    )
    .await
    .expect("reserve first operation") else {
        panic!("first request unexpectedly recovered a result")
    };
    complete(
        &first_state,
        &mut first,
        credential_scope,
        operation,
        &secret,
    )
    .await
    .expect("complete first operation");
    let ciphertext: Vec<u8> = sqlx::query_scalar(
        r#"SELECT result_ciphertext FROM "PlayerCredentialOperations"
            WHERE operation_id = $1"#,
    )
    .bind(operation_id)
    .fetch_one(&mut *first)
    .await
    .unwrap();
    assert!(!ciphertext
        .windows(secret.secret.len())
        .any(|window| window == secret.secret.as_bytes()));

    let retry_state = second_state.clone();
    let retry_pool = fixture.replica_pool.clone();
    let retry = tokio::spawn(async move {
        let mut transaction = retry_pool.begin().await.unwrap();
        let result = reserve::<RecoveredSecret>(
            &retry_state,
            &mut transaction,
            credential_scope,
            request,
            FIRST_BINDING,
        )
        .await;
        transaction.commit().await.unwrap();
        result
    });
    tokio::task::yield_now().await;
    first.commit().await.unwrap();
    let CredentialReservation::Recovered(recovered) = retry.await.unwrap().unwrap() else {
        panic!("exact retry did not recover its committed secret")
    };
    assert_eq!(recovered, secret);
    assert_eq!(
        current_revision(&fixture.pool, 1, 7, CredentialKind::AdToken, 0)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i32>(
            r#"SELECT disclosure_count FROM "PlayerCredentialOperations"
                WHERE operation_id = $1"#,
        )
        .bind(operation_id)
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        2
    );

    for (changed_scope, binding) in [
        (scope(1, fixture.other_actor), FIRST_BINDING),
        (scope(2, fixture.actor), FIRST_BINDING),
        (
            CredentialScope {
                kind: CredentialKind::AdSsh,
                ..credential_scope
            },
            FIRST_BINDING,
        ),
        (credential_scope, b"revoke-token".as_slice()),
    ] {
        let mut transaction = fixture.pool.begin().await.unwrap();
        let error = reservation_error(
            reserve::<RecoveredSecret>(
                &second_state,
                &mut transaction,
                changed_scope,
                request,
                binding,
            )
            .await,
            "operation identity was rebound to another actor or request",
        );
        assert_eq!(error.status(), axum::http::StatusCode::CONFLICT);
        transaction.rollback().await.unwrap();
    }

    let mut stale = fixture.pool.begin().await.unwrap();
    let stale_error = reservation_error(
        reserve::<RecoveredSecret>(
            &first_state,
            &mut stale,
            credential_scope,
            CredentialMutationRequest {
                operation_id: Uuid::new_v4(),
                expected_revision: 0,
            },
            FIRST_BINDING,
        )
        .await,
        "a competing stale operation minted another credential",
    );
    assert_eq!(stale_error.status(), axum::http::StatusCode::CONFLICT);
    stale.rollback().await.unwrap();

    let mut superseding = fixture.pool.begin().await.unwrap();
    let CredentialReservation::Fresh(newer_operation) = reserve::<RecoveredSecret>(
        &first_state,
        &mut superseding,
        credential_scope,
        CredentialMutationRequest {
            operation_id: Uuid::new_v4(),
            expected_revision: 1,
        },
        FIRST_BINDING,
    )
    .await
    .unwrap() else {
        panic!("newer operation unexpectedly recovered")
    };
    complete(
        &first_state,
        &mut superseding,
        credential_scope,
        newer_operation,
        &RecoveredSecret {
            secret: "newer-secret".to_string(),
            revision: 2,
        },
    )
    .await
    .unwrap();
    superseding.commit().await.unwrap();
    let mut superseded = fixture.pool.begin().await.unwrap();
    let superseded_error = reservation_error(
        reserve::<RecoveredSecret>(
            &second_state,
            &mut superseded,
            credential_scope,
            request,
            FIRST_BINDING,
        )
        .await,
        "a superseded operation disclosed an inactive credential",
    );
    assert_eq!(superseded_error.status(), axum::http::StatusCode::CONFLICT);
    superseded.rollback().await.unwrap();

    let expired_id = Uuid::new_v4();
    let expired_scope = scope(2, fixture.actor);
    let expired_request = CredentialMutationRequest {
        operation_id: expired_id,
        expected_revision: 0,
    };
    let mut expired = fixture.pool.begin().await.unwrap();
    let CredentialReservation::Fresh(expired_operation) = reserve::<RecoveredSecret>(
        &first_state,
        &mut expired,
        expired_scope,
        expired_request,
        FIRST_BINDING,
    )
    .await
    .unwrap() else {
        panic!("new expiry fixture unexpectedly recovered")
    };
    complete(
        &first_state,
        &mut expired,
        expired_scope,
        expired_operation,
        &secret,
    )
    .await
    .unwrap();
    expired.commit().await.unwrap();
    sqlx::query(
        r#"UPDATE "PlayerCredentialOperations"
              SET created_at = clock_timestamp() - interval '16 minutes',
                  expires_at = clock_timestamp() - interval '1 minute'
            WHERE operation_id = $1"#,
    )
    .bind(expired_id)
    .execute(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(
        crate::services::cron::purge_expired_player_credential_operations(&second_state, 1)
            .await
            .unwrap(),
        1,
        "scheduled maintenance physically removes the encrypted recovery row"
    );
    let mut retry_expired = fixture.pool.begin().await.unwrap();
    let error = reservation_error(
        reserve::<RecoveredSecret>(
            &second_state,
            &mut retry_expired,
            expired_scope,
            expired_request,
            FIRST_BINDING,
        )
        .await,
        "expired recovery disclosed plaintext",
    );
    assert_eq!(error.status(), axum::http::StatusCode::CONFLICT);
    retry_expired.commit().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            r#"SELECT COUNT(*)::BIGINT FROM "PlayerCredentialOperations"
                WHERE operation_id = $1"#,
        )
        .bind(expired_id)
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        0
    );

    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn fresh_revision_bootstrap_binds_every_credential_kind_correctly() {
    let fixture = CredentialFixture::new().await;
    let state = fixture.state_for(&fixture.pool);
    sqlx::query(
        r#"INSERT INTO "AdTeamApiTokens" (participation_id, token_hash)
           VALUES (1, decode(repeat('ab', 32), 'hex'))"#,
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "AdSshKeys" (participation_id, fingerprint)
           VALUES (1, 'SHA256:bootstrap')"#,
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "KothApiTeamTokens"
               (game_id, challenge_id, participation_id, generation)
           VALUES (7, 9, 1, 7)"#,
    )
    .execute(&fixture.pool)
    .await
    .unwrap();

    for (kind, challenge_id, expected_revision) in [
        (CredentialKind::AdToken, 0, 1),
        (CredentialKind::AdSsh, 0, 1),
        (CredentialKind::KothApi, 9, 7),
    ] {
        let credential_scope = CredentialScope {
            participation_id: 1,
            game_id: 7,
            challenge_id,
            actor_user_id: fixture.actor,
            kind,
        };
        let mut transaction = fixture.pool.begin().await.unwrap();
        let CredentialReservation::Fresh(operation) = reserve::<RecoveredSecret>(
            &state,
            &mut transaction,
            credential_scope,
            CredentialMutationRequest {
                operation_id: Uuid::new_v4(),
                expected_revision,
            },
            b"bootstrap",
        )
        .await
        .expect("fresh credential kind bootstrap must bind all SQL parameters") else {
            panic!("a new operation unexpectedly recovered a result")
        };
        assert_eq!(operation.expected_revision, expected_revision);
        assert_eq!(operation.result_revision, expected_revision + 1);
        assert_eq!(
            current_revision(&mut *transaction, 1, 7, kind, challenge_id)
                .await
                .unwrap(),
            expected_revision
        );
        transaction.rollback().await.unwrap();
    }

    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn competing_replica_operations_commit_only_one_revision() {
    let fixture = CredentialFixture::new().await;
    let first_state = fixture.state_for(&fixture.pool);
    let second_state = fixture.state_for(&fixture.replica_pool);
    let credential_scope = scope(1, fixture.actor);

    let mut first = fixture.pool.begin().await.unwrap();
    let CredentialReservation::Fresh(operation) = reserve::<RecoveredSecret>(
        &first_state,
        &mut first,
        credential_scope,
        CredentialMutationRequest {
            operation_id: Uuid::new_v4(),
            expected_revision: 0,
        },
        FIRST_BINDING,
    )
    .await
    .unwrap() else {
        panic!("first operation must be fresh")
    };
    complete(
        &first_state,
        &mut first,
        credential_scope,
        operation,
        &RecoveredSecret {
            secret: "winner".to_string(),
            revision: 1,
        },
    )
    .await
    .unwrap();

    let competing_pool = fixture.replica_pool.clone();
    let competing = tokio::spawn(async move {
        let mut transaction = competing_pool.begin().await.unwrap();
        let result = reserve::<RecoveredSecret>(
            &second_state,
            &mut transaction,
            credential_scope,
            CredentialMutationRequest {
                operation_id: Uuid::new_v4(),
                expected_revision: 0,
            },
            b"revoke-token",
        )
        .await;
        transaction.rollback().await.unwrap();
        result
    });
    tokio::task::yield_now().await;
    first.commit().await.unwrap();
    let error = reservation_error(
        competing.await.unwrap(),
        "the losing replica committed a second credential",
    );
    assert_eq!(error.status(), axum::http::StatusCode::CONFLICT);
    assert_eq!(
        current_revision(&fixture.pool, 1, 7, CredentialKind::AdToken, 0)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            r#"SELECT COUNT(*)::BIGINT FROM "PlayerCredentialOperations"
                WHERE participation_id = 1 AND completed_at IS NOT NULL"#,
        )
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        1
    );

    fixture.cleanup().await;
}
