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
            .repair_ad_volumes(&mixed.0, &mixed_spec)
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
