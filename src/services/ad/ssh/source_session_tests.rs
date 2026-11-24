use super::*;
use crate::services::ad::reset_fixture::{docker, docker_state, LiveService};
use crate::services::ad::scoring::test_fixture::ad_scoring_fixture;
use crate::services::container::docker_installation_scope;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Stdio;

pub(crate) async fn enroll(db: &sqlx::PgPool, participant: i32, key: &Path) {
    let output = std::process::Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(key)
        .output()
        .unwrap();
    assert!(output.status.success());
    let public = std::fs::read_to_string(key.with_extension("pub")).unwrap();
    let parsed = russh::keys::ssh_key::PublicKey::from_openssh(&public).unwrap();
    sqlx::query(r#"INSERT INTO "AdSshKeys"(participation_id,algorithm,public_key,fingerprint,platform_generated,created_at_utc)
        VALUES($1,'ssh-ed25519',$2,$3,false,now())"#).bind(participant).bind(public.trim())
        .bind(parsed.fingerprint(HashAlg::Sha256).to_string()).execute(db).await.unwrap();
}

pub(crate) struct LocalBastion {
    listener: TcpListener,
    config: Arc<russh::server::Config>,
    known_hosts: std::path::PathBuf,
}
impl LocalBastion {
    pub(crate) async fn new(st: &SharedState, dir: &Path) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let host_key = load_host_key(st).await;
        let known_hosts = dir.join("known_hosts");
        std::fs::write(
            &known_hosts,
            format!(
                "[127.0.0.1]:{port} {}\n",
                host_key.public_key().to_openssh().unwrap()
            ),
        )
        .unwrap();
        let config = Arc::new(russh::server::Config {
            keys: vec![host_key],
            ..Default::default()
        });

        Self {
            listener,
            config,
            known_hosts,
        }
    }
    pub(crate) async fn session(
        &self,
        st: &SharedState,
        key: &Path,
        script: &str,
    ) -> std::process::Output {
        let (_shutdown, stop) = watch::channel(false);
        let listener = &self.listener;
        let port = listener.local_addr().unwrap().port();
        let config = &self.config;
        let known_hosts = &self.known_hosts;
        let mut client = tokio::process::Command::new("ssh");
        client
            .args([
                "-F",
                "/dev/null",
                "-tt",
                "-oBatchMode=yes",
                "-oIdentitiesOnly=yes",
                "-oStrictHostKeyChecking=yes",
                "-oConnectTimeout=5",
                "-oGlobalKnownHostsFile=/dev/null",
            ])
            .arg(format!("-oUserKnownHostsFile={}", known_hosts.display()))
            .arg("-p")
            .arg(port.to_string())
            .arg("-i")
            .arg(key)
            .arg("900031@127.0.0.1")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut client = client.spawn().unwrap();
        let (socket, peer) = tokio::time::timeout(Duration::from_secs(8), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let handler = new_bastion_handler(
            st.clone(),
            peer.ip(),
            acquire_connection_slot(peer.ip()).unwrap(),
        );
        let session = tokio::spawn(serve_connection(config.clone(), socket, handler, stop));
        let mut input = client.stdin.take().unwrap();
        input.write_all(script.as_bytes()).await.unwrap();
        // Keep the stream open until the remote shell exits.
        let output = tokio::time::timeout(Duration::from_secs(15), client.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        drop(input);
        tokio::time::timeout(Duration::from_secs(12), session)
            .await
            .unwrap()
            .unwrap();
        output
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires disposable PostgreSQL, Docker with RSCTF_TEST_CONTAINER_IMAGE and OpenSSH"]
async fn hosted_team_ssh_disconnect_records_only_its_registered_source() {
    let fixture = ad_scoring_fixture().await;
    let db = &fixture.pool;
    let st = docker_state(db);
    sqlx::raw_sql(r#"UPDATE "Games" SET end_time_utc=now()+interval '1 hour' WHERE id=900001;
        UPDATE "GameChallenges" SET ad_self_hosted=false WHERE id=900031;
        INSERT INTO "AdPatchStarters"(challenge_id,tree_id,archive,ssh_sources)
          VALUES(900031,repeat('a',64),'fixture','[{"sourcePath":"game.py","containerPath":"/tmp/game.py"}]')"#)
        .execute(db).await.unwrap();
    sqlx::query(r#"UPDATE "AspNetUsers" SET role=$1 WHERE id IN (SELECT captain_id FROM "Teams" WHERE id IN (900011,900012))"#)
        .bind(Role::User as i16).execute(db).await.unwrap();
    struct KeyDir(std::path::PathBuf);
    impl Drop for KeyDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let tmp = KeyDir(std::env::temp_dir().join(format!("rsctf-ssh-{}", uuid::Uuid::new_v4())));
    let mut directory = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut directory, 0o700);
    directory.create(&tmp.0).unwrap();
    let scope = docker_installation_scope();
    let image = std::env::var("RSCTF_TEST_CONTAINER_IMAGE").unwrap();
    let mut services = Vec::new();
    for i in 0..2 {
        let key = tmp.0.join(format!("team{i}"));
        enroll(db, 900021 + i, &key).await;
        let container = LiveService(docker(&[
            "run",
            "-d",
            "--rm",
            "--network=none",
            "--no-healthcheck",
            "--user=65534:65534",
            "--cap-drop=ALL",
            "--label",
            &format!("rsctf.managed={scope}"),
            "--label",
            &format!("rsctf.scope={scope}"),
            "--entrypoint",
            "/bin/sleep",
            &image,
            "300",
        ]));
        let service = 900041 + i;
        sqlx::query(r#"UPDATE "AdTeamServices" SET container_id=$1 WHERE id=$2"#)
            .bind(&container.0)
            .bind(service)
            .execute(db)
            .await
            .unwrap();
        services.push((service, key, container));
    }
    let bastion = LocalBastion::new(&st, &tmp.0).await;
    for (team, value) in [(0, "first"), (1, "peer"), (0, "second")] {
        let (service, key, _) = &services[team];
        let output = bastion
            .session(
                &st,
                key,
                &format!("printf {value} > /tmp/game.py\nprintf skip > /tmp/unregistered\nexit\n"),
            )
            .await;
        let oid = hex::encode(Sha256::digest(format!("blob {}\0{value}", value.len())));
        tokio::time::timeout(Duration::from_secs(12),async {
            loop {
                let found=sqlx::query_scalar::<_,bool>(r#"SELECT EXISTS(SELECT 1 FROM "AdHostedSshSourceHistory"
                     WHERE service_id=$1 AND files::jsonb->'game.py'->>'blob'=$2 AND files::jsonb - 'game.py' = '{}'::jsonb)"#)
                    .bind(service).bind(&oid).fetch_one(db).await.unwrap();
                if found {break}
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }).await.unwrap_or_else(|_|panic!("team {team} history did not record the shell edit: {}",String::from_utf8_lossy(&output.stderr)));
    }
    let distinct=sqlx::query_scalar::<_,i64>(r#"SELECT COUNT(*) FROM "AdHostedSshSourceHistory" a JOIN "AdHostedSshSourceHistory" b
        ON a.service_id=900041 AND b.service_id=900042 AND a.files=b.files WHERE a.files::jsonb->'game.py'->>'blob' IS NOT NULL"#)
        .fetch_one(db).await.unwrap();
    assert_eq!(distinct, 0);
    drop(services);
    fixture.cleanup().await;
}
