use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use uuid::Uuid;

use super::{
    count_posts, load_all_posts, load_post_page, LATEST_POST_LIMIT, MAX_POST_PAGE_SIZE,
    ORDERED_POST_PAGE_SQL,
};

struct PostFeedHarness {
    admin: sqlx::PgPool,
    pool: sqlx::PgPool,
    schema: String,
}

impl PostFeedHarness {
    async fn new() -> Self {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("rsctf_post_feed_{}", Uuid::new_v4().simple());
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
            CREATE TABLE "AspNetUsers" (
                id UUID PRIMARY KEY,
                user_name TEXT,
                avatar_hash TEXT
            );
            CREATE TABLE "Posts" (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                summary TEXT NOT NULL,
                content TEXT NOT NULL,
                is_pinned BOOLEAN NOT NULL,
                tags JSONB,
                author_id UUID,
                update_time_utc TIMESTAMPTZ NOT NULL
            );
            CREATE INDEX ix_posts_feed_order
                ON "Posts" (is_pinned DESC, update_time_utc DESC, id DESC);
            INSERT INTO "AspNetUsers" (id, user_name, avatar_hash)
            SELECT md5(number::text)::uuid,
                   'author-' || number::text,
                   'avatar-' || number::text
              FROM generate_series(1, 100) AS number;
            INSERT INTO "Posts" (
                id, title, summary, content, is_pinned, tags, author_id,
                update_time_utc
            )
            SELECT lpad(number::text, 8, '0'),
                   'Post ' || number::text,
                   repeat('summary ', 20),
                   repeat('content that must not enter the feed projection ', 100),
                   number <= 120,
                   '["news"]'::jsonb,
                   md5((((number - 1) % 100) + 1)::text)::uuid,
                   TIMESTAMPTZ '2026-01-01 00:00:00+00'
                       + number * INTERVAL '1 second'
              FROM generate_series(1, 10000) AS number;
            INSERT INTO "Posts" (
                id, title, summary, content, is_pinned, tags, author_id,
                update_time_utc
            ) VALUES
                ('yyyyyyyy', 'Tie Y', 'summary', 'content', TRUE, NULL, NULL,
                 TIMESTAMPTZ '2030-01-01 00:00:00+00'),
                ('zzzzzzzz', 'Tie Z', 'summary', 'content', TRUE, NULL, NULL,
                 TIMESTAMPTZ '2030-01-01 00:00:00+00');
            ANALYZE "Posts";
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        Self {
            admin,
            pool,
            schema,
        }
    }

    async fn cleanup(self) {
        self.pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{}" CASCADE"#, self.schema))
            .execute(&self.admin)
            .await
            .unwrap();
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn large_feed_preserves_legacy_history_and_bounds_paginated_consumers() {
    let fixture = PostFeedHarness::new().await;

    let total = count_posts(&fixture.pool).await.unwrap();
    assert_eq!(total, 10_002);

    let first = load_post_page(&fixture.pool, 0, LATEST_POST_LIMIT)
        .await
        .unwrap();
    assert_eq!(first.len(), LATEST_POST_LIMIT as usize);
    assert_eq!(first[0].id, "zzzzzzzz");
    assert_eq!(first[1].id, "yyyyyyyy");
    assert!(first.iter().all(|post| post.is_pinned));
    assert_eq!(first[2].id, "00000120");
    assert_eq!(first[2].author_name.as_deref(), Some("author-20"));
    assert_eq!(
        first[2].author_avatar.as_deref(),
        Some("/assets/avatar-20/avatar")
    );
    assert!(
        serde_json::to_vec(&first).unwrap().len() < 16 * 1024,
        "latest response size must depend on 20 summaries, not retained history"
    );

    let legacy = load_all_posts(&fixture.pool).await.unwrap();
    assert_eq!(
        legacy.len(),
        10_002,
        "legacy array must retain complete history"
    );
    assert!(legacy.iter().any(|post| post.id == "00000001"));
    assert_eq!(legacy.last().map(|post| post.id.as_str()), Some("00000121"));

    let over_limit = load_post_page(&fixture.pool, 0, i64::MAX).await.unwrap();
    assert_eq!(over_limit.len(), MAX_POST_PAGE_SIZE as usize);

    let second = load_post_page(&fixture.pool, 20, 10).await.unwrap();
    assert_eq!(second.len(), 10);
    assert_eq!(second[0].id, "00000102");
    assert!(first
        .iter()
        .all(|post| !second.iter().any(|row| row.id == post.id)));

    let explain = format!("EXPLAIN (FORMAT JSON) {ORDERED_POST_PAGE_SQL}");
    let plan = sqlx::query_scalar::<_, serde_json::Value>(&explain)
        .bind(0_i64)
        .bind(20_i64)
        .fetch_one(&fixture.pool)
        .await
        .unwrap()
        .to_string();
    assert!(
        plan.contains("ix_posts_feed_order"),
        "latest-page plan must use the ordered feed index: {plan}"
    );
    assert!(!ORDERED_POST_PAGE_SQL.contains("post.content"));
    assert!(ORDERED_POST_PAGE_SQL.contains("MATERIALIZED"));

    fixture.cleanup().await;
}
