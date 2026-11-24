use super::reset_fixture::{docker, docker_state, LiveService};
use super::scoring::test_fixture::ad_scoring_fixture;
use std::process::Command;

struct TestImage(String);
impl Drop for TestImage {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["image", "rm", &self.0])
            .output();
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL, Docker and the CI static-flag-service image"]
async fn docker_repair_preserves_each_teams_anonymous_data_volume() {
    let base = std::env::var("RSCTF_TEST_CONTAINER_IMAGE").unwrap();
    let template = LiveService(docker(&["create", &base]));
    let image = TestImage(docker(&["commit", "--change", "VOLUME /data", &template.0]));
    let fixture = ad_scoring_fixture().await;
    let pool = &fixture.pool;
    let runtime_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(12)
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let state = docker_state(&runtime_pool);
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(
        r#"SET LOCAL rsctf.maintenance_writer='on';
        UPDATE "Games" SET end_time_utc=now()+interval '1 hour' WHERE id=900001;
        UPDATE "AdRounds" SET finalized=FALSE WHERE id=900108;"#,
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        r#"UPDATE "GameChallenges" SET ad_self_hosted=FALSE,container_image='fixture',
        build_status=1,build_image_digest=$1,expose_port=8080 WHERE id=900031"#,
    )
    .bind(&image.0)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let game = crate::services::game_config::load_game(&state, fixture.game_id)
        .await
        .unwrap();
    let reconcile =
        || super::provision::ensure_ad_containers(&state, &game, None, false, false, false, None);
    let id = |service: i32| {
        sqlx::query_scalar::<_, String>(r#"SELECT container_id FROM "AdTeamServices" WHERE id=$1"#)
            .bind(service)
            .fetch_one(pool)
    };
    let volume = |container: &str| {
        docker(&[
            "inspect",
            "--format",
            "{{(index .Mounts 0).Name}}",
            container,
        ])
    };
    let (shutdown, rx) = tokio::sync::watch::channel(false);
    let capture = crate::services::traffic::start_capture_reconciler(state.clone(), rx);
    assert_eq!(reconcile().await.unwrap(), (2, 0));
    let first = LiveService(id(900041).await.unwrap());
    let peer = LiveService(id(900042).await.unwrap());
    let data = volume(&first.0);
    assert_ne!(data, volume(&peer.0));
    docker(&[
        "exec",
        "-u",
        "0",
        &first.0,
        "sh",
        "-c",
        "echo first >/data/marker; touch /tmp/ephemeral",
    ]);
    docker(&[
        "exec",
        "-u",
        "0",
        &peer.0,
        "sh",
        "-c",
        "echo peer >/data/marker",
    ]);
    docker(&["stop", "-t", "1", &first.0]);
    // A daemon restart must never bring up a second writer to the same data.
    docker(&["update", "--restart=always", &first.0]);
    assert_eq!(reconcile().await.unwrap(), (0, 1));
    assert_eq!(id(900041).await.unwrap(), first.0);
    assert_eq!(volume(&first.0), data);
    docker(&["update", "--restart=no", &first.0]);

    // Fail after Docker starts the successor. The donor still owns the state.
    sqlx::raw_sql(
        r#"CREATE FUNCTION refuse_repair() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN RAISE EXCEPTION 'fixture refuses publication'; END $$;
        CREATE TRIGGER refuse_repair BEFORE UPDATE ON "AdTeamServices" FOR EACH ROW
        WHEN (OLD.id=900041 AND OLD.host='' AND NEW.host<>'') EXECUTE FUNCTION refuse_repair()"#,
    )
    .execute(pool)
    .await
    .unwrap();
    assert_eq!(reconcile().await.unwrap(), (0, 1));
    assert_eq!(id(900041).await.unwrap(), first.0);
    assert_eq!(volume(&first.0), data);
    sqlx::query(r#"DROP TRIGGER refuse_repair ON "AdTeamServices""#)
        .execute(pool)
        .await
        .unwrap();
    // Installations predating image receipts still have the exact service ID.
    sqlx::query(r#"UPDATE "AdTeamServices" SET published_image_ref=NULL, published_image_backend=NULL WHERE id=900041"#)
        .execute(pool).await.unwrap();
    let mut previous = first.0.clone();
    let mut generations = Vec::new();
    let flag: String = sqlx::query_scalar(
        r#"SELECT flag FROM "AdFlags" WHERE round_id=900108 AND team_service_id=900041"#,
    )
    .fetch_one(pool)
    .await
    .unwrap();
    // A second generation also inherits an anonymous volume from its donor.
    for i in 0..2 {
        assert_eq!(reconcile().await.unwrap(), (1, 0));
        let replacement = LiveService(id(900041).await.unwrap());
        assert_ne!(replacement.0, previous);
        assert_eq!(volume(&replacement.0), data);
        assert_eq!(
            docker(&["exec", &replacement.0, "cat", "/data/marker"]),
            "first"
        );
        docker(&["exec", &replacement.0, "test", "!", "-e", "/tmp/ephemeral"]);
        assert!(docker(&["exec", &replacement.0, "printenv", "RSCTF_FLAG"]) == flag);
        assert!(docker(&["exec", &replacement.0, "cat", "/flag"]) == flag);
        assert_eq!(id(900042).await.unwrap(), peer.0);
        assert_eq!(docker(&["exec", &peer.0, "cat", "/data/marker"]), "peer");
        assert!(!Command::new("docker")
            .args(["inspect", &previous])
            .output()
            .unwrap()
            .status
            .success());
        if i == 0 {
            docker(&["stop", "-t", "1", &replacement.0]);
            previous = replacement.0.clone();
        } else {
            reset_running_service_to_starter(&state, &peer.0, &base, &template.0).await;
            assert_eq!(id(900041).await.unwrap(), replacement.0);
            assert_eq!(
                docker(&["exec", &replacement.0, "cat", "/data/marker"]),
                "first"
            );
            state.containers.destroy(&replacement.0).await.unwrap();
        }
        generations.push(replacement);
    }
    assert!(!Command::new("docker")
        .args(["volume", "inspect", &data])
        .output()
        .unwrap()
        .status
        .success());

    // VolumesFrom inherits *all* mounts. A container with an unregistered
    // second volume must never contribute even its recognized data volume.
    use crate::services::container::{ContainerResourceLimits, ContainerSpec};
    let mixed_image = TestImage(docker(&[
        "commit",
        "--change",
        "VOLUME /data /private",
        &template.0,
    ]));
    let mixed_spec = ContainerSpec::ad_service(
        mixed_image.0.clone(),
        ContainerResourceLimits {
            memory_limit: 128,
            cpu_count: 1,
            storage_limit: 256,
        },
        8080,
        1,
        false,
        flag,
    );
    let mixed = LiveService(
        state
            .containers
            .create(mixed_spec.clone())
            .await
            .unwrap()
            .id,
    );
    docker(&["stop", "-t", "1", &mixed.0]);
    assert!(state
        .containers
        .has_ad_data_volumes(&mixed.0)
        .await
        .unwrap());
    assert!(matches!(
        state
            .containers
            .repair_ad_volumes(
                &mixed.0,
                &mixed_spec,
                crate::services::container::AdVolumeChange::RepairStopped
            )
            .await,
        Err(crate::utils::error::AppError::Conflict(_))
    ));
    assert_eq!(
        docker(&["inspect", "--format", "{{len .Mounts}}", &mixed.0]),
        "2"
    );
    state.containers.destroy(&mixed.0).await.unwrap();
    shutdown.send_replace(true);
    capture.await.unwrap();
    runtime_pool.close().await;
    fixture.cleanup().await;
}

async fn reset_running_service_to_starter(
    state: &crate::app_state::SharedState,
    peer: &str,
    incompatible: &str,
    template: &str,
) {
    use crate::services::control_jobs::{self, ControlJobKind};
    use serde_json::json;
    use std::time::Duration;
    let pool = state.pg();
    let target = TestImage(docker(&[
        "commit",
        "--change",
        "VOLUME /data",
        "--change",
        "ENV RSCTF_RESET_FIXTURE=starter",
        template,
    ]));
    let volume = |id: &str| docker(&["inspect", "--format", "{{(index .Mounts 0).Name}}", id]);
    let original_data = volume(peer);
    sqlx::query(r#"UPDATE "GameChallenges" SET ad_allow_self_reset=TRUE WHERE id=900031"#)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "AdPatchStarters"(challenge_id,tree_id,archive)
        VALUES(900031,repeat('a',64),'fixture'::bytea)"#,
    )
    .execute(pool)
    .await
    .unwrap();
    // The fixture game is already live. Seed the image receipt normally
    // recorded when a starter is registered before the game.
    sqlx::query(r#"UPDATE "AdPatchStarters" SET runtime_image_ref=$1 WHERE challenge_id=900031"#)
        .bind(incompatible)
        .execute(pool)
        .await
        .unwrap();
    let kind = ControlJobKind::AdReset;
    let job = control_jobs::enqueue(pool, kind, "ad-service:900042",900001,Some(900031),uuid::Uuid::new_v4(),&"d".repeat(64),
        json!({"serviceId":900042,"participationId":900022,"expectedBackendId":peer,"playerPolicy":true})).await.unwrap();
    let claim = || control_jobs::claim_next(pool, kind, Duration::from_secs(120));
    let claimed = claim().await.unwrap().unwrap();
    let snapshot = || {
        sqlx::query_scalar::<_, serde_json::Value>(
            r#"SELECT jsonb_build_array(to_jsonb(s),to_jsonb(c),j.input) FROM "AdTeamServices" s
        JOIN "AdCheckResults" c ON c.team_service_id=s.id AND c.round_id=900108
        CROSS JOIN "ControlPlaneJobs" j WHERE s.id=900042 AND j.id=$1"#,
        )
        .bind(job.id)
        .fetch_one(pool)
    };
    let before = snapshot().await.unwrap();
    let mismatch = super::reset::execute_job(state, &claimed)
        .await
        .unwrap_err();
    assert!(
        matches!(mismatch, crate::utils::error::AppError::Conflict(_)),
        "{mismatch}"
    );
    assert_eq!(snapshot().await.unwrap(), before);
    assert_eq!(docker(&["exec", peer, "cat", "/data/marker"]), "peer");
    sqlx::query(r#"UPDATE "AdPatchStarters" SET runtime_image_ref=$1 WHERE challenge_id=900031"#)
        .bind(&target.0)
        .execute(pool)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"CREATE FUNCTION refuse_volume_receipt() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN RAISE EXCEPTION 'fixture rejects receipt'; END $$;
        CREATE TRIGGER refuse_volume_receipt BEFORE UPDATE ON "ControlPlaneJobs" FOR EACH ROW
        WHEN (NEW.input ? 'replacementBackendId') EXECUTE FUNCTION refuse_volume_receipt()"#,
    )
    .execute(pool)
    .await
    .unwrap();
    let failure = super::reset::execute_job(state, &claimed)
        .await
        .unwrap_err();
    assert!(
        failure.to_string().contains("fixture rejects receipt"),
        "{failure}"
    );
    let pending = snapshot().await.unwrap();
    assert_eq!(pending[0]["host"], "");
    assert_eq!(pending[0]["container_id"], peer);
    assert_eq!(pending[2]["preserveDataVolumes"], true);
    assert!(pending[2].get("replacementBackendId").is_none());
    assert_eq!(volume(peer), original_data);
    let candidate = LiveService(docker(&[
        "ps",
        "-q",
        "--no-trunc",
        "--filter",
        &format!("label=rsctf.operation=ad-volume-repair:{peer}"),
    ]));
    assert_eq!(candidate.0.len(), 64);
    assert_eq!(volume(&candidate.0), original_data);
    sqlx::query(r#"DROP TRIGGER refuse_volume_receipt ON "ControlPlaneJobs""#)
        .execute(pool)
        .await
        .unwrap();
    // Recovery must use the prepared image even if an organizer replaces
    // the challenge starter while the reset worker is offline.
    sqlx::query(r#"UPDATE "AdPatchStarters" SET runtime_image_ref=$1 WHERE challenge_id=900031"#)
        .bind(incompatible)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(r#"UPDATE "ControlPlaneJobs" SET lease_expires_at_utc=now()-interval '1 second' WHERE id=$1"#)
        .bind(job.id).execute(pool).await.unwrap();
    let reclaimed = claim().await.unwrap().unwrap();
    assert_eq!(
        super::reset::execute_job(state, &reclaimed).await.unwrap()["reset"],
        true
    );
    let published = snapshot().await.unwrap();
    assert_eq!(published[0]["container_id"], candidate.0);
    assert_eq!(published[2]["replacementBackendId"], candidate.0);
    assert_eq!(published[0]["published_image_ref"], target.0);
    assert_eq!(
        docker(&["exec", &candidate.0, "cat", "/data/marker"]),
        "peer"
    );
    assert_eq!(
        docker(&["exec", &candidate.0, "printenv", "RSCTF_RESET_FIXTURE"]),
        "starter"
    );
    let flag: String = sqlx::query_scalar(
        r#"SELECT flag FROM "AdFlags" WHERE round_id=900108 AND team_service_id=900042"#,
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert!(docker(&["exec", &candidate.0, "printenv", "RSCTF_FLAG"]) == flag);
    assert!(docker(&["exec", &candidate.0, "cat", "/flag"]) == flag);
    assert!(!Command::new("docker")
        .args(["inspect", peer])
        .output()
        .unwrap()
        .status
        .success());
    assert!(control_jobs::complete(
        pool,
        job.id,
        reclaimed.lease_token,
        reclaimed.input_revision,
        json!({"reset":true}),
    )
    .await
    .unwrap());
    // Restoring the previous program image must keep writes made after the
    // starter reset. A volume snapshot would silently lose these changes.
    docker(&[
        "exec",
        "-u",
        "0",
        &candidate.0,
        "sh",
        "-c",
        "echo after-starter >/data/marker",
    ]);
    let prior = published[0]["previous_image_ref"].as_str().unwrap();
    sqlx::query(
        r#"UPDATE "AdTeamServices" SET last_reset_at=now()-interval '1 day' WHERE id=900042"#,
    )
    .execute(pool)
    .await
    .unwrap();
    let rollback_kind = ControlJobKind::AdRollback;
    let rollback = control_jobs::enqueue(
        pool,
        rollback_kind,
        "ad-service:900042",
        900001,
        Some(900031),
        uuid::Uuid::new_v4(),
        &"e".repeat(64),
        json!({
            "serviceId":900042, "participationId":900022, "expectedBackendId":candidate.0,
            "playerPolicy":true, "previousImageRef":prior,
        }),
    )
    .await
    .unwrap();
    let claimed = control_jobs::claim_next(pool, rollback_kind, Duration::from_secs(120))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.model.id, rollback.id);
    assert_eq!(
        super::reset::execute_job(state, &claimed).await.unwrap()["reset"],
        true
    );
    let restored = snapshot().await.unwrap();
    let running = LiveService(restored[0]["container_id"].as_str().unwrap().to_owned());
    assert_eq!(restored[0]["previous_image_ref"], target.0);
    assert!(docker(&["exec", &running.0, "cat", "/flag"]) == flag);
    assert_eq!(
        docker(&["inspect", "--format", "{{.Image}}", &running.0]),
        prior
    );
    assert_eq!(volume(&running.0), original_data);
    assert_eq!(
        docker(&["exec", &running.0, "cat", "/data/marker"]),
        "after-starter"
    );
    assert_eq!(
        docker(&["exec", &running.0, "printenv", "RSCTF_FLAG"]),
        flag
    );
    // A stale value on disk must never count as an acknowledgement when the
    // challenge user cannot write the file.
    docker(&[
        "exec",
        "-u",
        "0",
        &running.0,
        "sh",
        "-c",
        "echo stale >/flag && chmod 0444 /flag",
    ]);
    let official = super::engine::ProvisionFlag::read(pool, 900001, 900022, 900031)
        .await
        .unwrap();
    assert!(official.official.as_deref() == Some(flag.as_str()));
    assert!(official
        .initialize(state, 900022, 900031, &running.0, Some(&running.0))
        .await
        .is_err());
    assert_eq!(docker(&["exec", &running.0, "cat", "/flag"]), "stale");
    state.containers.destroy(&running.0).await.unwrap();
    assert!(!Command::new("docker")
        .args(["volume", "inspect", &original_data])
        .output()
        .unwrap()
        .status
        .success());
}
