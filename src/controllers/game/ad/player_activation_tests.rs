use super::*;
use crate::app_state::SharedState;
use serde_json::json;

fn publish_test_image(tag: &str, repo: &str) -> (String, String) {
    let config = docker(&["image", "inspect", "--format", "{{.Id}}", tag]);
    docker(&["push", tag]);
    let digests: Vec<String> = serde_json::from_str(&docker(&[
        "image",
        "inspect",
        "--format",
        "{{json .RepoDigests}}",
        tag,
    ]))
    .unwrap();
    let image = digests
        .into_iter()
        .find(|r| r.starts_with(&format!("{repo}@")))
        .unwrap();
    (config, image)
}

pub(super) async fn ready_candidate(
    pool: &sqlx::PgPool,
    captain: Uuid,
    config: &str,
    image: &str,
) -> Uuid {
    let (_, manifest) = image.split_once('@').unwrap();
    let attempt = Uuid::new_v4();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query(r#"INSERT INTO "AdPatchSourceTrees" VALUES(900041,repeat('b',64),'{}')"#)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "AdPatchRevisions" VALUES(900041,1,repeat('b',64),repeat('a',64),repeat('c',64),$1,now())"#)
        .bind(captain).execute(&mut *tx).await.unwrap();
    sqlx::query(r#"INSERT INTO "AdSharedPatchChanges"(service_id,revision,changes) VALUES(900041,1,'[{"path":"app.py","kind":"Modified"}]')"#)
        .execute(&mut *tx).await.unwrap();
    sqlx::query(
        r#"INSERT INTO "AdPatchBuilds"(service_id,revision,attempt,profile,status)
        VALUES(900041,1,$1,repeat('a',64),'Ready')"#,
    )
    .bind(attempt)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "AdPatchCandidates"(service_id,revision,digest,archive_bytes,config_digest,manifest_digest,registry_image_ref)
        VALUES(900041,1,repeat('a',64),1,$1,$2,$3)"#)
        .bind(config).bind(manifest).bind(image).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    attempt
}

pub(super) async fn check(
    app: &axum::Router,
    state: &SharedState,
    captain: Uuid,
    token: &str,
    registry: &str,
    original: &str,
    flag: &str,
) {
    let endpoint: std::net::SocketAddr = registry.parse().unwrap();
    assert!(endpoint.ip().is_loopback());
    let repo = format!("{registry}/test-{}/service-900041", Uuid::new_v4());
    let tag = TestImage(format!("{repo}:fixture"));
    if let Ok(candidate) = std::env::var("RSCTF_TEST_AD_PATCH_CANDIDATE_IMAGE") {
        assert!(crate::services::challenge_images::is_local_image_id(
            &candidate
        ));
        docker(&["tag", &candidate, &tag.0]);
    } else {
        let source = LiveService(docker(&["create", original]));
        docker(&[
            "cp",
            "tests/fixtures/ad-smoke.Dockerfile",
            &format!("{}:/rsctf-patch-fixture", source.0),
        ]);
        docker(&["commit", &source.0, &tag.0]);
    }
    let (config, image) = publish_test_image(&tag.0, &repo);
    assert_ne!(config, original);
    let pool = state.pg();
    let attempt = ready_candidate(pool, captain, &config, &image).await;
    let activate = |service: i32, attempt: Uuid, operation: Uuid| {
        player_post(
            app,
            token,
            "/api/game/900001/ad/challenges/900031/patch-revisions/1/activate".to_owned(),
            operation,
            Some(json!({"service":service,"attempt":attempt})),
        )
    };
    // The previous rollback's cooldown still applies to ZIP activation.
    assert_eq!(
        activate(900041, attempt, Uuid::new_v4())
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    sqlx::query(
        r#"UPDATE "AdTeamServices" SET last_reset_at=now()-interval '1 day' WHERE id=900041"#,
    )
    .execute(pool)
    .await
    .unwrap();
    assert_eq!(
        activate(900042, attempt, Uuid::new_v4())
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        activate(900041, Uuid::new_v4(), Uuid::new_v4())
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    let read = || {
        sqlx::query_as::<_, (String, i32, String, Value)>(
            r#"SELECT host,port,container_id,to_jsonb(c) FROM "AdTeamServices" s
        JOIN "AdCheckResults" c ON c.team_service_id=s.id AND c.round_id=900108 WHERE s.id=900041"#,
        )
        .fetch_one(pool)
    };
    let before = read().await.unwrap();
    // A candidate can build successfully while introducing an incompatible volume.
    let source = LiveService(docker(&["create", original]));
    let extra = TestImage(format!("{repo}:extra-volume"));
    docker(&[
        "commit",
        "--change",
        "VOLUME /var/lib/extra",
        &source.0,
        &extra.0,
    ]);
    let (extra_config, extra_image) = publish_test_image(&extra.0, &repo);
    let select_candidate = |config: String, image: String| async move {
        let manifest = image.split_once('@').unwrap().1;
        sqlx::query(r#"UPDATE "AdPatchCandidates" SET config_digest=$1,manifest_digest=$2,registry_image_ref=$3 WHERE service_id=900041"#)
            .bind(config).bind(manifest).bind(&image).execute(pool).await.unwrap();
    };
    select_candidate(extra_config, extra_image.clone()).await;
    assert_eq!(
        activate(900041, attempt, Uuid::new_v4())
            .await
            .unwrap()
            .status(),
        StatusCode::ACCEPTED
    );
    let denied =
        control_jobs::claim_next(pool, ControlJobKind::AdActivate, Duration::from_secs(120))
            .await
            .unwrap()
            .unwrap();
    assert_eq!(denied.input["candidateImageRef"], extra_image.as_str());
    let error = crate::services::ad::reset::execute_job(state, &denied)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, crate::utils::error::AppError::Conflict(reason) if reason.contains("volumes"))
    );
    assert_eq!(read().await.unwrap(), before);
    assert_eq!(docker(&["exec", &before.2, "cat", "/flag"]), flag);
    assert!(control_jobs::fail(
        pool,
        denied.model.id,
        denied.lease_token,
        denied.input_revision,
        &error.to_string()
    )
    .await
    .unwrap());
    select_candidate(config.clone(), image.clone()).await;
    let op = Uuid::new_v4();
    assert_eq!(
        activate(900041, attempt, op).await.unwrap().status(),
        StatusCode::ACCEPTED
    );
    let job = control_jobs::claim_next(pool, ControlJobKind::AdActivate, Duration::from_secs(120))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job.input["candidateImageRef"], image.as_str());
    // Replacing the ready candidate after admission must preserve the live endpoint and checker evidence.
    let previous_config = docker(&["inspect", "--format", "{{.Image}}", &before.2]);
    sqlx::query(r#"UPDATE "AdPatchBuilds" SET status='Replaced' WHERE attempt=$1"#)
        .bind(attempt)
        .execute(pool)
        .await
        .unwrap();
    assert!(matches!(
        crate::services::ad::reset::execute_job(state, &job).await,
        Err(crate::utils::error::AppError::Conflict(_))
    ));
    assert!(!sqlx::query_scalar::<_, bool>(
        r#"SELECT published FROM "AdSharedPatchChanges" WHERE service_id=900041 AND revision=1"#
    )
    .fetch_one(pool)
    .await
    .unwrap());
    assert_eq!(read().await.unwrap(), before);
    sqlx::query(r#"UPDATE "AdPatchBuilds" SET status='Ready' WHERE attempt=$1"#)
        .bind(attempt)
        .execute(pool)
        .await
        .unwrap();
    assert_eq!(
        crate::services::ad::reset::execute_job(state, &job)
            .await
            .unwrap()["reset"],
        true
    );
    let (host, port, backend, _) = read().await.unwrap();
    assert!(sqlx::query_scalar::<_, bool>(
        r#"SELECT published FROM "AdSharedPatchChanges" WHERE service_id=900041 AND revision=1"#
    )
    .fetch_one(pool)
    .await
    .unwrap());
    let service = LiveService(backend);
    assert!(!host.is_empty() && port > 0);
    assert_eq!(
        docker(&["inspect", "--format", "{{.Image}}", &service.0]),
        config
    );
    assert_eq!(docker(&["exec", &service.0, "cat", "/flag"]), flag);
    let published: String =
        sqlx::query_scalar(r#"SELECT published_image_ref FROM "AdTeamServices" WHERE id=900041"#)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(published, image);
    assert!(control_jobs::complete(
        pool,
        job.model.id,
        job.lease_token,
        job.input_revision,
        json!({"reset":true})
    )
    .await
    .unwrap());
    assert_eq!(
        activate(900041, attempt, op).await.unwrap().status(),
        StatusCode::ACCEPTED
    );
    assert_eq!(
        activate(900041, Uuid::new_v4(), op).await.unwrap().status(),
        StatusCode::CONFLICT
    );
    assert!(
        control_jobs::claim_next(pool, ControlJobKind::AdActivate, Duration::from_secs(60))
            .await
            .unwrap()
            .is_none()
    );
    let mut restored_services = Vec::new();
    for (action, kind, expected) in [
        (
            "Rollback",
            ControlJobKind::AdRollback,
            previous_config.as_str(),
        ),
        ("Reset", ControlJobKind::AdReset, original),
    ] {
        sqlx::query(
            r#"UPDATE "AdTeamServices" SET last_reset_at=now()-interval '1 day' WHERE id=900041"#,
        )
        .execute(pool)
        .await
        .unwrap();
        let response = player_post(
            app,
            token,
            format!("/api/Game/900001/Ad/Services/900041/{action}"),
            Uuid::new_v4(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let restore = control_jobs::claim_next(pool, kind, Duration::from_secs(90))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            crate::services::ad::reset::execute_job(state, &restore)
                .await
                .unwrap()["reset"],
            true
        );
        let backend = LiveService(read().await.unwrap().2);
        assert_eq!(
            docker(&["inspect", "--format", "{{.Image}}", &backend.0]),
            expected
        );
        assert_eq!(docker(&["exec", &backend.0, "cat", "/flag"]), flag);
        assert!(control_jobs::complete(
            pool,
            restore.model.id,
            restore.lease_token,
            restore.input_revision,
            json!({"reset":true})
        )
        .await
        .unwrap());
        // Leave the published container available for the following replacement.
        restored_services.push(backend);
    }
}
