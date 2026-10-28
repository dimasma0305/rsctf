//! Isolated PostgreSQL schemas for regressions that exercise real SQL races.
pub(crate) struct TestDatabase {
    pub pool: sqlx::PgPool,
    admin: sqlx::PgPool,
    schema: String,
}

impl TestDatabase {
    pub async fn new(prefix: &str) -> Self {
        assert!(
            prefix.len() <= 24
                && prefix
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        );
        let url = std::env::var("RSCTF_TEST_DATABASE_URL").expect("disposable PostgreSQL required");
        let options = crate::migrations::test_pg_connect_options(&url);
        let admin = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options.clone())
            .await
            .unwrap();
        let schema = format!("{prefix}_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(12)
            .connect_with(options.options([("search_path", schema.as_str())]))
            .await
            .unwrap();
        Self {
            pool,
            admin,
            schema,
        }
    }

    pub async fn close(self) {
        self.pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{}" CASCADE"#, self.schema))
            .execute(&self.admin)
            .await
            .unwrap();
        self.admin.close().await;
    }
}
