use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use super::{load_pending_challenges, PENDING_CHALLENGES_SQL, PENDING_CHALLENGE_LIMIT};
use crate::utils::enums::ChallengeReviewStatus;

#[test]
fn pending_projection_is_bounded_and_selects_only_review_columns() {
    assert!(PENDING_CHALLENGES_SQL.contains("LIMIT $3"));
    assert!(PENDING_CHALLENGES_SQL.contains("ORDER BY challenge.review_status ASC"));
    assert!(PENDING_CHALLENGES_SQL.contains("challenge.id DESC"));
    assert!(!PENDING_CHALLENGES_SQL.contains("challenge.*"));
    assert!(!PENDING_CHALLENGES_SQL.contains("workload_spec"));
    assert_eq!(PENDING_CHALLENGE_LIMIT, 200);
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn pending_projection_returns_the_newest_bounded_page_with_submitter_names() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect test database");
    let schema = format!("pending_review_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .expect("create isolated schema");
    let options = PgConnectOptions::from_str(&database_url)
        .expect("parse database URL")
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .expect("connect isolated schema");

    sqlx::raw_sql(
        r#"
        CREATE TABLE "AspNetUsers" (id UUID PRIMARY KEY, user_name TEXT);
        CREATE TABLE "GameChallenges" (
          id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, title TEXT NOT NULL,
          category SMALLINT NOT NULL, "Type" SMALLINT NOT NULL,
          review_status SMALLINT NOT NULL, review_note TEXT,
          submitted_at_utc TIMESTAMPTZ, reviewed_at_utc TIMESTAMPTZ,
          submitted_by_user_id UUID
        );
        "#,
    )
    .execute(&pool)
    .await
    .expect("create review fixture tables");

    let submitter = uuid::Uuid::new_v4();
    sqlx::query(r#"INSERT INTO "AspNetUsers" (id, user_name) VALUES ($1, 'author')"#)
        .bind(submitter)
        .execute(&pool)
        .await
        .unwrap();
    // 210 pending rows submitted one minute apart, then a rejected row, an
    // active row, and a pending row that belongs to another game.
    sqlx::query(
        r#"INSERT INTO "GameChallenges"
             (id, game_id, title, category, "Type", review_status, submitted_at_utc,
              submitted_by_user_id)
           SELECT n, 1, 'Pending ' || n, 0, 0, $1,
                  TIMESTAMPTZ '2026-01-01 00:00:00+00' + make_interval(mins => n),
                  CASE WHEN n = 210 THEN $2 ELSE NULL END
             FROM generate_series(1, 210) AS n"#,
    )
    .bind(ChallengeReviewStatus::Pending as i16)
    .bind(submitter)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "GameChallenges"
             (id, game_id, title, category, "Type", review_status, review_note,
              submitted_at_utc, reviewed_at_utc)
           VALUES
             (900, 1, 'Rejected', 0, 0, $1, 'no', TIMESTAMPTZ '2026-02-01 00:00:00+00',
              TIMESTAMPTZ '2026-02-02 00:00:00+00'),
             (901, 1, 'Active', 0, 0, $2, NULL, TIMESTAMPTZ '2026-02-01 00:00:00+00', NULL),
             (902, 2, 'Other game', 0, 0, $3, NULL, TIMESTAMPTZ '2026-03-01 00:00:00+00', NULL)"#,
    )
    .bind(ChallengeReviewStatus::Rejected as i16)
    .bind(ChallengeReviewStatus::Active as i16)
    .bind(ChallengeReviewStatus::Pending as i16)
    .execute(&pool)
    .await
    .unwrap();

    let rows = load_pending_challenges(&pool, 1).await.unwrap();
    assert_eq!(rows.len(), PENDING_CHALLENGE_LIMIT as usize);
    assert!(rows
        .iter()
        .all(|row| row.review_status == ChallengeReviewStatus::Pending));
    let ids: Vec<i32> = rows.iter().map(|row| row.id).collect();
    let expected: Vec<i32> = (11..=210).rev().collect();
    assert_eq!(ids, expected, "newest pending submissions come first");
    assert_eq!(rows[0].submitted_by_user_id, Some(submitter));
    assert_eq!(rows[0].submitted_by_user_name.as_deref(), Some("author"));
    assert_eq!(rows[1].submitted_by_user_name, None);
    assert!(!ids.contains(&901), "active rows are not review rows");
    assert!(!ids.contains(&902), "other games are excluded");

    // With room left under the bound, rejected rows follow the pending ones.
    sqlx::query(r#"DELETE FROM "GameChallenges" WHERE id <= 200"#)
        .execute(&pool)
        .await
        .unwrap();
    let rows = load_pending_challenges(&pool, 1).await.unwrap();
    let ids: Vec<i32> = rows.iter().map(|row| row.id).collect();
    let mut expected: Vec<i32> = (201..=210).rev().collect();
    expected.push(900);
    assert_eq!(ids, expected);
    assert_eq!(rows[10].review_status, ChallengeReviewStatus::Rejected);
    assert_eq!(rows[10].review_note.as_deref(), Some("no"));
    assert!(rows[10].reviewed_at_utc.is_some());

    pool.close().await;
    assert!(schema.starts_with("pending_review_"));
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .expect("drop isolated schema");
    admin.close().await;
}
