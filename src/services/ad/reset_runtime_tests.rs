use std::time::Duration;

use serde_json::{json, Value};
use uuid::Uuid;

use crate::services::ad::reset_fixture::{docker, docker_state, LiveService};
use crate::services::ad::scoring::test_fixture::ad_scoring_fixture;
use crate::services::container::docker_installation_scope;
use crate::services::control_jobs::{self, ControlJobKind};

#[tokio::test]
#[ignore = "requires PostgreSQL, Docker and the CI static-flag-service image"]
async fn missing_reset_image_preserves_live_endpoint_and_scoring() {
    let image = std::env::var("RSCTF_TEST_CONTAINER_IMAGE").unwrap();
    let fixture = ad_scoring_fixture().await;
    let pool = &fixture.pool;
    let state = docker_state(pool);
    state
        .containers
        .profile_pull_image(None, &image)
        .await
        .unwrap();
    let scope = docker_installation_scope();
    let live = LiveService(docker(&[
        "run",
        "-d",
        "--rm",
        "--label",
        &format!("rsctf.managed={scope}"),
        "--label",
        &format!("rsctf.scope={scope}"),
        "-p",
        "127.0.0.1::8080",
        &image,
    ]));
    let addr = docker(&["port", &live.0, "8080/tcp"])
        .parse::<std::net::SocketAddr>()
        .unwrap();
    let url = format!("http://{addr}/health");
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if client
                .get(&url)
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();

    sqlx::raw_sql(r#"
        SET LOCAL rsctf.maintenance_writer = 'on';
        UPDATE "Games" SET end_time_utc=now()+interval '1 hour' WHERE id=900001;
        UPDATE "GameChallenges" SET ad_self_hosted=FALSE, build_status=1,
            build_image_digest='sha256:' || repeat('f',64) WHERE id=900031;
        UPDATE "AdRounds" SET finalized=FALSE WHERE id=900108;
        UPDATE "AdCheckResults" SET sla_credit=NULL WHERE round_id=900108 AND team_service_id=900041;
    "#).execute(pool).await.unwrap();
    sqlx::query(r#"UPDATE "AdTeamServices" SET host=$1, port=$2, container_id=$3 WHERE id=900041"#)
        .bind(addr.ip().to_string())
        .bind(i32::from(addr.port()))
        .bind(&live.0)
        .execute(pool)
        .await
        .unwrap();
    let input = json!({
        "serviceId":900041, "participationId":900021, "expectedBackendId":live.0, "playerPolicy":false
    });
    let kind = ControlJobKind::AdReset;
    let job = control_jobs::enqueue(
        pool,
        kind,
        "fixture-reset",
        fixture.game_id,
        Some(900031),
        Uuid::new_v4(),
        &"a".repeat(64),
        input,
    )
    .await
    .unwrap();
    let claimed = control_jobs::claim_next(pool, kind, Duration::from_secs(60))
        .await
        .unwrap()
        .unwrap();
    let snapshot = || {
        sqlx::query_scalar::<_, Value>(
            r#"
        SELECT jsonb_build_array(to_jsonb(s), to_jsonb(c), to_jsonb(f), j.input)
          FROM "AdTeamServices" s
          JOIN "AdCheckResults" c ON c.team_service_id=s.id AND c.round_id=900108
          JOIN "AdFlags" f ON f.team_service_id=s.id AND f.round_id=c.round_id
          CROSS JOIN "ControlPlaneJobs" j WHERE s.id=900041 AND j.id=$1
    "#,
        )
        .bind(job.id)
        .fetch_one(pool)
    };
    let original = snapshot().await.unwrap();
    for _ in 0..2 {
        let error = super::execute_job(&state, &claimed).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("daemon-local image id is absent"),
            "{error}"
        );
        assert_eq!(snapshot().await.unwrap(), original);
        assert_eq!(
            client
                .get(&url)
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .text()
                .await
                .unwrap()
                .trim(),
            "ok"
        );
    }
    // A process can stop after committing the preparation but before removing
    // the old container. Remove only this test's staged replacement image.
    let tag = format!("rsctf-reset-test:{}", job.id);
    docker(&[
        "commit",
        "--pause=false",
        "--change",
        &format!("LABEL fixture.reset={}", job.id),
        &live.0,
        &tag,
    ]);
    let replacement = docker(&["image", "inspect", "--format", "{{.Id}}", &tag]);
    state
        .containers
        .profile_pull_image(None, &replacement)
        .await
        .unwrap();
    let prepared = crate::services::ad_engine::prepare_service_reset(
        &state.db,
        fixture.game_id,
        900041,
        Some(&live.0),
        job.id,
        claimed.lease_token,
        &replacement,
        "fixture reset",
    )
    .await;
    docker(&["image", "rm", &tag]);
    assert_eq!(
        prepared.unwrap().retired_container_id.as_ref(),
        Some(&live.0)
    );
    docker(&[
        "exec",
        &live.0,
        "sh",
        "-c",
        "printf team-data > /tmp/reset-note",
    ]);
    sqlx::query(r#"UPDATE "ControlPlaneJobs" SET lease_expires_at_utc=now()-interval '1 second' WHERE id=$1"#)
        .bind(job.id).execute(pool).await.unwrap();
    let reclaimed = control_jobs::claim_next(pool, kind, Duration::from_secs(60))
        .await
        .unwrap()
        .unwrap();
    assert_ne!(claimed.lease_token, reclaimed.lease_token);
    let checkpoint = snapshot().await.unwrap();
    let error = super::execute_job(&state, &reclaimed).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("daemon-local image id is absent"),
        "{error}"
    );
    assert_eq!(snapshot().await.unwrap(), checkpoint);
    assert_eq!(
        docker(&["exec", &live.0, "cat", "/tmp/reset-note"]),
        "team-data"
    );
    assert!(client.get(&url).send().await.unwrap().status().is_success());
    drop(live);
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL, local Docker and the CI static-flag-service image"]
async fn reconcile_preserves_uncertain_backends_and_reclaimed_reset() {
    let image = std::env::var("RSCTF_TEST_CONTAINER_IMAGE").unwrap();
    let fixture = ad_scoring_fixture().await;
    let pool = &fixture.pool;
    // Production reserves dedicated connections for the capture owner and
    // its heartbeat while the reset holds the service publication lock.
    let runtime_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(12)
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let state = docker_state(&runtime_pool);
    let scope = docker_installation_scope();
    let starting = LiveService(docker(&[
        "create",
        "--label",
        &format!("rsctf.managed={scope}"),
        "--label",
        &format!("rsctf.scope={scope}"),
        &image,
    ]));
    // Docker refuses to attribute this other installation's container to our manager.
    let other = LiveService(docker(&["run", "-d", "--rm", &image]));
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(
        r#"
        SET LOCAL rsctf.maintenance_writer='on';
        UPDATE "Games" SET end_time_utc=now()+interval '1 hour' WHERE id=900001;
        UPDATE "AdRounds" SET finalized=FALSE WHERE id=900108;
    "#,
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        r#"UPDATE "GameChallenges" SET ad_self_hosted=FALSE,container_image='fixture',
        build_status=1,build_image_digest=$1,expose_port=8080 WHERE id=900031"#,
    )
    .bind(&image)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(r#"UPDATE "AdTeamServices" SET container_id=CASE id WHEN 900041 THEN $1 ELSE $2 END WHERE id IN (900041,900042)"#)
        .bind(&starting.0).bind(&other.0).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();

    let game = crate::services::game_config::load_game(&state, fixture.game_id)
        .await
        .unwrap();
    let reconcile = || {
        crate::services::ad::provision::ensure_ad_containers(
            &state, &game, None, false, false, false, None,
        )
    };
    let row = |id| {
        sqlx::query_scalar::<_, Value>(r#"SELECT to_jsonb(s) FROM "AdTeamServices" s WHERE id=$1"#)
            .bind(id)
            .fetch_one(pool)
    };
    let old = row(900041).await.unwrap();
    let unrelated = row(900042).await.unwrap();
    // Docker Created and an inspection error both leave the published endpoints intact.
    assert_eq!(reconcile().await.unwrap(), (0, 2));
    assert_eq!(row(900041).await.unwrap(), old);
    assert_eq!(row(900042).await.unwrap(), unrelated);
    docker(&["start", &starting.0]);
    assert_eq!(reconcile().await.unwrap(), (0, 1));

    let (shutdown, rx) = tokio::sync::watch::channel(false);
    let capture = crate::services::traffic::start_capture_reconciler(state.clone(), rx);
    docker(&[
        "exec",
        &starting.0,
        "sh",
        "-c",
        "printf team-data > /tmp/repair-note",
    ]);
    docker(&["stop", "-t", "1", &starting.0]);
    sqlx::raw_sql(r#"SET LOCAL rsctf.maintenance_writer='on';
        UPDATE "GameChallenges" SET build_image_digest='sha256:' || repeat('e',64) WHERE id=900031"#)
        .execute(pool).await.unwrap();
    assert_eq!(reconcile().await.unwrap(), (0, 2));
    let stopped = row(900041).await.unwrap();
    assert_eq!(stopped["host"], "");
    assert_eq!(stopped["port"], 0);
    assert_eq!(stopped["container_id"], starting.0);
    assert_eq!(row(900042).await.unwrap(), unrelated);
    let note = std::env::temp_dir().join(format!("rsctf-repair-note-{}", Uuid::new_v4()));
    docker(&[
        "cp",
        &format!("{}:/tmp/repair-note", starting.0),
        note.to_str().unwrap(),
    ]);
    let contents = std::fs::read(&note).unwrap();
    std::fs::remove_file(note).unwrap();
    assert_eq!(contents, b"team-data");
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL rsctf.maintenance_writer='on'")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(r#"UPDATE "GameChallenges" SET build_image_digest=$1 WHERE id=900031"#)
        .bind(&image)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(reconcile().await.unwrap(), (1, 1));
    let repaired = row(900041).await.unwrap();
    assert_ne!(repaired["container_id"], starting.0);
    let starting = LiveService(repaired["container_id"].as_str().unwrap().to_owned());
    assert_eq!(repaired["published_image_ref"], image);

    let kind = ControlJobKind::AdReset;
    let job = control_jobs::enqueue(
        pool,
        kind,
        "ad-service:900041",
        fixture.game_id,
        Some(900031),
        Uuid::new_v4(),
        &"a".repeat(64),
        json!({"serviceId":900041,"participationId":900021,
            "expectedBackendId":starting.0,"playerPolicy":false}),
    )
    .await
    .unwrap();
    let claim = || control_jobs::claim_next(pool, kind, Duration::from_secs(60));
    let first = claim().await.unwrap().unwrap();
    let prepared = crate::services::ad_engine::prepare_service_reset(
        &state.db,
        fixture.game_id,
        900041,
        Some(&starting.0),
        job.id,
        first.lease_token,
        &image,
        "fixture restart",
    )
    .await
    .unwrap();
    let pending = row(900041).await.unwrap();
    assert_eq!(pending["host"], "");
    assert_eq!(pending["container_id"], starting.0);
    sqlx::query(r#"UPDATE "ControlPlaneJobs" SET lease_expires_at_utc=now()-interval '1 second' WHERE id=$1"#)
        .bind(job.id).execute(pool).await.unwrap();
    assert_eq!(reconcile().await.unwrap(), (0, 1));
    assert_eq!(row(900041).await.unwrap(), pending);
    assert_eq!(
        docker(&["inspect", "--format", "{{.State.Status}}", &starting.0]),
        "running"
    );
    // An organizer rebuild after retirement must not change the image this
    // operation launches or cause its already staged local image to be pruned.
    sqlx::raw_sql(
        r#"SET LOCAL rsctf.maintenance_writer='on';
           UPDATE "GameChallenges" SET build_image_digest='sha256:' || repeat('f',64) WHERE id=900031"#,
    )
    .execute(pool)
    .await
    .unwrap();
    let protected: bool = sqlx::query_scalar(r#"SELECT EXISTS(SELECT 1 FROM "AdProtectedLocalImages" WHERE challenge_id=900031 AND image_id=$1)"#)
        .bind(&image).fetch_one(pool).await.unwrap();
    assert!(protected);
    let recovered = claim().await.unwrap().unwrap();
    assert_ne!(recovered.lease_token, first.lease_token);
    // Fail publication after the replacement has started and its ownership
    // has been recorded. A reclaimed lease must publish the same instance.
    sqlx::raw_sql(
        r#"
        CREATE FUNCTION interrupt_publication() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN RAISE EXCEPTION 'fixture publication interrupted'; END $$;
        CREATE TRIGGER interrupt_publication BEFORE UPDATE ON "AdTeamServices"
        FOR EACH ROW WHEN (NEW.id=900041 AND OLD.host='' AND NEW.host<>'')
        EXECUTE FUNCTION interrupt_publication();
    "#,
    )
    .execute(pool)
    .await
    .unwrap();
    let error = super::execute_job(&state, &recovered).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("fixture publication interrupted"),
        "{error}"
    );
    let interrupted = row(900041).await.unwrap();
    let saved = interrupted["container_id"].as_str().unwrap();
    let receipt: String = sqlx::query_scalar(
        r#"SELECT input->>'replacementBackendId' FROM "ControlPlaneJobs" WHERE id=$1"#,
    )
    .bind(job.id)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(saved, receipt);
    assert_eq!(interrupted["host"], "");
    docker(&[
        "exec",
        saved,
        "sh",
        "-c",
        "printf team-data > /tmp/publication-note",
    ]);
    // On the next claim only the first write fails. The worker must finish
    // publication itself, without starting another container.
    sqlx::raw_sql(
        r#"CREATE SEQUENCE publication_attempt;
        CREATE OR REPLACE FUNCTION interrupt_publication() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN
            IF nextval('publication_attempt')=1 THEN
                RAISE EXCEPTION 'transient fixture publication failure';
            END IF;
            RETURN NEW;
        END $$;"#,
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(r#"UPDATE "ControlPlaneJobs" SET lease_expires_at_utc=now()-interval '1 second' WHERE id=$1"#)
        .bind(job.id).execute(pool).await.unwrap();
    let retry = claim().await.unwrap().unwrap();
    assert_ne!(retry.lease_token, recovered.lease_token);
    let result = super::execute_job(&state, &retry).await;
    shutdown.send_replace(true);
    capture.await.unwrap();
    let result = result.unwrap();
    assert_eq!(result["reset"], true);
    let writes: i64 = sqlx::query_scalar("SELECT last_value FROM publication_attempt")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(writes, 2);
    let restored = row(900041).await.unwrap();
    let replacement = LiveService(restored["container_id"].as_str().unwrap().to_owned());
    assert_eq!(replacement.0, saved);
    assert_eq!(
        docker(&["exec", saved, "cat", "/tmp/publication-note"]),
        "team-data"
    );
    assert_eq!(
        docker(&["inspect", "--format", "{{.Image}}", &replacement.0]),
        image
    );
    assert_eq!(restored["published_image_ref"], image);
    assert_eq!(restored["published_image_backend"], replacement.0);
    assert!(restored["previous_image_ref"].is_null());
    assert_ne!(replacement.0, starting.0);
    assert_eq!(restored["port"], 8080);
    assert!(!restored["host"].as_str().unwrap().is_empty());
    assert_eq!(
        docker(&["exec", &replacement.0, "printenv", "RSCTF_FLAG"]),
        prepared.current_flag.unwrap()
    );
    assert_eq!(row(900042).await.unwrap(), unrelated);
    assert_eq!(
        docker(&["inspect", "--format", "{{.State.Status}}", &other.0]),
        "running"
    );
    assert_eq!(reconcile().await.unwrap(), (0, 1));
    assert_eq!(row(900041).await.unwrap(), restored);
    runtime_pool.close().await;
    fixture.cleanup().await;
}
