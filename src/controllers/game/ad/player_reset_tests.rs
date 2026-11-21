use crate::services::ad::reset_fixture::{docker, docker_state, LiveService};
use crate::services::ad::scoring::test_fixture::ad_scoring_fixture;
use crate::services::container::docker_installation_scope;
use crate::services::control_jobs::{self, ControlJobKind};
use crate::utils::enums::Role;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::Value;
use std::{process::Command, time::Duration};
use tower::ServiceExt;
use uuid::Uuid;

struct TestImage(String);

impl Drop for TestImage {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["image", "rm", &self.0])
            .output();
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL, local Docker and the CI static-flag-service image"]
async fn player_reset_restores_the_registered_starter_after_an_organizer_rebuild() {
    let original = std::env::var("RSCTF_TEST_CONTAINER_IMAGE").unwrap();
    let fixture = ad_scoring_fixture().await;
    let pool = &fixture.pool;
    let runtime_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(12)
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let state = docker_state(&runtime_pool);
    let scope = docker_installation_scope();
    let source = LiveService(docker(&["create", &original]));
    let rebuilt = TestImage(docker(&[
        "commit",
        "--change",
        "LABEL fixture.rebuilt=true",
        &source.0,
    ]));
    assert_ne!(rebuilt.0, original);
    let live = LiveService(docker(&[
        "run",
        "-d",
        "--label",
        &format!("rsctf.managed={scope}"),
        "--label",
        &format!("rsctf.scope={scope}"),
        &rebuilt.0,
    ]));

    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(
        r#"SET LOCAL rsctf.maintenance_writer='on';
        UPDATE "Games" SET start_time_utc=now()+interval '5 minutes',
            end_time_utc=now()+interval '1 hour' WHERE id=900001;
        UPDATE "AdRounds" SET finalized=FALSE WHERE id=900108"#,
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(r#"UPDATE "GameChallenges" SET ad_self_hosted=FALSE,ad_allow_self_reset=TRUE,
        build_status=1,build_image_digest=$1,container_image='fixture',expose_port=8080 WHERE id=900031"#)
        .bind(&original).execute(&mut *tx).await.unwrap();
    sqlx::raw_sql(
        r#"INSERT INTO "AdPatchStarters"(challenge_id,tree_id,archive)
        VALUES(900031,repeat('a',64),'fixture');
        UPDATE "Games" SET start_time_utc=now()-interval '2 hours' WHERE id=900001"#,
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(r#"UPDATE "GameChallenges" SET build_image_digest=$1 WHERE id=900031"#)
        .bind(&rebuilt.0)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        r#"UPDATE "AdTeamServices" SET container_id=$1,published_image_backend=$1,
        published_image_ref=$2 WHERE id=900041"#,
    )
    .bind(&live.0)
    .bind(&rebuilt.0)
    .execute(&mut *tx)
    .await
    .unwrap();
    let captain: Uuid = sqlx::query_scalar(
        r#"UPDATE "AspNetUsers" SET role=$1, security_stamp='baseline-test'
        WHERE id=(SELECT captain_id FROM "Teams" WHERE id=900011) RETURNING id"#,
    )
    .bind(Role::User as i16)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "UserParticipations"(user_id,game_id,team_id,participation_id)
        VALUES($1,900001,900011,900021)"#,
    )
    .bind(captain)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let row = |id| {
        sqlx::query_scalar::<_, Value>(r#"SELECT to_jsonb(s) FROM "AdTeamServices" s WHERE id=$1"#)
            .bind(id)
            .fetch_one(pool)
    };
    let other_team = row(900042).await.unwrap();
    let flag = sqlx::query_scalar::<_, String>(
        r#"SELECT flag FROM "AdFlags" WHERE round_id=900108 AND team_service_id=900041"#,
    )
    .fetch_one(pool)
    .await
    .unwrap();
    let kind = ControlJobKind::AdReset;
    let token = state
        .token
        .issue(captain, Role::User, "fixture-one", "baseline-test")
        .unwrap();
    let app = super::router().with_state(state.clone());
    let request_reset = |service: i32, operation: Uuid| {
        app.clone().oneshot(
            Request::post(format!("/api/Game/900001/Ad/Services/{service}/Reset"))
                .header("authorization", format!("Bearer {token}"))
                .header("idempotency-key", operation.to_string())
                .body(Body::empty())
                .unwrap(),
        )
    };
    assert_eq!(
        request_reset(900041, Uuid::new_v4())
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    sqlx::query(r#"INSERT INTO "IdentityObservations"
        (user_id,game_id,team_id,participation_id,kind,value_hash,value_hint,source,observed_at_utc)
        VALUES($1,900001,900011,900021,'Fingerprint',decode(repeat('a',64),'hex'),'fixture','GameJoin',clock_timestamp())"#)
        .bind(captain).execute(pool).await.unwrap();
    assert_eq!(
        request_reset(900042, Uuid::new_v4())
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let operation = Uuid::new_v4();
    for _ in 0..2 {
        assert_eq!(
            request_reset(900041, operation).await.unwrap().status(),
            StatusCode::ACCEPTED
        );
    }
    let claimed = control_jobs::claim_next(pool, kind, Duration::from_secs(60))
        .await
        .unwrap()
        .unwrap();
    let (shutdown, rx) = tokio::sync::watch::channel(false);
    let capture = crate::services::traffic::start_capture_reconciler(state.clone(), rx);
    let result = crate::services::ad::reset::execute_job(&state, &claimed).await;
    assert_eq!(result.unwrap()["reset"], true);
    let restored = row(900041).await.unwrap();
    let replacement = LiveService(restored["container_id"].as_str().unwrap().to_owned());
    assert_eq!(
        docker(&["inspect", "--format", "{{.Image}}", &replacement.0]),
        original
    );
    assert_eq!(restored["published_image_ref"], original);
    assert_eq!(restored["previous_image_ref"], rebuilt.0);
    assert_eq!(
        docker(&["exec", &replacement.0, "printenv", "RSCTF_FLAG"]),
        flag
    );
    assert_eq!(row(900042).await.unwrap(), other_team);
    // Recover a stopped service after both a different successful organizer
    // build and a failed one. The team's selected revision stays authoritative.
    let game = crate::services::game_config::load_game(&state, fixture.game_id)
        .await
        .unwrap();
    let mut active = replacement;
    for build_ready in [true, false] {
        if !build_ready {
            sqlx::raw_sql(r#"SET LOCAL rsctf.maintenance_writer='on';
                UPDATE "GameChallenges" SET build_status=2,build_image_digest=NULL WHERE id=900031"#)
                .execute(pool)
                .await
                .unwrap();
        }
        docker(&["stop", "-t", "1", &active.0]);
        let counts = crate::services::ad::provision::ensure_ad_containers(
            &state,
            &game,
            Some(900021),
            false,
            false,
            false,
            None,
        )
        .await
        .unwrap();
        let repaired = row(900041).await.unwrap();
        let next = LiveService(repaired["container_id"].as_str().unwrap().to_owned());
        assert_eq!(counts, (1, 0));
        assert_ne!(active.0, next.0);
        assert_eq!(
            docker(&["inspect", "--format", "{{.Image}}", &next.0]),
            original
        );
        assert_eq!(docker(&["exec", &next.0, "printenv", "RSCTF_FLAG"]), flag);
        assert_eq!(repaired["previous_image_ref"], rebuilt.0);
        assert_eq!(repaired["last_reset_at"], restored["last_reset_at"]);
        assert_eq!(row(900042).await.unwrap(), other_team);
        active = next;
    }
    shutdown.send_replace(true);
    capture.await.unwrap();
    runtime_pool.close().await;
    fixture.cleanup().await;
}
