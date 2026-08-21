//! Normalize container-policy values accepted by older releases.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP_SQL: &str = r#"
WITH bounds(config_key, minimum, maximum) AS (
    VALUES
        ('ContainerPolicy:MaxExerciseContainerCountPerUser', 1::numeric, 100::numeric),
        ('ContainerPolicy:DefaultLifetime',                  1::numeric, 7200::numeric),
        ('ContainerPolicy:ExtensionDuration',                1::numeric, 7200::numeric),
        ('ContainerPolicy:RenewalWindow',                    1::numeric, 360::numeric),
        ('ContainerPolicy:ImageIdleRetentionHours',          1::numeric, 8760::numeric),
        ('ContainerPolicy:BuildCacheRetentionHours',         1::numeric, 8760::numeric),
        ('ContainerPolicy:MinimumFreeStorageGiB',            0::numeric, 1024::numeric)
), normalized AS (
    SELECT config.config_key,
           CASE
               WHEN config.value ~ '^[+-]?[0-9]+$'
                AND length(config.value) <= 11
               THEN config.value::numeric
           END AS numeric_value,
           bounds.minimum,
           bounds.maximum
      FROM "Configs" config
      JOIN bounds ON bounds.config_key = config.config_key
)
UPDATE "Configs" config
   SET value = LEAST(
                   GREATEST(normalized.numeric_value, normalized.minimum),
                   normalized.maximum
               )::text
  FROM normalized
 WHERE config.config_key = normalized.config_key
   AND normalized.numeric_value IS NOT NULL
   AND normalized.numeric_value NOT BETWEEN normalized.minimum AND normalized.maximum;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // A downgrade cannot reconstruct invalid legacy values safely.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::*;

    #[test]
    fn migration_clamps_every_validated_integer_policy() {
        for key in [
            "MaxExerciseContainerCountPerUser",
            "DefaultLifetime",
            "ExtensionDuration",
            "RenewalWindow",
            "ImageIdleRetentionHours",
            "BuildCacheRetentionHours",
            "MinimumFreeStorageGiB",
        ] {
            assert!(UP_SQL.contains(key));
        }
        assert!(UP_SQL.contains("GREATEST"));
        assert!(UP_SQL.contains("LEAST"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn migration_clamps_parseable_legacy_values_and_preserves_malformed_rows() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("container_policy_bounds_{}", uuid::Uuid::new_v4().simple());
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
            CREATE TABLE "Configs" (
                config_key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            INSERT INTO "Configs" (config_key, value) VALUES
                ('ContainerPolicy:MaxExerciseContainerCountPerUser', '0'),
                ('ContainerPolicy:DefaultLifetime', '9999'),
                ('ContainerPolicy:MinimumFreeStorageGiB', '-2'),
                ('ContainerPolicy:RenewalWindow', '30'),
                ('ContainerPolicy:ExtensionDuration', 'not-a-number');
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        let rows = sqlx::query_as::<_, (String, String)>(
            r#"SELECT config_key, value FROM "Configs" ORDER BY config_key"#,
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            rows,
            vec![
                (
                    "ContainerPolicy:DefaultLifetime".to_string(),
                    "7200".to_string(),
                ),
                (
                    "ContainerPolicy:ExtensionDuration".to_string(),
                    "not-a-number".to_string(),
                ),
                (
                    "ContainerPolicy:MaxExerciseContainerCountPerUser".to_string(),
                    "1".to_string(),
                ),
                (
                    "ContainerPolicy:MinimumFreeStorageGiB".to_string(),
                    "0".to_string(),
                ),
                (
                    "ContainerPolicy:RenewalWindow".to_string(),
                    "30".to_string(),
                ),
            ]
        );

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
