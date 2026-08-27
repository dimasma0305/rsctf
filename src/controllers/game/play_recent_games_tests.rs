use super::*;
use chrono::TimeZone;
use sqlx::postgres::PgPoolOptions;

const PRECISE_FULL_SORT_RECENT_GAME_IDS_SQL: &str = r#"
    SELECT id
      FROM "Games"
     WHERE hidden = FALSE
     ORDER BY CASE
         WHEN end_time_utc <= $1::timestamptz THEN
             $1::timestamptz - end_time_utc
         WHEN start_time_utc >= $1::timestamptz THEN
             start_time_utc - $1::timestamptz
         ELSE LEAST(
             $1::timestamptz - start_time_utc,
             end_time_utc - $1::timestamptz
         )
     END ASC,
     id ASC
     LIMIT $2
"#;

async fn recent_games_test_pool() -> sqlx::PgPool {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect test database");
    sqlx::raw_sql(
        r#"CREATE TEMP TABLE "Games" (
            id INTEGER PRIMARY KEY,
            title TEXT NOT NULL,
            summary TEXT NOT NULL,
            poster_hash TEXT,
            team_member_count_limit INTEGER NOT NULL,
            hidden BOOLEAN NOT NULL,
            start_time_utc TIMESTAMPTZ NOT NULL,
            end_time_utc TIMESTAMPTZ NOT NULL
        )"#,
    )
    .execute(&pool)
    .await
    .expect("create recent-games fixture table");
    pool
}

async fn install_candidate_indexes(pool: &sqlx::PgPool) {
    sqlx::raw_sql(crate::migrations::RECENT_GAMES_INDEX_SQL)
        .execute(pool)
        .await
        .expect("install recent-games candidate indexes");
}

fn actual_rows(plan_line: &str) -> Option<u64> {
    let tail = plan_line.rsplit_once(" rows=")?.1;
    tail.split(|character: char| !character.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

fn plan_counter(plan: &str, label: &str) -> u64 {
    plan.lines()
        .filter_map(|line| line.trim().strip_prefix(label))
        .filter_map(|value| {
            value
                .trim()
                .split(|character: char| !character.is_ascii_digit())
                .next()?
                .parse::<u64>()
                .ok()
        })
        .sum()
}

async fn explain_recent_games(pool: &sqlx::PgPool, now: DateTime<Utc>, limit: i64) -> String {
    let explain = format!("EXPLAIN (ANALYZE, BUFFERS, COSTS OFF, FORMAT TEXT) {RECENT_GAMES_SQL}");
    sqlx::query_scalar::<_, String>(&explain)
        .bind(now)
        .bind(limit)
        .fetch_all(pool)
        .await
        .expect("explain bounded recent-games query")
        .join("\n")
}

async fn bounded_recent_game_ids(pool: &sqlx::PgPool, now: DateTime<Utc>, limit: i64) -> Vec<i32> {
    query_recent_games(pool, now, limit)
        .await
        .expect("query bounded candidates")
        .into_iter()
        .map(|row| row.id)
        .collect()
}

async fn precise_full_sort_recent_game_ids(
    pool: &sqlx::PgPool,
    now: DateTime<Utc>,
    limit: i64,
) -> Vec<i32> {
    sqlx::query_scalar::<_, i32>(PRECISE_FULL_SORT_RECENT_GAME_IDS_SQL)
        .bind(now)
        .bind(limit)
        .fetch_all(pool)
        .await
        .expect("query precise full-sort order")
}

async fn assert_recent_game_ids(
    pool: &sqlx::PgPool,
    now: DateTime<Utc>,
    limit: i64,
    expected: &[i32],
) {
    let actual = bounded_recent_game_ids(pool, now, limit).await;
    let full_sort = precise_full_sort_recent_game_ids(pool, now, limit).await;
    assert_eq!(actual, expected);
    assert_eq!(actual, full_sort);
}

async fn grow_historical_games(pool: &sqlx::PgPool, now: DateTime<Utc>, offset: i32, count: i32) {
    sqlx::query(
        r#"INSERT INTO "Games"
           SELECT $1 + n, 'old history', '', NULL, 0, FALSE,
                  $3::timestamptz - interval '730 days' - make_interval(secs => n * 2),
                  $3::timestamptz - interval '730 days' - make_interval(secs => n * 2 - 1)
             FROM generate_series(1, $2) AS n"#,
    )
    .bind(offset)
    .bind(count)
    .bind(now)
    .execute(pool)
    .await
    .expect("grow historical games");
}

async fn grow_future_games(pool: &sqlx::PgPool, now: DateTime<Utc>, offset: i32, count: i32) {
    sqlx::query(
        r#"INSERT INTO "Games"
           SELECT $1 + n, 'far future', '', NULL, 0, FALSE,
                  $3::timestamptz + interval '730 days' + make_interval(secs => n * 2),
                  $3::timestamptz + interval '731 days' + make_interval(secs => n * 2)
             FROM generate_series(1, $2) AS n"#,
    )
    .bind(offset)
    .bind(count)
    .bind(now)
    .execute(pool)
    .await
    .expect("grow future games");
}

async fn grow_active_games(pool: &sqlx::PgPool, now: DateTime<Utc>, offset: i32, count: i32) {
    sqlx::query(
        r#"INSERT INTO "Games"
           SELECT $1 + n, 'far active', '', NULL, 0, FALSE,
                  $3::timestamptz - interval '365 days' - make_interval(secs => n),
                  $3::timestamptz + interval '365 days' + make_interval(secs => n)
             FROM generate_series(1, $2) AS n"#,
    )
    .bind(offset)
    .bind(count)
    .bind(now)
    .execute(pool)
    .await
    .expect("grow concurrently active games");
}

async fn analyze_and_explain(pool: &sqlx::PgPool, now: DateTime<Utc>, limit: i64) -> String {
    sqlx::raw_sql(r#"VACUUM (ANALYZE) "Games""#)
        .execute(pool)
        .await
        .expect("analyze recent-games fixture");
    explain_recent_games(pool, now, limit).await
}

#[test]
fn recent_games_stamp_the_clock_after_the_database_read() {
    let source = include_str!("play.rs");
    let database_read = source
        .find("query_recent_games_coalesced(st.pg()")
        .expect("recent-games database read remains visible");
    let response_stamp = source
        .find("let response_time = Utc::now();")
        .expect("recent-games response has a fresh clock sample");

    assert!(database_read < response_stamp);
    assert!(source[response_stamp..].contains("server_time: response_time"));
}

#[test]
fn recent_games_query_has_four_bounded_candidates_and_a_bounded_flight_key() {
    for fragment in [
        "ORDER BY end_time_utc DESC, id ASC",
        "ORDER BY start_time_utc ASC, id ASC",
        "ORDER BY start_time_utc DESC, id ASC",
        "ORDER BY end_time_utc ASC, id ASC",
        "nearest AS MATERIALIZED",
        "ORDER BY distance ASC, id ASC",
    ] {
        assert!(
            RECENT_GAMES_SQL.contains(fragment),
            "missing recent-games query invariant: {fragment}"
        );
    }
    assert!(!RECENT_GAMES_SQL.contains("FLOOR("));
    assert_eq!(RECENT_GAMES_SQL.matches("LIMIT $2").count(), 5);
    assert_eq!(recent_games_limit(0), 50);
    assert_eq!(recent_games_limit(1), 1);
    assert_eq!(recent_games_limit(50), 50);
    assert_eq!(recent_games_limit(51), 50);
    assert_eq!(recent_games_limit(usize::MAX), 50);
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn recent_games_candidates_preserve_the_precise_full_sort_order() {
    let pool = recent_games_test_pool().await;
    let now = Utc
        .with_ymd_and_hms(2026, 8, 26, 12, 0, 0)
        .single()
        .expect("valid fixture timestamp");
    sqlx::raw_sql(
        r#"INSERT INTO "Games" VALUES
            (1, 'ended', '', NULL, 0, FALSE,
             '2026-08-26 11:59:40+00', '2026-08-26 11:59:57+00'),
            (2, 'upcoming', '', NULL, 0, FALSE,
             '2026-08-26 12:00:02+00', '2026-08-27 12:00:00+00'),
            (3, 'active-start', '', NULL, 0, FALSE,
             '2026-08-26 11:59:59+00', '2026-08-26 13:00:00+00'),
            (4, 'hidden', '', NULL, 0, TRUE,
             '2026-08-26 12:00:00+00', '2026-08-27 12:00:00+00'),
            (5, 'active-end', '', NULL, 0, FALSE,
             '2026-08-26 11:00:00+00', '2026-08-26 12:00:01+00'),
            (6, 'active-tie-low-id', '', NULL, 0, FALSE,
             '2026-08-26 11:59:56+00', '2026-08-26 14:00:00+00'),
            (7, 'active-tie-high-id', '', NULL, 0, FALSE,
             '2026-08-26 10:00:00+00', '2026-08-26 12:00:04+00');

        INSERT INTO "Games"
        SELECT 1000 + n, 'past filler', '', NULL, 0, FALSE,
               '2026-08-01 00:00:00+00'::timestamptz - make_interval(secs => n),
               '2026-08-02 00:00:00+00'::timestamptz - make_interval(secs => n)
          FROM generate_series(1, 300) AS n;

        INSERT INTO "Games"
        SELECT 2000 + n, 'future filler', '', NULL, 0, FALSE,
               '2026-09-01 00:00:00+00'::timestamptz + make_interval(secs => n),
               '2026-09-02 00:00:00+00'::timestamptz + make_interval(secs => n)
          FROM generate_series(1, 300) AS n;

        INSERT INTO "Games"
        SELECT 3000 + n, 'active filler', '', NULL, 0, FALSE,
               '2026-08-20 00:00:00+00'::timestamptz - make_interval(secs => n),
               '2026-09-20 00:00:00+00'::timestamptz + make_interval(secs => n)
          FROM generate_series(1, 300) AS n;"#,
    )
    .execute(&pool)
    .await
    .expect("insert ordering fixtures");
    install_candidate_indexes(&pool).await;

    for limit in [1_i64, 3, 7, 50] {
        let expected = precise_full_sort_recent_game_ids(&pool, now, limit).await;
        let actual = bounded_recent_game_ids(&pool, now, limit).await;
        assert_eq!(
            actual, expected,
            "candidate order diverged at limit {limit}"
        );
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn recent_games_subsecond_edges_cannot_omit_the_precise_winner() {
    let pool = recent_games_test_pool().await;
    let now = Utc
        .with_ymd_and_hms(2026, 8, 26, 12, 0, 0)
        .single()
        .expect("valid fixture timestamp");
    install_candidate_indexes(&pool).await;

    sqlx::raw_sql(
        r#"INSERT INTO "Games" VALUES
            (1, 'ended 900ms', '', NULL, 0, FALSE,
             '2026-08-26 11:00:00+00', '2026-08-26 11:59:59.100+00'),
            (5, 'ended 500ms', '', NULL, 0, FALSE,
             '2026-08-26 11:00:00+00', '2026-08-26 11:59:59.500+00'),
            (10, 'ended 100ms', '', NULL, 0, FALSE,
             '2026-08-26 11:00:00+00', '2026-08-26 11:59:59.900+00')"#,
    )
    .execute(&pool)
    .await
    .expect("insert ended sub-second fixtures");
    assert_recent_game_ids(&pool, now, 1, &[10]).await;
    assert_recent_game_ids(&pool, now, 2, &[10, 5]).await;

    sqlx::raw_sql(
        r#"TRUNCATE "Games";
        INSERT INTO "Games" VALUES
            (2, 'upcoming 900ms', '', NULL, 0, FALSE,
             '2026-08-26 12:00:00.900+00', '2026-08-27 12:00:00+00'),
            (15, 'upcoming 500ms', '', NULL, 0, FALSE,
             '2026-08-26 12:00:00.500+00', '2026-08-27 12:00:00+00'),
            (20, 'upcoming 100ms', '', NULL, 0, FALSE,
             '2026-08-26 12:00:00.100+00', '2026-08-27 12:00:00+00')"#,
    )
    .execute(&pool)
    .await
    .expect("insert upcoming sub-second fixtures");
    assert_recent_game_ids(&pool, now, 1, &[20]).await;
    assert_recent_game_ids(&pool, now, 2, &[20, 15]).await;

    sqlx::raw_sql(
        r#"TRUNCATE "Games";
        INSERT INTO "Games" VALUES
            (3, 'active start 900ms', '', NULL, 0, FALSE,
             '2026-08-26 11:59:59.100+00', '2026-08-27 12:00:00+00'),
            (25, 'active start 500ms', '', NULL, 0, FALSE,
             '2026-08-26 11:59:59.500+00', '2026-08-27 12:00:00+00'),
            (30, 'active start 100ms', '', NULL, 0, FALSE,
             '2026-08-26 11:59:59.900+00', '2026-08-27 12:00:00+00')"#,
    )
    .execute(&pool)
    .await
    .expect("insert active-start sub-second fixtures");
    assert_recent_game_ids(&pool, now, 1, &[30]).await;
    assert_recent_game_ids(&pool, now, 2, &[30, 25]).await;

    sqlx::raw_sql(
        r#"TRUNCATE "Games";
        INSERT INTO "Games" VALUES
            (4, 'active end 900ms', '', NULL, 0, FALSE,
             '2026-08-25 12:00:00+00', '2026-08-26 12:00:00.900+00'),
            (35, 'active end 500ms', '', NULL, 0, FALSE,
             '2026-08-25 12:00:00+00', '2026-08-26 12:00:00.500+00'),
            (40, 'active end 100ms', '', NULL, 0, FALSE,
             '2026-08-25 12:00:00+00', '2026-08-26 12:00:00.100+00')"#,
    )
    .execute(&pool)
    .await
    .expect("insert active-end sub-second fixtures");
    assert_recent_game_ids(&pool, now, 1, &[40]).await;
    assert_recent_game_ids(&pool, now, 2, &[40, 35]).await;

    sqlx::raw_sql(
        r#"TRUNCATE "Games";
        INSERT INTO "Games" VALUES
            (9, 'ended tie low id', '', NULL, 0, FALSE,
             '2026-08-25 12:00:00+00', '2026-08-26 11:59:59.750+00'),
            (90, 'ended tie high id', '', NULL, 0, FALSE,
             '2026-08-25 12:00:00+00', '2026-08-26 11:59:59.750+00'),
            (50, 'upcoming tie', '', NULL, 0, FALSE,
             '2026-08-26 12:00:00.250+00', '2026-08-27 12:00:00+00'),
            (70, 'active tie', '', NULL, 0, FALSE,
             '2026-08-26 11:59:59.750+00', '2026-08-27 12:00:00+00')"#,
    )
    .execute(&pool)
    .await
    .expect("insert exact-distance tie fixtures");
    assert_recent_game_ids(&pool, now, 1, &[9]).await;
    assert_recent_game_ids(&pool, now, 4, &[9, 50, 70, 90]).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn recent_games_explain_keeps_candidate_and_sort_rows_bounded_as_games_grow() {
    const LIMIT: i64 = 7;
    const INDEXES: [&str; 4] = [
        "ix_games_visible_ended_edge",
        "ix_games_visible_upcoming_edge",
        "ix_games_visible_active_start_edge",
        "ix_games_visible_active_end_edge",
    ];
    let pool = recent_games_test_pool().await;
    let now = Utc
        .with_ymd_and_hms(2026, 8, 26, 12, 0, 0)
        .single()
        .expect("valid fixture timestamp");

    sqlx::raw_sql(
        r#"INSERT INTO "Games"
        SELECT n, 'near past', '', NULL, 0, FALSE,
               '2026-08-26 11:00:00+00'::timestamptz,
               '2026-08-26 12:00:00+00'::timestamptz - make_interval(secs => n)
          FROM generate_series(1, 100) AS n;
        INSERT INTO "Games"
        SELECT 1000 + n, 'near future', '', NULL, 0, FALSE,
               '2026-08-26 12:00:00+00'::timestamptz + make_interval(secs => n),
               '2026-08-27 12:00:00+00'::timestamptz
          FROM generate_series(1, 100) AS n;
        INSERT INTO "Games"
        SELECT 2000 + n, 'near active start', '', NULL, 0, FALSE,
               '2026-08-26 12:00:00+00'::timestamptz - make_interval(secs => n),
               '2026-09-26 12:00:00+00'::timestamptz
          FROM generate_series(1, 100) AS n;
        INSERT INTO "Games"
        SELECT 3000 + n, 'near active end', '', NULL, 0, FALSE,
               '2026-07-26 12:00:00+00'::timestamptz,
               '2026-08-26 12:00:00+00'::timestamptz + make_interval(secs => n)
          FROM generate_series(1, 100) AS n;"#,
    )
    .execute(&pool)
    .await
    .expect("insert near-edge fixtures");
    install_candidate_indexes(&pool).await;

    grow_historical_games(&pool, now, 10_000, 2_000).await;
    grow_future_games(&pool, now, 110_000, 2_000).await;
    grow_active_games(&pool, now, 210_000, 2_000).await;
    let baseline = analyze_and_explain(&pool, now, LIMIT).await;

    grow_historical_games(&pool, now, 20_000, 30_000).await;
    let historical_scaled = analyze_and_explain(&pool, now, LIMIT).await;
    grow_future_games(&pool, now, 120_000, 30_000).await;
    let future_scaled = analyze_and_explain(&pool, now, LIMIT).await;
    grow_active_games(&pool, now, 220_000, 30_000).await;
    let active_scaled = analyze_and_explain(&pool, now, LIMIT).await;

    for (label, plan) in [
        ("baseline", &baseline),
        ("historical-scaled", &historical_scaled),
        ("future-scaled", &future_scaled),
        ("active-scaled", &active_scaled),
    ] {
        assert!(!plan.contains("Seq Scan on Games"), "{label} plan:\n{plan}");
        for index in INDEXES {
            assert!(plan.contains(index), "{label} missed {index}:\n{plan}");
            let rows = plan
                .lines()
                .find(|line| line.contains(index))
                .and_then(actual_rows)
                .expect("candidate index scan exposes actual rows");
            assert!(rows <= LIMIT as u64, "{label} {index} returned {rows} rows");
        }
        for (line, rows) in plan
            .lines()
            .filter(|line| line.contains("Sort"))
            .filter_map(|line| actual_rows(line).map(|rows| (line, rows)))
        {
            assert!(
                rows <= (LIMIT * 4) as u64,
                "{label} sort consumed {rows} rows at {line}:\n{plan}"
            );
        }
        assert!(
            plan_counter(plan, "Rows Removed by Filter:") <= (LIMIT * 2) as u64,
            "{label} filtered too many index candidates:\n{plan}"
        );
    }

    eprintln!(
        concat!(
            "recent-games EXPLAIN evidence: shared hits baseline={} historical={} ",
            "future={} active={}\nactive-scaled plan:\n{}"
        ),
        plan_counter(&baseline, "Buffers: shared hit="),
        plan_counter(&historical_scaled, "Buffers: shared hit="),
        plan_counter(&future_scaled, "Buffers: shared hit="),
        plan_counter(&active_scaled, "Buffers: shared hit="),
        active_scaled
    );
}
