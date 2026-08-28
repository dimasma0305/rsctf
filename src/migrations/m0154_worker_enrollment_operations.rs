use sea_orm_migration::prelude::*;

pub const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "WorkerEnrollmentOperations" (
    operation_id UUID PRIMARY KEY,
    worker_id UUID NOT NULL REFERENCES "WorkerNodes"(id) ON DELETE CASCADE,
    token_hash BYTEA NOT NULL CHECK (octet_length(token_hash) = 32),
    csr_digest BYTEA NOT NULL CHECK (octet_length(csr_digest) = 32),
    state TEXT NOT NULL CHECK (state IN ('Signing', 'Completed', 'Retryable', 'Failed')),
    claim_expires_at TIMESTAMPTZ NOT NULL,
    response JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    completed_at TIMESTAMPTZ,
    UNIQUE (token_hash),
    CHECK ((state = 'Completed') = (response IS NOT NULL))
);
CREATE INDEX IF NOT EXISTS ix_worker_enrollment_operations_expiry
    ON "WorkerEnrollmentOperations" (completed_at, operation_id)
    WHERE state = 'Completed';
"#;

const DOWN_SQL: &str = r#"DROP TABLE IF EXISTS "WorkerEnrollmentOperations";"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(DOWN_SQL)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn enrollment_claims_bind_token_operation_and_csr() {
        assert!(UP_SQL.contains("operation_id UUID PRIMARY KEY"));
        assert!(UP_SQL.contains("UNIQUE (token_hash)"));
        assert!(UP_SQL.contains("csr_digest BYTEA"));
        assert!(UP_SQL.contains("'Retryable'"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn concurrent_exact_claims_have_one_signing_owner() {
        use sqlx::postgres::PgPoolOptions;
        use uuid::Uuid;

        use crate::services::worker_store::{EnrollmentClaim, WorkerStore};

        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL").unwrap();
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("worker_enrollment_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let scoped = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .after_connect(move |connection, _| {
                let statement = format!(r#"SET search_path TO "{scoped}""#);
                Box::pin(async move {
                    sqlx::query(&statement).execute(connection).await?;
                    Ok(())
                })
            })
            .connect(&database_url)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"CREATE TABLE "WorkerNodes" (
                   id uuid PRIMARY KEY,
                   enrollment_token_hash bytea,
                   enrollment_token_used_at timestamptz,
                   enrollment_token_expires_at timestamptz
               );"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        let worker_id = Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO "WorkerNodes" VALUES
                ($1, $2, NULL, clock_timestamp() + interval '5 minutes')"#,
        )
        .bind(worker_id)
        .bind([1_u8; 32].as_slice())
        .execute(&pool)
        .await
        .unwrap();
        let operation_id = Uuid::new_v4();
        let first = WorkerStore::new(pool.clone());
        let second = first.clone();
        let (first, second) = tokio::join!(
            first.claim_enrollment(operation_id, [1; 32], [2; 32]),
            second.claim_enrollment(operation_id, [1; 32], [2; 32])
        );
        let claims = [first.unwrap(), second.unwrap()];
        assert_eq!(
            claims
                .iter()
                .filter(|claim| matches!(claim, EnrollmentClaim::Claimed { .. }))
                .count(),
            1
        );
        assert_eq!(
            claims
                .iter()
                .filter(|claim| matches!(claim, EnrollmentClaim::InProgress))
                .count(),
            1
        );
        assert!(matches!(
            WorkerStore::new(pool.clone())
                .claim_enrollment(operation_id, [1; 32], [3; 32])
                .await
                .unwrap(),
            EnrollmentClaim::Invalid
        ));

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
