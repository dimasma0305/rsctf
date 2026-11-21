use super::*;
use crate::app_state::SharedState;
use serde_json::json;

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
    docker(&["tag", original, &tag.0]);
    docker(&["push", &tag.0]);
    let digests: Vec<String> = serde_json::from_str(&docker(&[
        "image",
        "inspect",
        "--format",
        "{{json .RepoDigests}}",
        &tag.0,
    ]))
    .unwrap();
    let image = digests
        .iter()
        .find(|r| r.starts_with(&format!("{repo}@")))
        .unwrap();
    let (_, manifest) = image.split_once('@').unwrap();
    let pool = state.pg();
    let attempt = Uuid::new_v4();
    let activate = |service: i32, attempt: Uuid, operation: Uuid| async move {
        let send = || {
            app.clone().oneshot(
                Request::post("/api/game/900001/ad/challenges/900031/patch-revisions/1/activate")
                    .header("authorization", format!("Bearer {token}"))
                    .header("idempotency-key", operation.to_string())
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"service":service,"attempt":attempt}).to_string(),
                    ))
                    .unwrap(),
            )
        };
        let response = send().await?;
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            let seconds: u64 = response.headers()["retry-after"]
                .to_str()
                .unwrap()
                .parse()
                .unwrap();
            assert!((1..=10).contains(&seconds));
            tokio::time::sleep(Duration::from_secs(seconds + 1)).await;
            send().await
        } else {
            Ok(response)
        }
    };
    let mut tx = pool.begin().await.unwrap();
    sqlx::query(r#"INSERT INTO "AdPatchSourceTrees" VALUES(900041,repeat('b',64),'{}')"#)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "AdPatchRevisions" VALUES(900041,1,repeat('b',64),repeat('a',64),repeat('c',64),$1,now())"#)
        .bind(captain).execute(&mut *tx).await.unwrap();
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
        .bind(original).bind(manifest).bind(image).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
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
    let read = || {
        sqlx::query_as::<_, (String, i32, String, Value)>(
            r#"SELECT host,port,container_id,to_jsonb(c) FROM "AdTeamServices" s
        JOIN "AdCheckResults" c ON c.team_service_id=s.id AND c.round_id=900108 WHERE s.id=900041"#,
        )
        .fetch_one(pool)
    };
    let before = read().await.unwrap();
    sqlx::query(r#"UPDATE "AdPatchBuilds" SET status='Replaced' WHERE attempt=$1"#)
        .bind(attempt)
        .execute(pool)
        .await
        .unwrap();
    assert!(matches!(
        crate::services::ad::reset::execute_job(state, &job).await,
        Err(crate::utils::error::AppError::Conflict(_))
    ));
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
    let service = LiveService(backend);
    assert!(!host.is_empty() && port > 0);
    assert_eq!(
        docker(&["inspect", "--format", "{{.Image}}", &service.0]),
        original
    );
    assert_eq!(docker(&["exec", &service.0, "cat", "/flag"]), flag);
    let published: String =
        sqlx::query_scalar(r#"SELECT published_image_ref FROM "AdTeamServices" WHERE id=900041"#)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(published, *image);
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
}
