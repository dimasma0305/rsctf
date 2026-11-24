use super::*;
use crate::app_state::SharedState;
use serde_json::json;
use std::io::{Cursor, Write};

fn source_zip(revision: &[u8]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (path, bytes) in [
        (
            "Dockerfile",
            include_bytes!("../../../../tests/fixtures/worker-patch/Dockerfile").as_slice(),
        ),
        ("src/revision", revision),
    ] {
        zip.start_file(path, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

pub(crate) async fn register_service(
    pool: &sqlx::PgPool,
    starter: &str,
    rebuilt: &str,
    port: i32,
) -> Uuid {
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(r#"SET LOCAL rsctf.maintenance_writer='on';
        UPDATE "Games" SET start_time_utc=now()+interval '5 minutes',end_time_utc=now()+interval '2 hours' WHERE id=900001;
        UPDATE "AdRounds" SET finalized=FALSE WHERE id=900108"#).execute(&mut *tx).await.unwrap();
    sqlx::query(
        r#"UPDATE "GameChallenges" SET ad_self_hosted=FALSE,ad_allow_self_reset=TRUE,
        build_status=1,build_image_digest=$1,container_image='fixture',expose_port=$2,
        memory_limit=128,storage_limit=64 WHERE id=900031"#,
    )
    .bind(starter)
    .bind(port)
    .execute(&mut *tx)
    .await
    .unwrap();
    let archive = source_zip(b"original\n");
    let tree = crate::services::ad::patches::PatchManifest::from_zip(&archive)
        .unwrap()
        .tree_id();
    sqlx::query(
        r#"INSERT INTO "AdPatchStarters"(challenge_id,tree_id,archive,data_mount)
        VALUES(900031,$1,$2,'/data')"#,
    )
    .bind(tree)
    .bind(archive)
    .execute(&mut *tx)
    .await
    .unwrap();
    // Register the starter and record the organizer's rebuild before opening play.
    sqlx::query(r#"UPDATE "GameChallenges" SET build_image_digest=$1 WHERE id=900031"#)
        .bind(rebuilt)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(r#"UPDATE "Games" SET start_time_utc=now()-interval '2 hours' WHERE id=900001"#)
        .execute(&mut *tx)
        .await
        .unwrap();
    let captain = sqlx::query_scalar::<_, Uuid>(
        r#"UPDATE "AspNetUsers" SET role=$1,security_stamp='kind-test'
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
    sqlx::query(r#"INSERT INTO "IdentityObservations"
        (user_id,game_id,team_id,participation_id,kind,value_hash,value_hint,source,observed_at_utc)
        VALUES($1,900001,900011,900021,'Fingerprint',decode(repeat('a',64),'hex'),'fixture','GameJoin',now())"#)
        .bind(captain).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    captain
}

pub(crate) async fn exercise(
    state: &SharedState,
    captain: Uuid,
    images: [&str; 3],
    config: &str,
    verify: impl AsyncFn(&str, &str),
) -> String {
    let [starter, previous, candidate] = images;
    let pool = state.pg();
    let token = state
        .token
        .issue(captain, Role::User, "fixture-one", "kind-test")
        .unwrap();
    let app = super::super::router().with_state(state.clone());
    let zip = source_zip(include_bytes!(
        "../../../../tests/fixtures/worker-patch/src/revision"
    ));
    let upload = Request::post("/api/game/900001/ad/challenges/900031/patch-revisions")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/zip")
        .header("x-rsctf-expected-revision", "0")
        .header("x-rsctf-expected-service", "900041")
        .body(Body::from(zip))
        .unwrap();
    let response = app.clone().oneshot(upload).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let revision: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 8192)
            .await
            .unwrap(),
    )
    .unwrap();
    let commit = revision["commitId"].as_str().unwrap();
    let attempt = super::activation::staged_image(pool, config, candidate).await;
    let base = "/api/game/900001/ad/challenges/900031/patch-revisions/1";
    let get = Request::get(format!("{base}/build?service=900041&commit={commit}"))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let status = app.clone().oneshot(get).await.unwrap();
    assert_eq!(status.status(), StatusCode::OK);
    let view: Value = serde_json::from_slice(
        &axum::body::to_bytes(status.into_body(), 8192)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(view["canActivate"], true);
    let activate = format!("{base}/activate");
    assert_eq!(
        player_post(
            &app,
            &token,
            activate.clone(),
            Uuid::new_v4(),
            Some(json!({"service":900042,"attempt":attempt}))
        )
        .await
        .unwrap()
        .status(),
        StatusCode::NOT_FOUND
    );
    let evidence = || {
        sqlx::query_scalar::<_, Value>(
            r#"SELECT jsonb_build_array(
        (SELECT jsonb_agg(to_jsonb(c) ORDER BY c.id) FROM "AdCheckResults" c),
        (SELECT jsonb_agg(to_jsonb(f) ORDER BY f.id) FROM "AdFlags" f),
        (SELECT to_jsonb(s) FROM "AdTeamServices" s WHERE id=900042))"#,
        )
        .fetch_one(pool)
    };
    let before = evidence().await.unwrap();
    let (shutdown, rx) = tokio::sync::watch::channel(false);
    let capture = crate::services::traffic::start_capture_reconciler(state.clone(), rx);
    let (host, port, mut serving): (String, i32, String) =
        sqlx::query_as(r#"SELECT host,port,container_id FROM "AdTeamServices" WHERE id=900041"#)
            .fetch_one(pool)
            .await
            .unwrap();
    for (kind, path, payload, expected, label) in [
        (
            ControlJobKind::AdActivate,
            activate,
            Some(json!({"service":900041,"attempt":attempt})),
            candidate,
            "patched",
        ),
        (
            ControlJobKind::AdRollback,
            "/api/Game/900001/Ad/Services/900041/Rollback".into(),
            None,
            previous,
            "previous",
        ),
        (
            ControlJobKind::AdReset,
            "/api/Game/900001/Ad/Services/900041/Reset".into(),
            None,
            starter,
            "starter",
        ),
    ] {
        sqlx::query(
            r#"UPDATE "AdTeamServices" SET last_reset_at=now()-interval '1 day' WHERE id=900041"#,
        )
        .execute(pool)
        .await
        .unwrap();
        let operation = Uuid::new_v4();
        for _ in 0..2 {
            let response = player_post(&app, &token, path.clone(), operation, payload.clone())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::ACCEPTED);
        }
        let job = control_jobs::claim_next(pool, kind, Duration::from_secs(240))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            crate::services::ad::reset::execute_job(state, &job)
                .await
                .unwrap()["reset"],
            true
        );
        assert!(control_jobs::complete(
            pool,
            job.model.id,
            job.lease_token,
            job.input_revision,
            json!({"reset":true})
        )
        .await
        .unwrap());
        let (ip,new_port,id,image) = sqlx::query_as::<_,(String,i32,String,String)>(
            r#"SELECT host,port,container_id,published_image_ref FROM "AdTeamServices" WHERE id=900041 AND published_image_backend=container_id"#)
            .fetch_one(pool).await.unwrap();
        assert_eq!((&ip, new_port), (&host, port));
        assert_eq!(image, expected);
        assert_ne!(id, serving);
        verify(&id, label).await;
        assert_eq!(evidence().await.unwrap(), before);
        serving = id;
    }
    shutdown.send_replace(true);
    capture.await.unwrap();
    serving
}
