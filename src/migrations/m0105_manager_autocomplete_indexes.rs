//! Indexes for the bounded game-manager identity autocomplete.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[cfg(test)]
const USERNAME_INDEX: &str = "ix_aspnetusers_manager_username_prefix";
#[cfg(test)]
const EMAIL_INDEX: &str = "ix_aspnetusers_manager_email_prefix";

const UP_SQL: &str = r#"
CREATE INDEX IF NOT EXISTS ix_aspnetusers_manager_username_prefix
    ON "AspNetUsers" ((normalized_user_name COLLATE "C"), id)
    WHERE normalized_user_name IS NOT NULL;

CREATE INDEX IF NOT EXISTS ix_aspnetusers_manager_email_prefix
    ON "AspNetUsers" ((normalized_email COLLATE "C"), id)
    WHERE normalized_email IS NOT NULL;
"#;

const DOWN_SQL: &str = r#"
DROP INDEX IF EXISTS ix_aspnetusers_manager_email_prefix;
DROP INDEX IF EXISTS ix_aspnetusers_manager_username_prefix;
"#;

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
    use std::str::FromStr;

    use serde_json::Value;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::*;
    use crate::controllers::admin::users_manager_autocomplete::{
        manager_autocomplete_rows, MANAGER_AUTOCOMPLETE_LIMIT, MANAGER_AUTOCOMPLETE_SQL,
    };

    fn plan_uses_bounded_index(node: &Value, index: &str) -> bool {
        let uses_index = node.get("Index Name").and_then(Value::as_str) == Some(index)
            && node
                .get("Actual Rows")
                .and_then(Value::as_f64)
                .is_some_and(|rows| rows <= MANAGER_AUTOCOMPLETE_LIMIT as f64);
        uses_index
            || node
                .get("Plans")
                .and_then(Value::as_array)
                .is_some_and(|plans| {
                    plans
                        .iter()
                        .any(|plan| plan_uses_bounded_index(plan, index))
                })
    }

    #[test]
    fn migration_is_idempotent_narrow_and_collation_stable() {
        assert_eq!(UP_SQL.matches("CREATE INDEX IF NOT EXISTS").count(), 2);
        assert!(UP_SQL.contains("normalized_user_name COLLATE \"C\""));
        assert!(UP_SQL.contains("normalized_email COLLATE \"C\""));
        // Fetching at most twenty heap tuples is cheaper than duplicating
        // three variable-length display fields in both indexes forever.
        assert!(!UP_SQL.contains("INCLUDE"));
        assert!(DOWN_SQL.contains(USERNAME_INDEX));
        assert!(DOWN_SQL.contains(EMAIL_INDEX));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn large_table_prefix_plans_and_concurrent_reads_remain_bounded() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("manager_autocomplete_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(12)
            .connect_with(options)
            .await
            .unwrap();

        sqlx::raw_sql(
            r#"CREATE TABLE "AspNetUsers" (
                   id UUID PRIMARY KEY,
                   user_name TEXT,
                   normalized_user_name TEXT,
                   email TEXT,
                   normalized_email TEXT,
                   avatar_hash TEXT
               );
               INSERT INTO "AspNetUsers"
                      (id, user_name, normalized_user_name, email, normalized_email, avatar_hash)
               SELECT md5(value::text)::uuid,
                      'user' || LPAD(value::text, 6, '0'),
                      'USER' || LPAD(value::text, 6, '0'),
                      'person' || LPAD(value::text, 6, '0') || '@example.test',
                      'PERSON' || LPAD(value::text, 6, '0') || '@EXAMPLE.TEST',
                      md5('avatar' || value::text)
                 FROM generate_series(1, 50000) value;
               INSERT INTO "AspNetUsers"
                      (id, user_name, normalized_user_name, email, normalized_email, avatar_hash)
               VALUES (md5('literal-wildcard')::uuid, 'al%_ice', 'AL%_ICE',
                       'literal@example.test', 'LITERAL@EXAMPLE.TEST', NULL),
                      (md5('oversized-legacy')::uuid, REPEAT('x', 10000), 'USER499999',
                       REPEAT('e', 10000), 'USER499999@EXAMPLE.TEST', REPEAT('a', 10000));
               ANALYZE "AspNetUsers";"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::query("ANALYZE \"AspNetUsers\"")
            .execute(&pool)
            .await
            .unwrap();

        let explain = format!("EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) {MANAGER_AUTOCOMPLETE_SQL}");
        let plan: Value = sqlx::query_scalar(&explain)
            .bind("USER49")
            .bind("USER4:")
            .bind(MANAGER_AUTOCOMPLETE_LIMIT)
            .fetch_one(&pool)
            .await
            .unwrap();
        let root = &plan[0]["Plan"];
        assert!(plan_uses_bounded_index(root, USERNAME_INDEX), "{plan}");
        assert!(plan_uses_bounded_index(root, EMAIL_INDEX), "{plan}");
        let index_bytes: i64 = sqlx::query_scalar(&format!(
            "SELECT pg_relation_size('{USERNAME_INDEX}'::regclass) + \
                    pg_relation_size('{EMAIL_INDEX}'::regclass)"
        ))
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(index_bytes <= 16 * 1024 * 1024, "{index_bytes}");

        let response = manager_autocomplete_rows(&pool, "USER49", "USER4:")
            .await
            .unwrap();
        assert!(response.len() <= MANAGER_AUTOCOMPLETE_LIMIT as usize);
        assert!(serde_json::to_vec(&response).unwrap().len() <= 8 * 1024);
        let oversized = manager_autocomplete_rows(&pool, "USER499999", "USER49999:")
            .await
            .unwrap();
        assert_eq!(oversized.len(), 1);
        assert_eq!(oversized[0].user_name.as_deref().unwrap().len(), 128);
        assert_eq!(oversized[0].email.as_deref().unwrap().len(), 320);
        assert!(serde_json::to_vec(&oversized).unwrap().len() <= 1024);

        let wildcard = manager_autocomplete_rows(&pool, "AL%_", "AL%`")
            .await
            .unwrap();
        assert_eq!(wildcard.len(), 1);
        assert_eq!(wildcard[0].user_name.as_deref(), Some("al%_ice"));

        let mut readers = tokio::task::JoinSet::new();
        for suffix in 0..8 {
            let pool = pool.clone();
            readers.spawn(async move {
                let prefix = format!("USER{suffix}");
                let upper = format!("USER{}", suffix + 1);
                manager_autocomplete_rows(&pool, &prefix, &upper).await
            });
        }
        while let Some(result) = readers.join_next().await {
            assert!(result.unwrap().unwrap().len() <= MANAGER_AUTOCOMPLETE_LIMIT as usize);
        }

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
