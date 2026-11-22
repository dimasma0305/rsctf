use super::*;
use crate::services::ad::reset_fixture::runtime_state;
use crate::services::container::{ContainerResourceLimits, ContainerSpec, TeamStorage};
use crate::services::k8s::KubernetesContainerManager;
use serde_json::json;
use std::sync::Arc;

async fn register_service(pool: &sqlx::PgPool, starter: &str) -> Uuid {
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(r#"SET LOCAL rsctf.maintenance_writer='on';
        UPDATE "Games" SET start_time_utc=now()+interval '5 minutes',end_time_utc=now()+interval '2 hours' WHERE id=900001;
        UPDATE "AdRounds" SET finalized=FALSE WHERE id=900108"#).execute(&mut *tx).await.unwrap();
    sqlx::query(
        r#"UPDATE "GameChallenges" SET ad_self_hosted=FALSE,ad_allow_self_reset=TRUE,
        build_status=1,build_image_digest=$1,container_image='fixture',expose_port=6379,
        memory_limit=128,storage_limit=64 WHERE id=900031"#,
    )
    .bind(starter)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::raw_sql(
        r#"INSERT INTO "AdPatchStarters"(challenge_id,tree_id,archive,data_mount)
        VALUES(900031,repeat('a',64),'fixture','/data');
        UPDATE "Games" SET start_time_utc=now()-interval '2 hours' WHERE id=900001"#,
    )
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

#[tokio::test]
#[ignore = "requires disposable Kind, CSI, a registry, and PostgreSQL"]
async fn player_kubernetes_patch_activation_and_rollbacks() {
    assert_eq!(std::env::var("RSCTF_K8S_LIVE_STORAGE").as_deref(), Ok("1"));
    let _ = tokio_rustls::rustls::crypto::aws_lc_rs::default_provider().install_default();
    let starter = std::env::var("RSCTF_K8S_TEST_STARTER").unwrap();
    let previous = std::env::var("RSCTF_K8S_TEST_REVISION_ONE").unwrap();
    let candidate = std::env::var("RSCTF_K8S_TEST_REVISION_TWO").unwrap();
    let config = std::env::var("RSCTF_K8S_TEST_CANDIDATE_CONFIG").unwrap();
    let fixture = ad_scoring_fixture().await;
    let pool = &fixture.pool;
    let runtime_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(12)
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let state = runtime_state(
        &runtime_pool,
        Arc::new(KubernetesContainerManager::connect().await.unwrap()),
    );
    let captain = register_service(pool, &starter).await;
    let flag: String = sqlx::query_scalar(
        r#"SELECT flag FROM "AdFlags" WHERE round_id=900108 AND team_service_id=900041"#,
    )
    .fetch_one(pool)
    .await
    .unwrap();
    let mut spec = ContainerSpec::ad_service(
        previous.clone(),
        ContainerResourceLimits {
            memory_limit: 128,
            cpu_count: 1,
            storage_limit: 64,
        },
        6379,
        900011,
        false,
        String::new(),
    );
    TeamStorage::configure(&state, &mut spec, 900001, 900021, 900031)
        .await
        .unwrap();
    spec.set_ad_launch_flag(flag.clone(), true);
    let first = state.containers.create(spec).await.unwrap();
    let installed = state
        .containers
        .exec(&first.id, vec!["cat".into(), "/flag".into()])
        .await
        .unwrap();
    assert_eq!(installed.trim(), flag);
    assert_eq!(
        state
            .containers
            .exec(
                &first.id,
                [
                    "sh",
                    "-ec",
                    "echo team-one > /data/installation && cat /data/installation"
                ]
                .map(String::from)
                .to_vec()
            )
            .await
            .unwrap()
            .trim(),
        "team-one"
    );
    sqlx::query(
        r#"UPDATE "AdTeamServices" SET host=$1,port=$2,container_id=$3,published_image_backend=$3,
        published_image_ref=$4 WHERE id=900041"#,
    )
    .bind(&first.ip)
    .bind(first.port)
    .bind(&first.id)
    .bind(&previous)
    .execute(pool)
    .await
    .unwrap();
    let attempt = super::activation::ready_candidate(pool, captain, &config, &candidate).await;
    let token = state
        .token
        .issue(captain, Role::User, "fixture-one", "kind-test")
        .unwrap();
    let app = super::super::router().with_state(state.clone());
    let base = "/api/game/900001/ad/challenges/900031/patch-revisions/1";
    let get = Request::get(format!(
        "{base}/build?service=900041&commit={}",
        "c".repeat(64)
    ))
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
    let mut serving = first.id;
    for (kind, path, payload, expected, label) in [
        (
            ControlJobKind::AdActivate,
            activate,
            Some(json!({"service":900041,"attempt":attempt})),
            &candidate,
            "patched",
        ),
        (
            ControlJobKind::AdRollback,
            "/api/Game/900001/Ad/Services/900041/Rollback".into(),
            None,
            &previous,
            "previous",
        ),
        (
            ControlJobKind::AdReset,
            "/api/Game/900001/Ad/Services/900041/Reset".into(),
            None,
            &starter,
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
            crate::services::ad::reset::execute_job(&state, &job)
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
        let (ip,port,id,image) = sqlx::query_as::<_,(String,i32,String,String)>(
            r#"SELECT host,port,container_id,published_image_ref FROM "AdTeamServices" WHERE id=900041 AND published_image_backend=container_id"#)
            .fetch_one(pool).await.unwrap();
        assert_eq!((&ip, port), (&first.ip, first.port));
        assert_eq!(&image, expected);
        assert_ne!(id, serving);
        let files = state
            .containers
            .exec(
                &id,
                [
                    "sh",
                    "-ec",
                    "printenv RSCTF_TEST_REVISION && cat /data/installation /flag",
                ]
                .map(String::from)
                .to_vec(),
            )
            .await
            .unwrap();
        assert_eq!(files.trim(), format!("{label}\nteam-one\n{flag}"));
        assert_eq!(evidence().await.unwrap(), before);
        serving = id;
    }
    state.containers.destroy(&serving).await.unwrap();
    shutdown.send_replace(true);
    capture.await.unwrap();
    runtime_pool.close().await;
    fixture.cleanup().await;
}
