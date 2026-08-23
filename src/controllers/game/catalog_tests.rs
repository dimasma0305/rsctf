use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;
use tower::ServiceExt;

use super::*;

#[tokio::test]
async fn game_list_query_accepts_compact_search_and_membership_filters() {
    let app = Router::new().route(
        "/",
        get(|Query(query): Query<GameListQuery>| async move {
            if query.count == 12
                && query.skip == 3
                && query.search.as_deref() == Some("TECHCOMFEST")
                && query.membership == GameMembershipFilter::Joined
            {
                StatusCode::NO_CONTENT
            } else {
                StatusCode::BAD_REQUEST
            }
        }),
    );
    let response = app
        .oneshot(
            Request::builder()
                .uri("/?count=12&skip=3&search=TECHCOMFEST&membership=joined")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[test]
fn catalog_search_is_trimmed_bounded_and_optional() {
    assert_eq!(normalized_catalog_search(None).unwrap(), None);
    assert_eq!(normalized_catalog_search(Some("   ")).unwrap(), None);
    assert_eq!(
        normalized_catalog_search(Some("  TECHCOMFEST  ")).unwrap(),
        Some("TECHCOMFEST".to_owned())
    );
    assert!(matches!(
        normalized_catalog_search(Some(&"x".repeat(101))),
        Err(AppError::BadRequest(message)) if message.contains("100 characters")
    ));
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn challenge_catalog_cannot_escape_join_start_visibility_or_division_boundaries() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect test database");
    let schema = format!("challenge_catalog_{}", Uuid::new_v4().simple());
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
        CREATE TABLE "Games" (
          id INTEGER PRIMARY KEY, title TEXT NOT NULL, hidden BOOLEAN NOT NULL,
          start_time_utc TIMESTAMPTZ NOT NULL, end_time_utc TIMESTAMPTZ NOT NULL
        );
        CREATE TABLE "GameChallenges" (
          id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, title TEXT NOT NULL,
          category SMALLINT NOT NULL, "Type" SMALLINT NOT NULL,
          original_score INTEGER NOT NULL, min_score_rate DOUBLE PRECISION NOT NULL,
          difficulty DOUBLE PRECISION NOT NULL, accepted_count INTEGER NOT NULL,
          score_curve SMALLINT NOT NULL, is_enabled BOOLEAN NOT NULL,
          review_status SMALLINT NOT NULL
        );
        CREATE TABLE "Participations" (
          id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, team_id INTEGER NOT NULL,
          status SMALLINT NOT NULL, division_id INTEGER NULL
        );
        CREATE TABLE "UserParticipations" (
          user_id UUID NOT NULL, game_id INTEGER NOT NULL, team_id INTEGER NOT NULL,
          participation_id INTEGER NOT NULL, PRIMARY KEY (user_id, game_id)
        );
        CREATE TABLE "Divisions" (
          id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, default_permissions INTEGER NOT NULL
        );
        CREATE TABLE "DivisionChallengeConfigs" (
          division_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
          permissions INTEGER NOT NULL, PRIMARY KEY (division_id, challenge_id)
        );
        CREATE TABLE "Submissions" (
          participation_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
          status SMALLINT NOT NULL
        );
        "#,
    )
    .execute(&pool)
    .await
    .expect("create catalog fixture tables");

    let player = Uuid::new_v4();
    let other = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "Games" VALUES
          (1, 'Joined event', FALSE, clock_timestamp() - interval '1 hour', clock_timestamp() + interval '1 day'),
          (2, 'Other event', FALSE, clock_timestamp() - interval '1 hour', clock_timestamp() + interval '1 day'),
          (3, 'Pending event', FALSE, clock_timestamp() - interval '1 hour', clock_timestamp() + interval '1 day'),
          (4, 'Future event', FALSE, clock_timestamp() + interval '1 hour', clock_timestamp() + interval '1 day'),
          (5, 'Hidden event', TRUE, clock_timestamp() - interval '1 hour', clock_timestamp() + interval '1 day'),
          (6, 'Denied division', FALSE, clock_timestamp() - interval '1 hour', clock_timestamp() + interval '1 day'),
          (7, 'Allowed division', FALSE, clock_timestamp() - interval '1 hour', clock_timestamp() + interval '1 day')"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "GameChallenges" VALUES
          (101, 1, 'Visible Web', 3, 0, 1000, 0.01, 5, 2, 0, TRUE, 0),
          (201, 2, 'Other Crypto', 1, 0, 1000, 0.01, 5, 0, 0, TRUE, 0),
          (301, 3, 'Pending Pwn', 2, 0, 1000, 0.01, 5, 0, 0, TRUE, 0),
          (401, 4, 'Future Reverse', 4, 0, 1000, 0.01, 5, 0, 0, TRUE, 0),
          (501, 5, 'Hidden Forensics', 6, 0, 1000, 0.01, 5, 0, 0, TRUE, 0),
          (601, 6, 'Denied Mobile', 8, 0, 1000, 0.01, 5, 0, 0, TRUE, 0),
          (701, 7, 'Allowed Blockchain', 5, 3, 1000, 0.01, 5, 0, 0, TRUE, 0)"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "Participations" VALUES
          (11, 1, 11, 1, NULL), (22, 2, 22, 1, NULL), (33, 3, 33, 0, NULL),
          (44, 4, 44, 1, NULL), (55, 5, 55, 1, NULL),
          (66, 6, 66, 1, 60), (77, 7, 77, 1, 70)"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "UserParticipations" VALUES
          ($1, 1, 11, 11), ($2, 2, 22, 22), ($1, 3, 33, 33),
          ($1, 4, 44, 44), ($1, 5, 55, 55), ($1, 6, 66, 66), ($1, 7, 77, 77)"#,
    )
    .bind(player)
    .bind(other)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "Divisions" VALUES (60, 6, 0), (70, 7, 256)"#)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "Submissions" VALUES (11, 101, 1)"#)
        .execute(&pool)
        .await
        .unwrap();

    let query = ChallengeCatalogQuery {
        count: 50,
        ..Default::default()
    };
    let (items, total) = load_challenge_catalog(&pool, player, &query).await.unwrap();
    assert_eq!(total, 2);
    assert_eq!(
        items.iter().map(|item| item.id).collect::<Vec<_>>(),
        vec![701, 101]
    );
    assert!(items.iter().find(|item| item.id == 101).unwrap().solved);
    assert_eq!(items.iter().find(|item| item.id == 101).unwrap().score, 820);

    let (other_items, _) = load_challenge_catalog(&pool, other, &query).await.unwrap();
    assert_eq!(
        other_items.iter().map(|item| item.id).collect::<Vec<_>>(),
        vec![201]
    );

    let unsolved = ChallengeCatalogQuery {
        count: 50,
        solved: Some(false),
        category: Some(ChallengeCategory::Blockchain),
        challenge_type: Some(ChallengeType::DynamicContainer),
        ..Default::default()
    };
    let (unsolved_items, total) = load_challenge_catalog(&pool, player, &unsolved)
        .await
        .unwrap();
    assert_eq!(total, 1);
    assert_eq!(unsolved_items[0].id, 701);

    pool.close().await;
    assert!(schema.starts_with("challenge_catalog_"));
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .expect("drop isolated schema");
    admin.close().await;
}
