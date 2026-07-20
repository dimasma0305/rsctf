//! Normalize legacy PostgreSQL infinite identity lockout timestamps.
//!
//! Chrono cannot represent PostgreSQL's `infinity` sentinels. Decoding a full
//! `AspNetUsers` row containing one panics inside SQLx, taking down the request
//! instead of returning an application error. Preserve positive infinity as an
//! effectively permanent but representable lockout and clear negative infinity,
//! which is already in the past. A check prevents the unsafe values returning.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP_SQL: &str = r#"
    UPDATE "AspNetUsers"
       SET lockout_end = CASE
           WHEN lockout_end = 'infinity'::TIMESTAMPTZ
               THEN TIMESTAMPTZ '9999-12-31 23:59:59.999999+00'
           WHEN lockout_end = '-infinity'::TIMESTAMPTZ
               THEN NULL
           ELSE lockout_end
       END
     WHERE lockout_end IS NOT NULL
       AND NOT isfinite(lockout_end);

    DO $$
    BEGIN
        IF NOT EXISTS (
            SELECT 1
              FROM pg_constraint
             WHERE conname = 'ck_aspnetusers_lockout_end_finite'
               AND conrelid = '"AspNetUsers"'::regclass
        ) THEN
            ALTER TABLE "AspNetUsers"
              ADD CONSTRAINT ck_aspnetusers_lockout_end_finite
              CHECK (lockout_end IS NULL OR isfinite(lockout_end)) NOT VALID;
        END IF;
    END
    $$;

    ALTER TABLE "AspNetUsers"
      VALIDATE CONSTRAINT ck_aspnetusers_lockout_end_finite;
"#;

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
            .execute_unprepared(
                r#"ALTER TABLE "AspNetUsers"
                     DROP CONSTRAINT IF EXISTS ck_aspnetusers_lockout_end_finite;"#,
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;
    use chrono::{DateTime, Datelike, Utc};
    use sqlx::postgres::PgPoolOptions;

    #[test]
    fn normalizes_before_enforcing_the_finite_invariant() {
        let normalize = UP_SQL.find("UPDATE \"AspNetUsers\"").unwrap();
        let constraint = UP_SQL.find("ADD CONSTRAINT").unwrap();
        assert!(normalize < constraint);
        assert!(UP_SQL.contains("WHEN lockout_end = 'infinity'::TIMESTAMPTZ"));
        assert!(UP_SQL.contains("WHEN lockout_end = '-infinity'::TIMESTAMPTZ"));
        assert!(UP_SQL.contains("NOT isfinite(lockout_end)"));
        assert!(UP_SQL.contains("lockout_end IS NULL OR isfinite(lockout_end)"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn legacy_infinities_are_normalized_and_cannot_return() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("finite_lockout_{}", uuid::Uuid::new_v4().simple());
        let setup = format!(
            r#"
            CREATE SCHEMA "{schema}";
            CREATE TABLE "{schema}"."AspNetUsers" (
                id INTEGER PRIMARY KEY,
                lockout_end TIMESTAMPTZ
            );
            INSERT INTO "{schema}"."AspNetUsers" (id, lockout_end)
            VALUES
                (1, 'infinity'),
                (2, '-infinity'),
                (3, '2030-01-02T03:04:05Z'),
                (4, NULL);
            "#
        );
        sqlx::raw_sql(&setup).execute(&admin).await.unwrap();

        let search_path_schema = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .after_connect(move |connection, _metadata| {
                let statement = format!(r#"SET search_path TO "{search_path_schema}""#);
                Box::pin(async move {
                    sqlx::query(&statement).execute(connection).await?;
                    Ok(())
                })
            })
            .connect(&database_url)
            .await
            .unwrap();

        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();

        // This is the same Chrono decoding path used by `user::Model`; any
        // remaining PostgreSQL infinity sentinel would panic here.
        let values = sqlx::query_as::<_, (i32, Option<DateTime<Utc>>)>(
            r#"SELECT id, lockout_end
                 FROM "AspNetUsers"
                ORDER BY id"#,
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(values[0].1.as_ref().map(Datelike::year), Some(9999));
        assert_eq!(values[1].1, None);
        assert_eq!(
            values[2].1,
            Some(
                DateTime::parse_from_rfc3339("2030-01-02T03:04:05Z")
                    .unwrap()
                    .with_timezone(&Utc)
            )
        );
        assert_eq!(values[3].1, None);

        let rejected = sqlx::query(
            r#"INSERT INTO "AspNetUsers" (id, lockout_end)
               VALUES (5, 'infinity')"#,
        )
        .execute(&pool)
        .await
        .unwrap_err();
        assert!(matches!(
            rejected,
            sqlx::Error::Database(error) if error.code().as_deref() == Some("23514")
        ));

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
