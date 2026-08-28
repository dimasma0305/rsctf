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

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct RecoveredSecret {
    secret: String,
    revision: i64,
}

struct CredentialFixture {
    admin: sqlx::PgPool,
    pool: sqlx::PgPool,
    schema: String,
    actor: Uuid,
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
            .connect_with(options)
            .await
            .expect("connect scoped credential-operation database");
        sqlx::raw_sql(
            r#"
            CREATE TABLE "Participations" (id INTEGER PRIMARY KEY);
            CREATE TABLE "AspNetUsers" (id UUID PRIMARY KEY);
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
                participation_id INTEGER NOT NULL REFERENCES "Participations"(id) ON DELETE CASCADE,
                game_id INTEGER NOT NULL,
                actor_user_id UUID NOT NULL REFERENCES "AspNetUsers"(id) ON DELETE CASCADE,
                credential_kind VARCHAR(16) NOT NULL,
                challenge_id INTEGER NOT NULL DEFAULT 0,
                expected_revision BIGINT NOT NULL,
                result_revision BIGINT,
                result_ciphertext BYTEA,
                result_nonce BYTEA,
                created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                completed_at TIMESTAMPTZ,
                expires_at TIMESTAMPTZ NOT NULL DEFAULT (clock_timestamp() + interval '15 minutes'),
                disclosure_count INTEGER NOT NULL DEFAULT 0,
                last_disclosed_at TIMESTAMPTZ,
                CHECK (expires_at > created_at),
                CHECK ((completed_at IS NULL AND result_revision IS NULL
                        AND result_ciphertext IS NULL AND result_nonce IS NULL
                        AND disclosure_count = 0 AND last_disclosed_at IS NULL)
                    OR (completed_at IS NOT NULL
                        AND result_revision = expected_revision + 1
                        AND octet_length(result_ciphertext) BETWEEN 17 AND 262144
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
        sqlx::query(r#"INSERT INTO "AspNetUsers" (id) VALUES ($1), ($2)"#)
            .bind(actor)
            .bind(Uuid::new_v4())
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "Participations" (id) VALUES (1), (2)"#)
            .execute(&pool)
            .await
            .unwrap();
        Self {
            admin,
            pool,
            schema,
            actor,
        }
    }

    fn state(&self) -> SharedState {
        AppState::new(
            SqlxPostgresConnector::from_sqlx_postgres_pool(self.pool.clone()),
            Arc::new(AppConfig::default()),
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
/// player_credential_operations_are_retryable_ordered_and_replica_safe --
/// --ignored --nocapture`.
#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn player_credential_operations_are_retryable_ordered_and_replica_safe() {
    let fixture = CredentialFixture::new().await;
    let first_state = fixture.state();
    let second_state = fixture.state();
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
    let CredentialReservation::Fresh {
        operation_id: reserved,
        expected_revision,
        result_revision,
    } = reserve::<RecoveredSecret>(&first_state, &mut first, credential_scope, request)
        .await
        .expect("reserve first operation")
    else {
        panic!("first request unexpectedly recovered a result")
    };
    complete(
        &first_state,
        &mut first,
        credential_scope,
        reserved,
        expected_revision,
        result_revision,
        &secret,
    )
    .await
    .expect("complete first operation");

    // A request through another state/pool owner emulates a second replica.
    // It must wait for the committing revision and then recover the same bytes.
    let retry_state = second_state.clone();
    let retry_pool = fixture.pool.clone();
    let retry = tokio::spawn(async move {
        let mut transaction = retry_pool.begin().await.unwrap();
        let result =
            reserve::<RecoveredSecret>(&retry_state, &mut transaction, credential_scope, request)
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
        current_revision(&fixture.pool, 1, CredentialKind::AdToken, 0)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i32>(
            r#"SELECT disclosure_count FROM "PlayerCredentialOperations" WHERE operation_id = $1"#,
        )
        .bind(operation_id)
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        2
    );

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
        )
        .await,
        "a competing stale operation minted another credential",
    );
    assert_eq!(stale_error.status(), axum::http::StatusCode::CONFLICT);
    stale.rollback().await.unwrap();

    let other_actor: Uuid =
        sqlx::query_scalar(r#"SELECT id FROM "AspNetUsers" WHERE id <> $1 LIMIT 1"#)
            .bind(fixture.actor)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    let mut wrong_scope = fixture.pool.begin().await.unwrap();
    let scope_error = reservation_error(
        reserve::<RecoveredSecret>(
            &second_state,
            &mut wrong_scope,
            scope(1, other_actor),
            request,
        )
        .await,
        "another actor recovered the plaintext",
    );
    assert_eq!(scope_error.status(), axum::http::StatusCode::CONFLICT);
    wrong_scope.rollback().await.unwrap();

    // Revoke/upload operations advance the same revision. An old recovery is
    // then deliberately unusable, even while its encrypted record still exists.
    let mut revoke = fixture.pool.begin().await.unwrap();
    assert_eq!(
        advance_revision(&mut revoke, 1, CredentialKind::AdToken, 0)
            .await
            .unwrap(),
        2
    );
    revoke.commit().await.unwrap();
    let mut superseded = fixture.pool.begin().await.unwrap();
    let superseded_error = reservation_error(
        reserve::<RecoveredSecret>(&first_state, &mut superseded, credential_scope, request).await,
        "a revoked/superseded secret was disclosed again",
    );
    assert_eq!(superseded_error.status(), axum::http::StatusCode::CONFLICT);
    superseded.rollback().await.unwrap();

    sqlx::query(
        r#"UPDATE "PlayerCredentialOperations"
              SET created_at = clock_timestamp() - interval '16 minutes',
                  expires_at = clock_timestamp() - interval '1 minute'
            WHERE operation_id = $1"#,
    )
    .bind(operation_id)
    .execute(&fixture.pool)
    .await
    .unwrap();
    let mut expired = fixture.pool.begin().await.unwrap();
    let expired_error = reservation_error(
        reserve::<RecoveredSecret>(&second_state, &mut expired, credential_scope, request).await,
        "expired recovery disclosed plaintext",
    );
    assert_eq!(expired_error.status(), axum::http::StatusCode::CONFLICT);
    expired.commit().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            r#"SELECT COUNT(*)::BIGINT FROM "PlayerCredentialOperations" WHERE operation_id = $1"#,
        )
        .bind(operation_id)
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        0
    );

    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn competing_replica_operations_commit_only_one_revision() {
    let fixture = CredentialFixture::new().await;
    let first_state = fixture.state();
    let second_state = fixture.state();
    let credential_scope = scope(2, fixture.actor);

    let mut first = fixture.pool.begin().await.unwrap();
    let first_request = CredentialMutationRequest {
        operation_id: Uuid::new_v4(),
        expected_revision: 0,
    };
    let CredentialReservation::Fresh {
        operation_id,
        expected_revision,
        result_revision,
    } = reserve::<RecoveredSecret>(&first_state, &mut first, credential_scope, first_request)
        .await
        .unwrap()
    else {
        panic!("first operation must be fresh")
    };
    complete(
        &first_state,
        &mut first,
        credential_scope,
        operation_id,
        expected_revision,
        result_revision,
        &RecoveredSecret {
            secret: "winner".to_string(),
            revision: 1,
        },
    )
    .await
    .unwrap();

    let competing_pool = fixture.pool.clone();
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
        current_revision(&fixture.pool, 2, CredentialKind::AdToken, 0)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            r#"SELECT COUNT(*)::BIGINT FROM "PlayerCredentialOperations"
                WHERE participation_id = 2 AND completed_at IS NOT NULL"#,
        )
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        1
    );

    fixture.cleanup().await;
}
