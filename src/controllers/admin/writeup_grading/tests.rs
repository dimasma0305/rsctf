use super::*;

#[test]
fn writeup_grade_validates_boundaries_and_wire_contract() {
    for percentage in [None, Some(0), Some(100)] {
        assert!(SaveGrade {
            percentage,
            expected_revision: 0,
            operation_id: Uuid::new_v4()
        }
        .validate()
        .is_ok());
    }
    for percentage in [-1, 101] {
        assert!(SaveGrade {
            percentage: Some(percentage),
            expected_revision: 0,
            operation_id: Uuid::new_v4()
        }
        .validate()
        .is_err());
    }
    assert!(serde_json::from_value::<SaveGrade>(
        serde_json::json!({"percentage": 2.5, "expectedRevision": 0, "operationId": Uuid::new_v4()})
    )
    .is_err());
    let wire = serde_json::to_value(Grade {
        participation_id: 1,
        challenge_id: 2,
        percentage: Some(0),
        revision: 1,
    })
    .unwrap();
    assert_eq!(
        wire,
        serde_json::json!({"participationId":1,"challengeId":2,"percentage":0,"revision":1})
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn writeup_grade_postgres_retry_conflict_scope_and_constraints() {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&std::env::var("RSCTF_TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    // A dedicated connection's temporary tables cannot affect any existing event.
    sqlx::raw_sql(r#"
        CREATE TEMP TABLE "Games" (id integer primary key);
        CREATE TEMP TABLE "Participations" (id integer primary key, game_id integer, status smallint);
        CREATE TEMP TABLE "GameChallenges" (id integer primary key, game_id integer);
        CREATE TEMP TABLE "AspNetUsers" (id uuid primary key, role smallint, user_name text, security_stamp text);
        INSERT INTO "Games" VALUES (1),(2);
        INSERT INTO "Participations" VALUES (10,1,1),(20,2,1),(30,1,0),(40,1,2),(50,1,3);
        INSERT INTO "GameChallenges" VALUES (100,1),(200,2);
    "#).execute(&pool).await.unwrap();
    // Keep the migration itself isolated, including its indexes and foreign keys.
    let migration = crate::migrations::m0347_writeup_grades::UP_SQL.replace(
        "CREATE TABLE IF NOT EXISTS",
        "CREATE TEMP TABLE IF NOT EXISTS",
    );
    sqlx::raw_sql(&migration).execute(&pool).await.unwrap();
    sqlx::raw_sql(&migration).execute(&pool).await.unwrap();
    let admin = Uuid::new_v4();
    sqlx::query(r#"INSERT INTO "AspNetUsers" (id) VALUES ($1)"#)
        .bind(admin)
        .execute(&pool)
        .await
        .unwrap();
    let mut model = SaveGrade {
        percentage: Some(80),
        expected_revision: 0,
        operation_id: Uuid::new_v4(),
    };
    assert_eq!(
        persist_grade(&pool, (1, 10, 100), admin, &model)
            .await
            .unwrap()
            .revision,
        1
    );
    assert_eq!(
        persist_grade(&pool, (1, 10, 100), admin, &model)
            .await
            .unwrap()
            .revision,
        1
    );
    model.operation_id = Uuid::new_v4();
    assert!(persist_grade(&pool, (1, 10, 100), admin, &model)
        .await
        .is_err());
    model.expected_revision = 1;
    model.percentage = Some(0);
    assert_eq!(
        persist_grade(&pool, (1, 10, 100), admin, &model)
            .await
            .unwrap()
            .percentage,
        Some(0)
    );
    model.expected_revision = 0;
    for ids in [
        (1, 20, 100),
        (1, 10, 200),
        (1, 30, 100),
        (1, 40, 100),
        (1, 50, 100),
        (2, 10, 100),
    ] {
        assert!(persist_grade(&pool, ids, admin, &model).await.is_err());
    }
    model.expected_revision = 2;
    model.operation_id = Uuid::new_v4();
    model.percentage = None;
    assert_eq!(
        persist_grade(&pool, (1, 10, 100), admin, &model)
            .await
            .unwrap()
            .percentage,
        None
    );
    assert!(sqlx::query(r#"UPDATE "WriteupGrades" SET percentage=101"#)
        .execute(&pool)
        .await
        .is_err());
    let count: i64 = sqlx::query_scalar(r#"SELECT count(*) FROM "WriteupGrades""#)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    test_private_routes(&pool).await;
    pool.close().await;
}

async fn test_private_routes(pool: &sqlx::PgPool) {
    use crate::{
        app_state::AppState,
        models::internal::configs::AppConfig,
        services::{cache::InMemoryCache, container::NoopContainerManager, token::TokenService},
        storage::LocalBlobStorage,
        utils::enums::Role,
    };
    use axum::{body::Body, http::Request};
    use sea_orm::ActiveEnum;
    use std::sync::Arc;
    use tower::ServiceExt;
    let state = AppState::new(
        sea_orm::SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone()),
        Arc::new(AppConfig::default()),
        Arc::new(InMemoryCache::new()),
        Arc::new(LocalBlobStorage::new(
            std::env::temp_dir().join("rsctf-writeup-grading-no-blobs"),
        )),
        TokenService::new("test-writeup-grading-key-not-a-secret", 60),
        Arc::new(NoopContainerManager),
    );
    for role in [
        None,
        Some(Role::User),
        Some(Role::Monitor),
        Some(Role::Banned),
    ] {
        let token = if let Some(role) = role {
            let user_id = Uuid::new_v4();
            sqlx::query(r#"INSERT INTO "AspNetUsers" (id,role,user_name,security_stamp) VALUES ($1,$2,'test','stamp')"#)
                .bind(user_id).bind(role.into_value()).execute(pool).await.unwrap();
            Some(state.token.issue(user_id, role, "test", "stamp").unwrap())
        } else {
            None
        };
        for (method, path) in [
            ("GET", "/api/admin/writeups/1/grading"),
            ("PUT", "/api/admin/writeups/1/grading/10/100"),
        ] {
            let mut request = Request::builder()
                .method(method)
                .uri(path)
                .header("Content-Type", "application/json");
            if let Some(token) = &token {
                request = request.header("Authorization", format!("Bearer {token}"));
            }
            let response = crate::controllers::admin::router()
                .with_state(state.clone())
                .oneshot(request.body(Body::from("{}")).unwrap())
                .await
                .unwrap();
            assert!(
                [401, 403].contains(&response.status().as_u16()),
                "{role:?} {method} {}",
                response.status()
            );
        }
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn writeup_grade_complete_event_keeps_official_points_unchanged() {
    use crate::{
        app_state::AppState,
        models::internal::configs::AppConfig,
        services::{cache::InMemoryCache, container::NoopContainerManager, token::TokenService},
        storage::LocalBlobStorage,
    };
    use std::sync::Arc;
    let fixture = crate::services::ad::scoring::test_fixture::ad_scoring_fixture().await;
    let pool = &fixture.pool;
    // Add one real Jeopardy solve beside the fixture's finalized A&D epoch.
    sqlx::raw_sql(r#"
        INSERT INTO "GameChallenges"
        SELECT (jsonb_populate_record(NULL::"GameChallenges", to_jsonb(c) ||
            '{"id":900032,"title":"Writeup Jeopardy","Type":0,"ad_self_hosted":false}')).*
          FROM "GameChallenges" c WHERE id=900031;
        INSERT INTO "Submissions" (id,answer,status,submit_time_utc,team_id,participation_id,game_id,challenge_id)
        VALUES (900051,'fixture',1,now()-interval '1 hour',900011,900021,900001,900032);
        INSERT INTO "FirstSolves" (participation_id,challenge_id,submission_id)
        VALUES (900021,900032,900051);
    "#).execute(pool).await.unwrap();
    let state = AppState::new(
        sea_orm::SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone()),
        Arc::new(AppConfig::default()),
        Arc::new(InMemoryCache::new()),
        Arc::new(LocalBlobStorage::new(
            std::env::temp_dir().join("rsctf-writeup-grading-no-blobs"),
        )),
        TokenService::new("test-writeup-grading-key-not-a-secret", 60),
        Arc::new(NoopContainerManager),
    );
    let before = load_board(&state, fixture.game_id).await.unwrap();
    assert_eq!(before.teams.len(), 2);
    let team = before.teams.iter().find(|t| t.team_id == 900011).unwrap();
    assert_eq!(team.challenges.len(), 2);
    assert!(team
        .challenges
        .iter()
        .any(|c| c.mode == GradeMode::AttackDefense && c.earned_points > 0.0));
    assert_eq!(
        team.challenges
            .iter()
            .find(|c| c.challenge_id == 900032)
            .unwrap()
            .earned_points,
        1000.0
    );
    assert!(
        (team
            .challenges
            .iter()
            .map(|c| c.overall_points)
            .sum::<f64>()
            - team.original_score)
            .abs()
            < 1e-9
    );
    let admin = Uuid::parse_str("00000000-0000-0000-0000-000000000011").unwrap();
    let model = SaveGrade {
        percentage: Some(0),
        expected_revision: 0,
        operation_id: Uuid::new_v4(),
    };
    persist_grade(pool, (900001, 900021, 900032), admin, &model)
        .await
        .unwrap();
    let after = load_board(&state, fixture.game_id).await.unwrap();
    let graded = after.teams.iter().find(|t| t.team_id == 900011).unwrap();
    assert_eq!(graded.original_score, team.original_score);
    let challenge = graded
        .challenges
        .iter()
        .find(|c| c.challenge_id == 900032)
        .unwrap();
    assert_eq!(challenge.percentage, Some(0));
    assert_eq!(challenge.earned_points, 1000.0);
    let official = build_combined_scoreboard(
        &state,
        &game::load_game_cached(&state, fixture.game_id)
            .await
            .unwrap(),
        true,
    )
    .await
    .unwrap();
    assert_eq!(
        official
            .items
            .iter()
            .find(|t| t.id == 900011)
            .unwrap()
            .score,
        team.original_score
    );
    drop(state);
    fixture.cleanup().await;
}
