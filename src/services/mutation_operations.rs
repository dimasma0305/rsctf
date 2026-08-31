//! Bounded, actor-scoped replay ledger for short HTTP mutations.
//!
//! Callers claim and complete an operation inside the same transaction as the
//! resource write. A committed resource therefore always has a recoverable
//! identity, while a rolled-back insert never strands a pending ledger row.

use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::utils::error::{AppError, AppResult};

const MAX_ACTIVE_OPERATIONS_PER_ACTOR_KIND: i64 = 128;
const CLEANUP_BATCH: i64 = 64;
const MAX_KIND_BYTES: usize = 48;
const MAX_SCOPE_BYTES: usize = 160;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletedOperation {
    pub result_id: String,
    pub result_revision: Option<i64>,
}

pub fn fingerprint<T: Serialize>(domain: &str, value: &T) -> AppResult<[u8; 32]> {
    let encoded = serde_json::to_vec(value)
        .map_err(|error| AppError::internal(format!("mutation fingerprint failed: {error}")))?;
    let mut digest = Sha256::new();
    digest.update(b"rsctf:mutation-operation:v1\0");
    digest.update((domain.len() as u64).to_be_bytes());
    digest.update(domain.as_bytes());
    digest.update((encoded.len() as u64).to_be_bytes());
    digest.update(encoded);
    Ok(digest.finalize().into())
}

/// Read-only fast path used before an expensive mutation fence. A concurrent
/// first attempt is still serialized by `claim`; this only recovers operations
/// whose resource write and result already committed.
pub async fn find_completed(
    pool: &sqlx::PgPool,
    actor_id: Uuid,
    kind: &str,
    scope: &str,
    operation_id: Uuid,
    request_fingerprint: [u8; 32],
) -> AppResult<Option<CompletedOperation>> {
    validate_identity(kind, scope, operation_id)?;
    let existing: Option<(Vec<u8>, Option<String>, Option<i64>)> = sqlx::query_as(
        r#"SELECT request_fingerprint, result_id, result_revision
             FROM "MutationOperations"
            WHERE actor_id = $1 AND resource_kind = $2 AND scope_key = $3
              AND operation_id = $4"#,
    )
    .bind(actor_id)
    .bind(kind)
    .bind(scope)
    .bind(operation_id)
    .fetch_optional(pool)
    .await
    .map_err(database_error)?;
    let Some((stored_fingerprint, result_id, result_revision)) = existing else {
        return Ok(None);
    };
    if stored_fingerprint.as_slice() != request_fingerprint.as_slice() {
        return Err(AppError::conflict(
            "operationId was already used for different content",
        ));
    }
    let result_id = result_id
        .ok_or_else(|| AppError::conflict("the matching mutation operation is still pending"))?;
    Ok(Some(CompletedOperation {
        result_id,
        result_revision,
    }))
}

fn validate_identity(kind: &str, scope: &str, operation_id: Uuid) -> AppResult<()> {
    if operation_id.is_nil() {
        return Err(AppError::bad_request("operationId must be an opaque UUID"));
    }
    if kind.is_empty() || kind.len() > MAX_KIND_BYTES || !kind.is_ascii() {
        return Err(AppError::internal("invalid mutation resource kind"));
    }
    if scope.len() > MAX_SCOPE_BYTES || !scope.is_ascii() {
        return Err(AppError::internal("invalid mutation scope"));
    }
    Ok(())
}

pub async fn claim(
    transaction: &mut Transaction<'_, Postgres>,
    actor_id: Uuid,
    kind: &str,
    scope: &str,
    operation_id: Uuid,
    request_fingerprint: [u8; 32],
) -> AppResult<Option<CompletedOperation>> {
    validate_identity(kind, scope, operation_id)?;

    // Different keys for one actor/kind are serialized only for the tiny
    // retention/count section. The resource's own lock remains authoritative.
    sqlx::query(
        r#"SELECT pg_advisory_xact_lock(
               hashtextextended($1::text || ':' || $2::text, 713421)
           )"#,
    )
    .bind(actor_id)
    .bind(kind)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;

    let existing: Option<(Vec<u8>, Option<String>, Option<i64>)> = sqlx::query_as(
        r#"SELECT request_fingerprint, result_id, result_revision
             FROM "MutationOperations"
            WHERE actor_id = $1 AND resource_kind = $2 AND scope_key = $3
              AND operation_id = $4
            FOR UPDATE"#,
    )
    .bind(actor_id)
    .bind(kind)
    .bind(scope)
    .bind(operation_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?;
    if let Some((stored_fingerprint, result_id, result_revision)) = existing {
        if stored_fingerprint.as_slice() != request_fingerprint.as_slice() {
            return Err(AppError::conflict(
                "operationId was already used for different content",
            ));
        }
        let result_id = result_id.ok_or_else(|| {
            AppError::conflict("the matching mutation operation is still pending")
        })?;
        return Ok(Some(CompletedOperation {
            result_id,
            result_revision,
        }));
    }

    sqlx::query(
        r#"DELETE FROM "MutationOperations" WHERE ctid IN (
               SELECT ctid FROM "MutationOperations"
                WHERE actor_id = $1 AND resource_kind = $2
                  AND expires_at_utc <= clock_timestamp()
                ORDER BY expires_at_utc
                LIMIT $3
           )"#,
    )
    .bind(actor_id)
    .bind(kind)
    .bind(CLEANUP_BATCH)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    let active: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)::BIGINT FROM "MutationOperations"
            WHERE actor_id = $1 AND resource_kind = $2
              AND expires_at_utc > clock_timestamp()"#,
    )
    .bind(actor_id)
    .bind(kind)
    .fetch_one(&mut **transaction)
    .await
    .map_err(database_error)?;
    if active >= MAX_ACTIVE_OPERATIONS_PER_ACTOR_KIND {
        return Err(AppError::conflict(
            "too many retained mutation operations; retry after older operations expire",
        ));
    }

    sqlx::query(
        r#"INSERT INTO "MutationOperations"
             (actor_id, resource_kind, scope_key, operation_id, request_fingerprint)
           VALUES ($1, $2, $3, $4, $5)"#,
    )
    .bind(actor_id)
    .bind(kind)
    .bind(scope)
    .bind(operation_id)
    .bind(request_fingerprint.as_slice())
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(None)
}

pub async fn complete(
    transaction: &mut Transaction<'_, Postgres>,
    actor_id: Uuid,
    kind: &str,
    scope: &str,
    operation_id: Uuid,
    result_id: &str,
    result_revision: Option<i64>,
) -> AppResult<()> {
    if result_id.is_empty() || result_id.len() > 256 {
        return Err(AppError::internal(
            "invalid mutation operation result identity",
        ));
    }
    let updated = sqlx::query(
        r#"UPDATE "MutationOperations"
              SET result_id = $5, result_revision = $6,
                  completed_at_utc = clock_timestamp()
            WHERE actor_id = $1 AND resource_kind = $2 AND scope_key = $3
              AND operation_id = $4 AND result_id IS NULL"#,
    )
    .bind(actor_id)
    .bind(kind)
    .bind(scope)
    .bind(operation_id)
    .bind(result_id)
    .bind(result_revision)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    if updated.rows_affected() != 1 {
        return Err(AppError::conflict(
            "mutation operation completion was superseded",
        ));
    }
    Ok(())
}

fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use std::str::FromStr;

    #[test]
    fn fingerprints_bind_the_domain_and_canonical_struct() {
        #[derive(Serialize)]
        struct Input<'a> {
            title: &'a str,
            enabled: bool,
        }
        let value = Input {
            title: "one",
            enabled: true,
        };
        let first = fingerprint("team-create", &value).unwrap();
        assert_eq!(first, fingerprint("team-create", &value).unwrap());
        assert_ne!(first, fingerprint("game-create", &value).unwrap());
        assert_ne!(
            first,
            fingerprint(
                "team-create",
                &Input {
                    title: "two",
                    enabled: true,
                },
            )
            .unwrap()
        );
    }

    #[test]
    fn operation_identity_rejects_nil_and_unbounded_scopes() {
        assert!(validate_identity("team", "", Uuid::nil()).is_err());
        assert!(
            validate_identity("team", &"x".repeat(MAX_SCOPE_BYTES + 1), Uuid::new_v4()).is_err()
        );
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn resource_result_commit_and_exact_replay_are_one_atomic_boundary() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("mutation_operations_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"CREATE TABLE "AspNetUsers" (id UUID PRIMARY KEY);
               CREATE TABLE "MutationOperations" (
                 actor_id UUID NOT NULL REFERENCES "AspNetUsers"(id) ON DELETE CASCADE,
                 resource_kind TEXT NOT NULL,
                 scope_key TEXT NOT NULL,
                 operation_id UUID NOT NULL,
                 request_fingerprint BYTEA NOT NULL,
                 result_id TEXT,
                 result_revision BIGINT,
                 created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                 completed_at_utc TIMESTAMPTZ,
                 expires_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp() + interval '7 days',
                 PRIMARY KEY (actor_id, resource_kind, scope_key, operation_id)
               );
               CREATE TABLE resources (id SERIAL PRIMARY KEY, title TEXT NOT NULL);"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let actor_id = Uuid::new_v4();
        sqlx::query(r#"INSERT INTO "AspNetUsers" (id) VALUES ($1)"#)
            .bind(actor_id)
            .execute(&pool)
            .await
            .unwrap();
        let operation_id = Uuid::new_v4();
        let request_fingerprint = fingerprint("test-create", &("one", 1_i32)).unwrap();

        let mut rolled_back = pool.begin().await.unwrap();
        assert!(claim(
            &mut rolled_back,
            actor_id,
            "test-create",
            "global",
            Uuid::new_v4(),
            fingerprint("test-create", &("rollback", 1_i32)).unwrap(),
        )
        .await
        .unwrap()
        .is_none());
        sqlx::query("INSERT INTO resources (title) VALUES ('rollback')")
            .execute(&mut *rolled_back)
            .await
            .unwrap();
        rolled_back.rollback().await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM resources")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );

        let mut transaction = pool.begin().await.unwrap();
        assert!(claim(
            &mut transaction,
            actor_id,
            "test-create",
            "global",
            operation_id,
            request_fingerprint,
        )
        .await
        .unwrap()
        .is_none());
        let resource_id: i32 =
            sqlx::query_scalar("INSERT INTO resources (title) VALUES ('one') RETURNING id")
                .fetch_one(&mut *transaction)
                .await
                .unwrap();
        complete(
            &mut transaction,
            actor_id,
            "test-create",
            "global",
            operation_id,
            &resource_id.to_string(),
            Some(1),
        )
        .await
        .unwrap();
        transaction.commit().await.unwrap();

        let mut replay_transaction = pool.begin().await.unwrap();
        let replay = claim(
            &mut replay_transaction,
            actor_id,
            "test-create",
            "global",
            operation_id,
            request_fingerprint,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(replay.result_id, resource_id.to_string());
        assert_eq!(replay.result_revision, Some(1));
        replay_transaction.commit().await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM resources")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );

        let mut conflicting = pool.begin().await.unwrap();
        let error = claim(
            &mut conflicting,
            actor_id,
            "test-create",
            "global",
            operation_id,
            fingerprint("test-create", &("different", 1_i32)).unwrap(),
        )
        .await
        .expect_err("same operation id accepted different content");
        assert_eq!(error.status(), axum::http::StatusCode::CONFLICT);

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
