use super::*;

fn find_plan_node_by_index<'a>(
    node: &'a serde_json::Value,
    index_name: &str,
) -> Option<&'a serde_json::Value> {
    if node.get("Index Name").and_then(serde_json::Value::as_str) == Some(index_name)
        && node
            .get("Actual Loops")
            .and_then(serde_json::Value::as_f64)
            .is_some_and(|loops| loops > 0.0)
    {
        return Some(node);
    }
    node.get("Plans")
        .and_then(serde_json::Value::as_array)
        .and_then(|plans| {
            plans
                .iter()
                .find_map(|child| find_plan_node_by_index(child, index_name))
        })
}

#[test]
fn review_query_bounds_every_input_before_postgres() {
    let normalized = ParticipationReviewQuery {
        count: u64::MAX,
        skip: u64::MAX,
        status: Some(ParticipationStatus::Accepted),
        division_id: Some(7),
        search: Some("  needle  ".to_owned()),
    }
    .normalized()
    .unwrap();

    assert_eq!(normalized.count, MAX_REVIEW_PAGE_SIZE as i64);
    assert_eq!(normalized.skip, MAX_REVIEW_SKIP as i64);
    assert_eq!(
        normalized.status,
        Some(ParticipationStatus::Accepted as i16)
    );
    assert_eq!(normalized.division_id, Some(7));
    assert_eq!(normalized.search.as_deref(), Some("%needle%"));

    let literal_pattern = ParticipationReviewQuery {
        search: Some("  100%_\\  ".to_owned()),
        ..ParticipationReviewQuery::default()
    }
    .normalized()
    .unwrap();
    assert_eq!(literal_pattern.search.as_deref(), Some("%100\\%\\_\\\\%"));

    let minimum = ParticipationReviewQuery {
        count: 0,
        ..ParticipationReviewQuery::default()
    }
    .normalized()
    .unwrap();
    assert_eq!(minimum.count, 1);

    assert!(ParticipationReviewQuery {
        search: Some("x".repeat(MAX_REVIEW_SEARCH_CHARS + 1)),
        ..ParticipationReviewQuery::default()
    }
    .normalized()
    .is_err());
    assert!(ParticipationReviewQuery {
        division_id: Some(0),
        ..ParticipationReviewQuery::default()
    }
    .normalized()
    .is_err());
}

#[test]
fn list_projection_is_compact_and_contains_no_member_pii_fields() {
    let value = serde_json::to_value(ParticipationReviewSummaryModel {
        id: 1,
        team_id: 2,
        team_name: "team".to_owned(),
        team_avatar: None,
        registered_member_count: 3,
        team_member_count: 4,
        division_id: Some(5),
        status: ParticipationStatus::Pending,
    })
    .unwrap();
    let object = value.as_object().unwrap();

    assert_eq!(object.len(), 8);
    for forbidden in [
        "members",
        "registeredMembers",
        "captainId",
        "email",
        "phone",
        "realName",
        "stdNumber",
        "bio",
        "role",
    ] {
        assert!(!object.contains_key(forbidden), "list leaked {forbidden}");
    }
}

#[test]
fn roster_detail_is_private_and_not_cacheable() {
    let response = private_no_store_detail(ParticipationReviewDetailModel {
        id: 1,
        team_id: 2,
        team_name: "team".to_owned(),
        team_avatar: None,
        members: Vec::new(),
    });

    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("private, no-store")
    );
    assert_eq!(
        response
            .headers()
            .get(header::PRAGMA)
            .and_then(|value| value.to_str().ok()),
        Some("no-cache")
    );
}

#[test]
fn review_sql_keeps_authorization_filters_and_limits_in_one_bound_statement() {
    for predicate in [
        "manager.user_id = $2",
        "$3::boolean",
        "participation.status = $4",
        "participation.division_id = $5",
        "OFFSET $7 LIMIT $8",
    ] {
        assert!(
            PARTICIPATION_REVIEW_PAGE_SQL.contains(predicate),
            "missing SQL boundary: {predicate}"
        );
    }
    assert!(PARTICIPATION_REVIEW_SEARCH_PAGE_SQL.contains("LOWER(team.name) LIKE $6 ESCAPE '\\'"));
    assert!(!PARTICIPATION_REVIEW_SEARCH_PAGE_SQL.contains("STRPOS"));
    assert!(!PARTICIPATION_REVIEW_PAGE_SQL.contains("AspNetUsers"));
    assert!(PARTICIPATION_REVIEW_PAGE_SQL.contains("filtered AS NOT MATERIALIZED"));
    assert!(PARTICIPATION_REVIEW_DETAIL_SQL.contains("participation.id = $4"));
    assert!(PARTICIPATION_REVIEW_DETAIL_SQL.contains("manager.user_id = $2"));
    for forbidden in [
        "password_hash",
        "security_stamp",
        "browser_fingerprint",
        "normalized_email",
        "last_signed_in_utc",
        "role",
        "bio",
    ] {
        assert!(
            !PARTICIPATION_REVIEW_DETAIL_SQL.contains(forbidden),
            "detail query selected unnecessary field {forbidden}"
        );
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn large_event_page_is_indexed_bounded_authorized_filtered_and_pii_minimal() {
    use std::str::FromStr;

    use serde_json::Value;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    sqlx::query("CREATE EXTENSION IF NOT EXISTS pg_trgm WITH SCHEMA public")
        .execute(&admin)
        .await
        .expect("install trusted trigram extension in shared public schema");
    let schema = format!("participation_review_{}", Uuid::new_v4().simple());
    assert!(schema.starts_with("participation_review_"));
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let search_path = format!("{schema},public");
    let options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .options([("search_path", search_path.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await
        .unwrap();

    sqlx::raw_sql(
        r#"
        CREATE TABLE "Games" (id INTEGER PRIMARY KEY);
        CREATE TABLE "GameManagers" (
            id SERIAL PRIMARY KEY,
            game_id INTEGER NOT NULL,
            user_id UUID NOT NULL
        );
        CREATE UNIQUE INDEX ux_gamemanagers_game_user
            ON "GameManagers" (game_id, user_id);
        CREATE TABLE "Teams" (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            avatar_hash TEXT,
            captain_id UUID NOT NULL
        );
        CREATE TABLE "Participations" (
            id INTEGER PRIMARY KEY,
            status SMALLINT NOT NULL,
            game_id INTEGER NOT NULL,
            team_id INTEGER NOT NULL,
            division_id INTEGER
        );
        CREATE UNIQUE INDEX ux_participations_game_team_id
            ON "Participations" (game_id, team_id, id);
        CREATE TABLE "TeamMembers" (
            id SERIAL PRIMARY KEY,
            team_id INTEGER NOT NULL,
            user_id UUID NOT NULL
        );
        CREATE UNIQUE INDEX ux_teammembers_team_user
            ON "TeamMembers" (team_id, user_id);
        CREATE TABLE "UserParticipations" (
            user_id UUID NOT NULL,
            game_id INTEGER NOT NULL,
            team_id INTEGER NOT NULL,
            participation_id INTEGER NOT NULL,
            PRIMARY KEY (user_id, game_id)
        );
        CREATE TABLE "AspNetUsers" (
            id UUID PRIMARY KEY,
            user_name TEXT,
            email TEXT,
            real_name TEXT NOT NULL DEFAULT '',
            std_number TEXT NOT NULL DEFAULT '',
            phone_number TEXT,
            avatar_hash TEXT
        );
        CREATE TABLE "GameChallenges" (
            id INTEGER PRIMARY KEY,
            game_id INTEGER NOT NULL,
            title TEXT NOT NULL
        );
        CREATE TABLE "GameEvents" (
            id INTEGER PRIMARY KEY,
            game_id INTEGER NOT NULL,
            "Type" SMALLINT NOT NULL,
            values JSONB NOT NULL,
            publish_time_utc TIMESTAMPTZ NOT NULL,
            user_id UUID,
            team_id INTEGER NOT NULL
        );
        CREATE TABLE "Submissions" (
            id INTEGER PRIMARY KEY,
            answer TEXT NOT NULL,
            status SMALLINT NOT NULL,
            submit_time_utc TIMESTAMPTZ NOT NULL,
            user_id UUID,
            team_id INTEGER NOT NULL,
            game_id INTEGER NOT NULL,
            challenge_id INTEGER NOT NULL
        );
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(crate::migrations::MONITOR_HISTORY_INDEX_SQL)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(crate::migrations::PARTICIPATION_REVIEW_INDEX_SQL)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(crate::migrations::PARTICIPATION_REVIEW_INDEX_SQL)
        .execute(&pool)
        .await
        .unwrap();

    let manager_id = Uuid::new_v4();
    let outsider_id = Uuid::new_v4();
    sqlx::query(r#"INSERT INTO "Games" (id) VALUES (77), (78)"#)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "GameManagers" (game_id, user_id) VALUES (77, $1)"#)
        .bind(manager_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "GameManagers" (game_id, user_id) VALUES (78, $1)"#)
        .bind(outsider_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "Teams" (id, name, avatar_hash, captain_id)
           SELECT value,
                  'team-' || LPAD(value::text, 5, '0'),
                  CASE WHEN value % 10 = 0 THEN 'avatar-' || value::text ELSE NULL END,
                  LPAD(TO_HEX(value::bigint), 32, '0')::uuid
             FROM GENERATE_SERIES(1, 12000) value"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "Participations" (id, status, game_id, team_id, division_id)
           SELECT value, (value % 5)::smallint, 77, value, (value % 4) + 1
             FROM GENERATE_SERIES(1, 12000) value"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "UserParticipations" (user_id, game_id, team_id, participation_id)
           SELECT LPAD(TO_HEX(value::bigint), 32, '0')::uuid, 77, value, value
             FROM GENERATE_SERIES(1, 12000) value"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(r#"UPDATE "Teams" SET name = 'Special Needle Team' WHERE id = 9999"#)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"UPDATE "Teams" SET name = 'Literal 100%_\ Team' WHERE id = 9998"#)
        .execute(&pool)
        .await
        .unwrap();

    let captain_id = Uuid::parse_str("00000000-0000-0000-0000-00000000270f").unwrap();
    let member_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "AspNetUsers"
              (id, user_name, email, real_name, std_number, phone_number, avatar_hash)
           VALUES
              ($1, 'captain', 'captain-secret@example.test', 'Captain Secret', 'STD-SECRET',
               '+62000001', 'captain-avatar'),
              ($2, 'member', 'member-secret@example.test', 'Member Secret', 'STD-MEMBER',
               '+62000002', NULL)"#,
    )
    .bind(captain_id)
    .bind(member_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES (9999, $1)"#)
        .bind(member_id)
        .execute(&pool)
        .await
        .unwrap();
    // m0107 builds this index over existing production teams. Flush the GIN
    // pending list so the fixture models that post-migration state instead of
    // an artificial bulk insert performed after index creation.
    sqlx::query(r#"VACUUM (ANALYZE) "Teams""#)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("ANALYZE").execute(&pool).await.unwrap();

    let first_page = participation_review_page(
        &pool,
        manager_id,
        false,
        77,
        &ParticipationReviewQuery::default(),
    )
    .await
    .unwrap()
    .expect("event manager is authorized");
    assert_eq!(first_page.total, 12000);
    assert_eq!(first_page.length, DEFAULT_REVIEW_PAGE_SIZE as usize);
    assert_eq!(first_page.data.len(), DEFAULT_REVIEW_PAGE_SIZE as usize);
    let first_page_json = serde_json::to_vec(&first_page).unwrap();
    assert!(
        first_page_json.len() < 8_000,
        "page response grew unexpectedly"
    );
    let first_page_text = String::from_utf8(first_page_json).unwrap();
    for pii in [
        "captain-secret@example.test",
        "member-secret@example.test",
        "Captain Secret",
        "+62000001",
        "STD-SECRET",
    ] {
        assert!(!first_page_text.contains(pii), "list response leaked {pii}");
    }

    let search_page = participation_review_page(
        &pool,
        manager_id,
        false,
        77,
        &ParticipationReviewQuery {
            search: Some(" needle ".to_owned()),
            ..ParticipationReviewQuery::default()
        },
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(search_page.total, 1);
    assert_eq!(search_page.data[0].id, 9999);
    assert_eq!(search_page.data[0].registered_member_count, 1);
    assert_eq!(search_page.data[0].team_member_count, 2);
    for literal in ["%", "_", "\\"] {
        let literal_page = participation_review_page(
            &pool,
            manager_id,
            false,
            77,
            &ParticipationReviewQuery {
                search: Some(literal.to_owned()),
                ..ParticipationReviewQuery::default()
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(literal_page.total, 1, "{literal:?} stays literal");
        assert_eq!(literal_page.data[0].id, 9998);
    }

    let empty_managed_event = participation_review_page(
        &pool,
        outsider_id,
        false,
        78,
        &ParticipationReviewQuery::default(),
    )
    .await
    .unwrap()
    .expect("an authorized empty event is not an authorization failure");
    assert_eq!(empty_managed_event.total, 0);
    assert!(empty_managed_event.data.is_empty());

    let filtered = ParticipationReviewQuery {
        count: 7,
        status: Some(ParticipationStatus::Accepted),
        division_id: Some(2),
        ..ParticipationReviewQuery::default()
    };
    let filtered_page = participation_review_page(&pool, manager_id, false, 77, &filtered)
        .await
        .unwrap()
        .unwrap();
    let expected_total: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)::bigint FROM "Participations"
            WHERE game_id = 77 AND status = 1 AND division_id = 2"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(expected_total, 600, "the 12k fixture shape changed");
    assert_eq!(filtered_page.total, expected_total);
    assert_eq!(filtered_page.length, 7);
    assert!(filtered_page
        .data
        .iter()
        .all(|row| { row.status == ParticipationStatus::Accepted && row.division_id == Some(2) }));

    assert!(participation_review_page(
        &pool,
        outsider_id,
        false,
        77,
        &ParticipationReviewQuery::default(),
    )
    .await
    .unwrap()
    .is_none());
    assert!(participation_review_page(
        &pool,
        outsider_id,
        true,
        77,
        &ParticipationReviewQuery::default(),
    )
    .await
    .unwrap()
    .is_some());

    assert!(
        participation_review_detail(&pool, outsider_id, false, 77, 9999)
            .await
            .unwrap()
            .is_none()
    );
    let detail = participation_review_detail(&pool, manager_id, false, 77, 9999)
        .await
        .unwrap()
        .expect("manager can open one roster");
    assert_eq!(detail.members.len(), 2);
    assert!(detail
        .members
        .iter()
        .any(|member| member.is_captain && member.is_registered));
    assert!(detail
        .members
        .iter()
        .any(|member| !member.is_captain && !member.is_registered));
    let detail_text = serde_json::to_string(&detail).unwrap();
    assert!(detail_text.contains("captain-secret@example.test"));
    for forbidden_key in [
        "passwordHash",
        "securityStamp",
        "browserFingerprint",
        "normalizedEmail",
        "lastSignedInUtc",
        "role",
        "bio",
    ] {
        assert!(!detail_text.contains(forbidden_key));
    }

    let explain_sql =
        format!("EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) {PARTICIPATION_REVIEW_PAGE_SQL}");
    let plan: Value = sqlx::query_scalar(&explain_sql)
        .bind(77_i32)
        .bind(manager_id)
        .bind(false)
        .bind(Some(ParticipationStatus::Accepted as i16))
        .bind(Some(2_i32))
        .bind(None::<&str>)
        .bind(0_i64)
        .bind(10_i64)
        .fetch_one(&pool)
        .await
        .unwrap();
    let plan_text = serde_json::to_string(&plan).unwrap();
    assert!(
        plan_text.contains("ix_participations_review_filter"),
        "large-event plan did not use the review filter index: {plan_text}"
    );
    let root = plan
        .get(0)
        .and_then(|entry| entry.get("Plan"))
        .expect("EXPLAIN JSON contains a root plan");
    assert_eq!(
        root.get("Actual Rows").and_then(Value::as_f64),
        Some(10.0),
        "the filtered query must return exactly one bounded page: {plan_text}"
    );
    let filter_index = find_plan_node_by_index(root, "ix_participations_review_filter")
        .expect("the filter index node is present");
    assert_eq!(
        filter_index.get("Actual Loops").and_then(Value::as_f64),
        Some(1.0),
        "the participation index must be scanned once: {plan_text}"
    );
    assert_eq!(
        filter_index.get("Actual Rows").and_then(Value::as_f64),
        Some(expected_total as f64),
        "the index must read exactly the matching cohort: {plan_text}"
    );
    let registration_index = find_plan_node_by_index(root, "ix_userparticipations_review_count")
        .expect("the per-page registration count uses its covering index");
    assert_eq!(
        registration_index
            .get("Actual Loops")
            .and_then(Value::as_f64),
        Some(10.0),
        "registration lookup work must stay proportional to the returned page: {plan_text}"
    );
    assert_eq!(
        registration_index
            .get("Actual Rows")
            .and_then(Value::as_f64),
        Some(1.0),
        "each per-page registration lookup must return the exact matching row: {plan_text}"
    );

    let selective_search = ParticipationReviewQuery {
        search: Some("needle".to_owned()),
        ..ParticipationReviewQuery::default()
    }
    .normalized()
    .unwrap()
    .search
    .expect("selective search has a normalized LIKE pattern");
    let search_explain_sql =
        format!("EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) {PARTICIPATION_REVIEW_SEARCH_PAGE_SQL}");
    let search_plan: Value = sqlx::query_scalar(&search_explain_sql)
        .bind(77_i32)
        .bind(manager_id)
        .bind(false)
        .bind(None::<i16>)
        .bind(None::<i32>)
        .bind(Some(selective_search.as_str()))
        .bind(0_i64)
        .bind(10_i64)
        .fetch_one(&pool)
        .await
        .unwrap();
    let search_plan_text = serde_json::to_string(&search_plan).unwrap();
    let search_root = search_plan
        .get(0)
        .and_then(|entry| entry.get("Plan"))
        .expect("search EXPLAIN JSON contains a root plan");
    assert_eq!(
        search_root.get("Actual Rows").and_then(Value::as_f64),
        Some(1.0),
        "selective search must return only its bounded matching page: {search_plan_text}"
    );
    let trigram_index = find_plan_node_by_index(search_root, "ix_teams_monitor_name_trgm")
        .unwrap_or_else(|| {
            panic!(
                "selective contains search did not use the team trigram index: {search_plan_text}"
            )
        });
    assert_eq!(
        trigram_index.get("Actual Loops").and_then(Value::as_f64),
        Some(1.0),
        "the trigram index must be scanned once: {search_plan_text}"
    );
    assert_eq!(
        trigram_index.get("Actual Rows").and_then(Value::as_f64),
        Some(1.0),
        "the selective trigram lookup must return one team: {search_plan_text}"
    );

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
