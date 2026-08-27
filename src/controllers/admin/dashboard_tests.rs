use super::*;

use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;
use tracing_subscriber::Layer;

#[test]
fn aggregate_result_bounds_are_explicit() {
    assert_eq!(TOP_GAME_LIMIT, 5);
    assert_eq!(TrendRange::Day.key(), "day");
    assert_eq!(TrendRange::Week.key(), "week");
    assert_eq!(TrendRange::Month.key(), "month");
    assert_eq!(TrendRange::Year.key(), "year");
}

#[derive(Clone)]
struct SqlxQueryCounter(Arc<AtomicUsize>);

impl<S> Layer<S> for SqlxQueryCounter
where
    S: Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        if event.metadata().target() == "sqlx::query" {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
}

struct Fixture {
    admin: PgPool,
    pool: PgPool,
    schema: String,
}

impl Fixture {
    async fn create(database_url: &str) -> Self {
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(database_url)
            .await
            .unwrap();
        let schema = format!("admin_dashboard_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "AspNetUsers" (id UUID PRIMARY KEY, user_name TEXT);
            CREATE TABLE "Teams" (
                id INTEGER PRIMARY KEY, name TEXT NOT NULL, bio TEXT,
                avatar_hash TEXT, locked BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TABLE "Containers" (id UUID PRIMARY KEY);
            CREATE TABLE "Games" (
                id INTEGER PRIMARY KEY, title TEXT NOT NULL, summary TEXT NOT NULL,
                poster_hash TEXT, team_member_count_limit INTEGER NOT NULL,
                start_time_utc TIMESTAMPTZ NOT NULL, end_time_utc TIMESTAMPTZ NOT NULL
            );
            CREATE TABLE "Files" (
                id INTEGER PRIMARY KEY, hash TEXT NOT NULL, name TEXT NOT NULL,
                upload_time_utc TIMESTAMPTZ NOT NULL
            );
            CREATE TABLE "Participations" (
                id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, team_id INTEGER NOT NULL,
                writeup_id INTEGER, division_id INTEGER
            );
            CREATE TABLE "UserParticipations" (
                user_id UUID NOT NULL, game_id INTEGER NOT NULL,
                team_id INTEGER NOT NULL, participation_id INTEGER NOT NULL
            );
            CREATE TABLE "GameChallenges" (
                id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, title TEXT NOT NULL
            );
            CREATE TABLE "ChallengeReviews" (
                id INTEGER PRIMARY KEY, challenge_id INTEGER NOT NULL, user_id UUID NOT NULL,
                game_id INTEGER NOT NULL, rating SMALLINT NOT NULL, comment TEXT,
                submit_time_utc TIMESTAMPTZ NOT NULL
            );
            CREATE TABLE "Submissions" (
                id BIGINT PRIMARY KEY, submit_time_utc TIMESTAMPTZ NOT NULL
            );
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

    async fn seed_range(&self, first: i32, last: i32) {
        sqlx::query(
            r#"
            INSERT INTO "Games"
                (id, title, summary, poster_hash, team_member_count_limit,
                 start_time_utc, end_time_utc)
            SELECT g, 'Game ' || g, 'Summary ' || g, 'poster-' || g, 0,
                   clock_timestamp() - interval '1 day',
                   clock_timestamp() + interval '1 day'
              FROM generate_series($1, $2) g
            "#,
        )
        .bind(first)
        .bind(last)
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO "Teams" (id, name, bio, avatar_hash, locked)
            SELECT g, 'Team ' || g, NULL, 'avatar-' || g, FALSE
              FROM generate_series($1, $2) g
            "#,
        )
        .bind(first)
        .bind(last)
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO "Files" (id, hash, name, upload_time_utc)
            SELECT g, 'hash-' || g, 'writeup-' || g || '.pdf', clock_timestamp()
              FROM generate_series($1, $2) g
            "#,
        )
        .bind(first)
        .bind(last)
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO "Participations" (id, game_id, team_id, writeup_id, division_id)
            SELECT g, g, g, g, NULL FROM generate_series($1, $2) g
            "#,
        )
        .bind(first)
        .bind(last)
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO "UserParticipations" (user_id, game_id, team_id, participation_id)
            SELECT lpad(to_hex(g * 10 + member), 32, '0')::uuid, g, g, g
              FROM generate_series($1, $2) g
              CROSS JOIN generate_series(1, 3) member
            "#,
        )
        .bind(first)
        .bind(last)
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO "GameChallenges" (id, game_id, title)
            SELECT g, g, 'Challenge ' || g FROM generate_series($1, $2) g
            "#,
        )
        .bind(first)
        .bind(last)
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO "ChallengeReviews"
                (id, challenge_id, user_id, game_id, rating, comment, submit_time_utc)
            SELECT g, g, '00000000-0000-0000-0000-000000000001'::uuid,
                   g, CASE WHEN g % 2 = 0 THEN 2 ELSE 1 END,
                   'Review ' || g, clock_timestamp() - g * interval '1 minute'
              FROM generate_series($1, $2) g
            "#,
        )
        .bind(first)
        .bind(last)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    async fn cleanup(self) {
        self.pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{}" CASCADE"#, self.schema))
            .execute(&self.admin)
            .await
            .unwrap();
        self.admin.close().await;
    }
}

async fn measured<T>(counter: &AtomicUsize, future: impl Future<Output = T>) -> (T, usize) {
    counter.store(0, Ordering::SeqCst);
    let value = future.await;
    (value, counter.load(Ordering::SeqCst))
}

#[test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
fn dashboard_queries_and_rows_stay_constant_as_tables_grow() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let counter = Arc::new(AtomicUsize::new(0));
    let subscriber = tracing_subscriber::registry().with(SqlxQueryCounter(counter.clone()));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    tracing::subscriber::with_default(subscriber, || {
        runtime.block_on(async {
            let fixture = Fixture::create(&database_url).await;
            sqlx::query(
                r#"INSERT INTO "AspNetUsers" (id, user_name)
                   VALUES ('00000000-0000-0000-0000-000000000001', 'reviewer')"#,
            )
            .execute(&fixture.pool)
            .await
            .unwrap();
            sqlx::query(
                r#"INSERT INTO "Containers" (id)
                   VALUES ('00000000-0000-0000-0000-000000000001')"#,
            )
            .execute(&fixture.pool)
            .await
            .unwrap();
            fixture.seed_range(1, 8).await;
            sqlx::query(
                r#"INSERT INTO "Submissions" (id, submit_time_utc)
                   SELECT g, clock_timestamp() - (g % 200) * interval '1 hour'
                     FROM generate_series(1, 500) g"#,
            )
            .execute(&fixture.pool)
            .await
            .unwrap();

            let (dashboard, dashboard_queries) =
                measured(&counter, load_dashboard_model(&fixture.pool)).await;
            let (reviews, review_queries) =
                measured(&counter, load_reviews(&fixture.pool, 5, 0)).await;
            let (writeups, writeup_queries) =
                measured(&counter, load_writeups(&fixture.pool, 5, 0)).await;
            dashboard.expect("small dashboard fixture should load");
            reviews.expect("small review fixture should load");
            writeups.expect("small writeup fixture should load");
            assert_eq!(dashboard_queries, 2);
            assert_eq!(review_queries, 1);
            assert_eq!(writeup_queries, 1);

            fixture.seed_range(9, 500).await;
            sqlx::query(
                r#"INSERT INTO "Submissions" (id, submit_time_utc)
                   SELECT g, clock_timestamp() - (g % 8760) * interval '1 hour'
                     FROM generate_series(501, 100500) g"#,
            )
            .execute(&fixture.pool)
            .await
            .unwrap();

            let (dashboard, grown_dashboard_queries) =
                measured(&counter, load_dashboard_model(&fixture.pool)).await;
            let (reviews, grown_review_queries) =
                measured(&counter, load_reviews(&fixture.pool, 5, 0)).await;
            let (writeups, grown_writeup_queries) =
                measured(&counter, load_writeups(&fixture.pool, 5, 0)).await;
            assert_eq!(grown_dashboard_queries, dashboard_queries);
            assert_eq!(grown_review_queries, review_queries);
            assert_eq!(grown_writeup_queries, writeup_queries);
            let dashboard = dashboard.expect("grown dashboard fixture should load");
            assert_eq!(dashboard.top_games.len(), 5);
            assert_eq!(dashboard.top_games[0].id, 500);
            assert_eq!(dashboard.top_games[0].team_count, 1);
            assert_eq!(dashboard.top_games[0].user_count, 3);
            assert_eq!(dashboard.top_games[0].review_count, 1);
            assert_eq!(reviews.expect("grown review fixture should load").len(), 5);
            assert_eq!(
                writeups.expect("grown writeup fixture should load").len(),
                5
            );

            for (range, expected_rows) in [
                (TrendRange::Day, 24),
                (TrendRange::Week, 7),
                (TrendRange::Month, 30),
                (TrendRange::Year, 12),
            ] {
                let (trend, query_count) = measured(
                    &counter,
                    load_submission_trend(&fixture.pool, range, Utc::now()),
                )
                .await;
                assert_eq!(query_count, 1);
                assert_eq!(
                    trend.expect("grown submission trend should load").len(),
                    expected_rows
                );
            }

            fixture.cleanup().await;
        });
    });
}
