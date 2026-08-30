//! Bounded retention for durable administrator mutation identities.

use crate::utils::error::{AppError, AppResult};

const PURGE_STATEMENTS: [&str; 6] = [
    r#"DELETE FROM "BulkChallengeMutationOperations" WHERE ctid IN (
          SELECT ctid FROM "BulkChallengeMutationOperations"
           WHERE state = 2
             AND completed_at_utc < clock_timestamp() - INTERVAL '30 days'
           ORDER BY completed_at_utc, game_id, operation_id
           LIMIT $1 FOR UPDATE SKIP LOCKED)"#,
    r#"DELETE FROM "BulkChallengeMutationOperations" WHERE ctid IN (
          SELECT ctid FROM "BulkChallengeMutationOperations"
           WHERE state IN (0, 1)
             AND lease_expires_at_utc <= clock_timestamp()
             AND created_at_utc < clock_timestamp() - INTERVAL '7 days'
           ORDER BY created_at_utc, game_id, operation_id
           LIMIT $1 FOR UPDATE SKIP LOCKED)"#,
    r#"DELETE FROM "FlagImportOperations" WHERE ctid IN (
          SELECT ctid FROM "FlagImportOperations"
           WHERE state = 1
             AND completed_at_utc < clock_timestamp() - INTERVAL '30 days'
           ORDER BY completed_at_utc, challenge_id, operation_id
           LIMIT $1 FOR UPDATE SKIP LOCKED)"#,
    r#"DELETE FROM "FlagImportOperations" WHERE ctid IN (
          SELECT ctid FROM "FlagImportOperations"
           WHERE state = 0
             AND lease_expires_at_utc <= clock_timestamp()
             AND created_at_utc < clock_timestamp() - INTERVAL '7 days'
           ORDER BY created_at_utc, challenge_id, operation_id
           LIMIT $1 FOR UPDATE SKIP LOCKED)"#,
    r#"DELETE FROM "TeamInviteOperations" WHERE ctid IN (
          SELECT ctid FROM "TeamInviteOperations"
           WHERE reconciled_at_utc IS NOT NULL
             AND created_at_utc < clock_timestamp() - INTERVAL '30 days'
           ORDER BY created_at_utc, team_id, operation_id
           LIMIT $1 FOR UPDATE SKIP LOCKED)"#,
    r#"DELETE FROM "TeamInviteOperations" WHERE ctid IN (
          SELECT ctid FROM "TeamInviteOperations"
           WHERE reconciled_at_utc IS NULL
             AND created_at_utc < clock_timestamp() - INTERVAL '30 days'
           ORDER BY created_at_utc, team_id, operation_id
           LIMIT $1 FOR UPDATE SKIP LOCKED)"#,
];

/// Purge a bounded page from each operation family. Running leases are never
/// removed until they are both expired and old enough that every documented
/// handler/runtime deadline has elapsed.
pub async fn purge_expired(pool: &sqlx::PgPool, limit_per_family: i64) -> AppResult<u64> {
    let limit = limit_per_family.clamp(1, 512);
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let mut removed = 0_u64;
    for statement in PURGE_STATEMENTS {
        removed = removed.saturating_add(
            sqlx::query(statement)
                .bind(limit)
                .execute(&mut *transaction)
                .await
                .map_err(|error| AppError::internal(error.to_string()))?
                .rows_affected(),
        );
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use std::str::FromStr;

    #[test]
    fn every_operation_family_has_terminal_and_abandoned_retention() {
        let sql = PURGE_STATEMENTS.join("\n");
        assert!(sql.contains("BulkChallengeMutationOperations"));
        assert!(sql.contains("FlagImportOperations"));
        assert!(sql.contains("TeamInviteOperations"));
        assert!(sql.contains("lease_expires_at_utc <= clock_timestamp()"));
        assert!(sql.contains("reconciled_at_utc IS NULL"));
        assert!(PURGE_STATEMENTS
            .iter()
            .all(|statement| statement.contains("LIMIT $1 FOR UPDATE SKIP LOCKED")));
    }

    #[tokio::test]
    #[ignore = "requires disposable PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn terminal_and_abandoned_rows_are_purged_without_touching_recent_work() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("admin_mutation_retention_{}", uuid::Uuid::new_v4().simple());
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
            CREATE TABLE "BulkChallengeMutationOperations" (
                game_id INTEGER NOT NULL,
                operation_id UUID NOT NULL,
                state SMALLINT NOT NULL,
                lease_expires_at_utc TIMESTAMPTZ NOT NULL,
                completed_at_utc TIMESTAMPTZ NULL,
                created_at_utc TIMESTAMPTZ NOT NULL
            );
            CREATE TABLE "FlagImportOperations" (
                challenge_id INTEGER NOT NULL,
                operation_id UUID NOT NULL,
                state SMALLINT NOT NULL,
                lease_expires_at_utc TIMESTAMPTZ NOT NULL,
                completed_at_utc TIMESTAMPTZ NULL,
                created_at_utc TIMESTAMPTZ NOT NULL
            );
            CREATE TABLE "TeamInviteOperations" (
                team_id INTEGER NOT NULL,
                operation_id UUID NOT NULL,
                reconciled_at_utc TIMESTAMPTZ NULL,
                created_at_utc TIMESTAMPTZ NOT NULL
            );

            INSERT INTO "BulkChallengeMutationOperations" VALUES
                (1, '00000000-0000-0000-0000-000000000001', 2, NOW(), NOW() - INTERVAL '31 days', NOW() - INTERVAL '31 days'),
                (1, '00000000-0000-0000-0000-000000000002', 1, NOW() - INTERVAL '8 days', NULL, NOW() - INTERVAL '8 days'),
                (1, '00000000-0000-0000-0000-000000000003', 1, NOW() - INTERVAL '1 hour', NULL, NOW() - INTERVAL '1 hour');
            INSERT INTO "FlagImportOperations" VALUES
                (1, '00000000-0000-0000-0000-000000000004', 1, NOW(), NOW() - INTERVAL '31 days', NOW() - INTERVAL '31 days'),
                (1, '00000000-0000-0000-0000-000000000005', 0, NOW() - INTERVAL '8 days', NULL, NOW() - INTERVAL '8 days'),
                (1, '00000000-0000-0000-0000-000000000006', 0, NOW() - INTERVAL '1 hour', NULL, NOW() - INTERVAL '1 hour');
            INSERT INTO "TeamInviteOperations" VALUES
                (1, '00000000-0000-0000-0000-000000000007', NOW() - INTERVAL '31 days', NOW() - INTERVAL '31 days'),
                (1, '00000000-0000-0000-0000-000000000008', NULL, NOW() - INTERVAL '31 days'),
                (1, '00000000-0000-0000-0000-000000000009', NULL, NOW() - INTERVAL '1 hour');
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        assert_eq!(purge_expired(&pool, 1).await.unwrap(), 6);
        let remaining: i64 = sqlx::query_scalar(
            r#"SELECT
                (SELECT COUNT(*) FROM "BulkChallengeMutationOperations")
              + (SELECT COUNT(*) FROM "FlagImportOperations")
              + (SELECT COUNT(*) FROM "TeamInviteOperations")"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(remaining, 3);

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
