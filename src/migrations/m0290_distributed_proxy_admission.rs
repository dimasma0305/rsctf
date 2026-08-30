//! Cross-replica live proxy and open-churn admission leases.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "ProxyTunnelLeases" (
    lease_id UUID PRIMARY KEY,
    user_id UUID NOT NULL,
    scope_kind SMALLINT NOT NULL CHECK (scope_kind BETWEEN 0 AND 3),
    scope_id TEXT NOT NULL CHECK (LENGTH(scope_id) BETWEEN 1 AND 64),
    source_ip TEXT NOT NULL CHECK (LENGTH(source_ip) BETWEEN 2 AND 64),
    event_id INTEGER NULL,
    workload_id UUID NOT NULL,
    expires_at_utc TIMESTAMPTZ NOT NULL,
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

CREATE INDEX IF NOT EXISTS ix_proxy_tunnel_lease_expiry
    ON "ProxyTunnelLeases" (expires_at_utc);
CREATE INDEX IF NOT EXISTS ix_proxy_tunnel_lease_user
    ON "ProxyTunnelLeases" (user_id, expires_at_utc);
CREATE INDEX IF NOT EXISTS ix_proxy_tunnel_lease_scope
    ON "ProxyTunnelLeases" (scope_kind, scope_id, expires_at_utc);
CREATE INDEX IF NOT EXISTS ix_proxy_tunnel_lease_source
    ON "ProxyTunnelLeases" (source_ip, expires_at_utc);
CREATE INDEX IF NOT EXISTS ix_proxy_tunnel_lease_event
    ON "ProxyTunnelLeases" (event_id, expires_at_utc)
    WHERE event_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_proxy_tunnel_lease_workload
    ON "ProxyTunnelLeases" (workload_id, expires_at_utc);

CREATE TABLE IF NOT EXISTS "ProxyOpenBudgets" (
    bucket_start_utc TIMESTAMPTZ NOT NULL,
    source_key TEXT NOT NULL CHECK (LENGTH(source_key) BETWEEN 1 AND 64),
    open_count INTEGER NOT NULL CHECK (open_count BETWEEN 1 AND 128),
    PRIMARY KEY (bucket_start_utc, source_key),
    CONSTRAINT ck_proxy_open_bucket CHECK (
        bucket_start_utc = date_trunc('second', bucket_start_utc)
    )
);

CREATE INDEX IF NOT EXISTS ix_proxy_open_budget_expiry
    ON "ProxyOpenBudgets" (bucket_start_utc);
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Production migrations are forward-only. Older binaries leave these
        // bounded admission tables untouched.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::UP_SQL;

    #[test]
    fn proxy_admission_schema_is_idempotent_bounded_and_indexed() {
        assert_eq!(UP_SQL.matches("CREATE TABLE IF NOT EXISTS").count(), 2);
        assert!(UP_SQL.contains("scope_kind BETWEEN 0 AND 3"));
        assert!(UP_SQL.contains("LENGTH(scope_id) BETWEEN 1 AND 64"));
        assert!(UP_SQL.contains("LENGTH(source_ip) BETWEEN 2 AND 64"));
        assert!(UP_SQL.contains("open_count BETWEEN 1 AND 128"));
        assert!(UP_SQL.contains("PRIMARY KEY (bucket_start_utc, source_key)"));
        assert!(UP_SQL.contains("bucket_start_utc = date_trunc('second', bucket_start_utc)"));

        for index in [
            "ix_proxy_tunnel_lease_expiry",
            "ix_proxy_tunnel_lease_user",
            "ix_proxy_tunnel_lease_scope",
            "ix_proxy_tunnel_lease_source",
            "ix_proxy_tunnel_lease_event",
            "ix_proxy_tunnel_lease_workload",
            "ix_proxy_open_budget_expiry",
        ] {
            assert!(
                UP_SQL.contains(&format!("CREATE INDEX IF NOT EXISTS {index}")),
                "missing bounded-admission index {index}"
            );
        }
    }

    #[tokio::test]
    #[ignore = "requires disposable PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn proxy_admission_schema_applies_twice_and_enforces_bounds() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("connect test database");
        let schema = format!("proxy_admission_m0290_{}", uuid::Uuid::new_v4().simple());
        assert!(schema
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'));
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .expect("create isolated schema");

        let options = PgConnectOptions::from_str(&database_url)
            .expect("parse test database URL")
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .expect("connect isolated pool");

        sqlx::raw_sql(UP_SQL)
            .execute(&pool)
            .await
            .expect("apply proxy admission schema");
        sqlx::raw_sql(UP_SQL)
            .execute(&pool)
            .await
            .expect("reapply proxy admission schema idempotently");

        let table_count: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*)::bigint
                 FROM pg_class relation
                 JOIN pg_namespace namespace ON namespace.oid = relation.relnamespace
                WHERE namespace.nspname = current_schema()
                  AND relation.relname IN ('ProxyTunnelLeases', 'ProxyOpenBudgets')
                  AND relation.relkind = 'r'"#,
        )
        .fetch_one(&pool)
        .await
        .expect("count admission tables");
        assert_eq!(table_count, 2);

        let subject = uuid::Uuid::new_v4();
        let workload = uuid::Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO "ProxyTunnelLeases"
                 (lease_id, user_id, scope_kind, scope_id, source_ip,
                  event_id, workload_id, expires_at_utc)
               VALUES ($1, $2, 3, '42', '192.0.2.10', 7, $3,
                       clock_timestamp() + INTERVAL '31 minutes')"#,
        )
        .bind(uuid::Uuid::new_v4())
        .bind(subject)
        .bind(workload)
        .execute(&pool)
        .await
        .expect("SSH is a supported admission scope");
        assert!(sqlx::query(
            r#"INSERT INTO "ProxyTunnelLeases"
                 (lease_id, user_id, scope_kind, scope_id, source_ip,
                  event_id, workload_id, expires_at_utc)
               VALUES ($1, $2, 4, '42', '192.0.2.10', 7, $3,
                       clock_timestamp() + INTERVAL '31 minutes')"#,
        )
        .bind(uuid::Uuid::new_v4())
        .bind(subject)
        .bind(workload)
        .execute(&pool)
        .await
        .is_err());
        assert!(sqlx::query(
            r#"INSERT INTO "ProxyOpenBudgets"
                 (bucket_start_utc, source_key, open_count)
               VALUES (date_trunc('second', clock_timestamp()), '192.0.2.10', 129)"#,
        )
        .execute(&pool)
        .await
        .is_err());

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .expect("drop isolated schema");
        admin.close().await;
    }
}
