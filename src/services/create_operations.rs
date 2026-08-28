//! Small durable exact-replay ledger shared by resource-creation endpoints.

use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::utils::error::{AppError, AppResult};

const MAX_RETAINED_PER_KIND: i64 = 128;

/// Create endpoints must always carry an opaque identity. Accepting an
/// unidentified request makes a lost response indistinguishable from a new
/// resource intent, which defeats the ledger below.
pub(crate) fn require_operation_id(operation_id: Option<Uuid>) -> AppResult<Uuid> {
    operation_id
        .filter(|operation_id| !operation_id.is_nil())
        .ok_or_else(|| AppError::bad_request("operationId is required"))
}

pub(crate) async fn claim(
    transaction: &mut Transaction<'_, Postgres>,
    actor_id: Uuid,
    kind: &'static str,
    scope_id: i32,
    operation_id: Uuid,
    request_digest: &str,
) -> AppResult<Option<String>> {
    sqlx::query(
        r#"INSERT INTO "ResourceCreateOperations"
                  (actor_id, resource_kind, scope_id, operation_id, request_digest)
           VALUES ($1, $2, $3, $4, $5)
           ON CONFLICT (actor_id, resource_kind, scope_id, operation_id) DO NOTHING"#,
    )
    .bind(actor_id)
    .bind(kind)
    .bind(scope_id)
    .bind(operation_id)
    .bind(request_digest)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;

    let (stored_digest, result_id) = sqlx::query_as::<_, (String, Option<String>)>(
        r#"SELECT request_digest, result_id
             FROM "ResourceCreateOperations"
            WHERE actor_id = $1 AND resource_kind = $2
              AND scope_id = $3 AND operation_id = $4
            FOR UPDATE"#,
    )
    .bind(actor_id)
    .bind(kind)
    .bind(scope_id)
    .bind(operation_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(database_error)?;
    if stored_digest != request_digest {
        return Err(AppError::conflict(format!(
            "{kind} create operation was already used with different input"
        )));
    }
    Ok(result_id)
}

pub(crate) async fn complete(
    transaction: &mut Transaction<'_, Postgres>,
    actor_id: Uuid,
    kind: &'static str,
    scope_id: i32,
    operation_id: Uuid,
    result_id: &str,
) -> AppResult<()> {
    sqlx::query(
        r#"UPDATE "ResourceCreateOperations"
              SET result_id = $5, completed_at_utc = clock_timestamp()
            WHERE actor_id = $1 AND resource_kind = $2
              AND scope_id = $3 AND operation_id = $4
              AND result_id IS NULL"#,
    )
    .bind(actor_id)
    .bind(kind)
    .bind(scope_id)
    .bind(operation_id)
    .bind(result_id)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;

    sqlx::query(
        r#"DELETE FROM "ResourceCreateOperations" old
            WHERE old.created_at_utc < clock_timestamp() - INTERVAL '7 days'
               OR (old.actor_id = $1 AND old.resource_kind = $2
                    AND old.ctid IN (
                        SELECT ctid
                          FROM "ResourceCreateOperations"
                         WHERE actor_id = $1 AND resource_kind = $2
                         ORDER BY created_at_utc DESC, operation_id DESC
                        OFFSET $3
                    ))"#,
    )
    .bind(actor_id)
    .bind(kind)
    .bind(MAX_RETAINED_PER_KIND)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(())
}

fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::*;

    #[test]
    fn create_operation_identity_is_required_and_non_nil() {
        assert!(require_operation_id(None).is_err());
        assert!(require_operation_id(Some(Uuid::nil())).is_err());
        let operation_id = Uuid::new_v4();
        assert_eq!(
            require_operation_id(Some(operation_id)).unwrap(),
            operation_id
        );
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn exact_replay_conflict_rollback_and_retention_are_durable() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("create_operations_{}", Uuid::new_v4().simple());
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
        sqlx::raw_sql(crate::migrations::RESOURCE_CREATE_OPERATIONS_SQL)
            .execute(&pool)
            .await
            .unwrap();

        let actor = Uuid::new_v4();
        let operation = Uuid::new_v4();
        let mut transaction = pool.begin().await.unwrap();
        let digest_a = crate::utils::codec::sha256_str("a");
        let digest_b = crate::utils::codec::sha256_str("b");
        let digest_c = crate::utils::codec::sha256_str("c");
        assert_eq!(
            claim(&mut transaction, actor, "post", 0, operation, &digest_a)
                .await
                .unwrap(),
            None
        );
        complete(&mut transaction, actor, "post", 0, operation, "post-one")
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let mut transaction = pool.begin().await.unwrap();
        assert_eq!(
            claim(&mut transaction, actor, "post", 0, operation, &digest_a)
                .await
                .unwrap(),
            Some("post-one".to_string())
        );
        transaction.commit().await.unwrap();
        let mut transaction = pool.begin().await.unwrap();
        assert!(
            claim(&mut transaction, actor, "post", 0, operation, &digest_b)
                .await
                .is_err()
        );
        transaction.rollback().await.unwrap();

        let rolled_back = Uuid::new_v4();
        let mut transaction = pool.begin().await.unwrap();
        claim(&mut transaction, actor, "team", 0, rolled_back, &digest_c)
            .await
            .unwrap();
        transaction.rollback().await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                r#"SELECT COUNT(*)::BIGINT FROM "ResourceCreateOperations"
                    WHERE operation_id = $1"#,
            )
            .bind(rolled_back)
            .fetch_one(&pool)
            .await
            .unwrap(),
            0
        );

        for index in 0..130 {
            let operation = Uuid::new_v4();
            let mut transaction = pool.begin().await.unwrap();
            let digest = crate::utils::codec::sha256_str(&format!("digest-{index}"));
            claim(&mut transaction, actor, "game", 0, operation, &digest)
                .await
                .unwrap();
            complete(
                &mut transaction,
                actor,
                "game",
                0,
                operation,
                &index.to_string(),
            )
            .await
            .unwrap();
            transaction.commit().await.unwrap();
        }
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                r#"SELECT COUNT(*)::BIGINT FROM "ResourceCreateOperations"
                    WHERE actor_id = $1 AND resource_kind = 'game'"#,
            )
            .bind(actor)
            .fetch_one(&pool)
            .await
            .unwrap(),
            MAX_RETAINED_PER_KIND
        );

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
