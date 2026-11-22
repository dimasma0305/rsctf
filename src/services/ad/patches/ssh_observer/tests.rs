use super::*;
use crate::services::ad::reset_fixture::{docker, docker_state, LiveService};
use crate::services::container::docker_installation_scope;
use crate::utils::test_database::TestDatabase;

#[tokio::test]
#[ignore = "requires disposable PostgreSQL and Docker with RSCTF_TEST_CONTAINER_IMAGE"]
async fn registered_edits_keep_separate_private_histories_in_docker() {
    let db = TestDatabase::new("hosted_ssh_history").await;
    sqlx::raw_sql(r#"CREATE TABLE "Games"(id INT PRIMARY KEY,deletion_pending BOOL,start_time_utc TIMESTAMPTZ,end_time_utc TIMESTAMPTZ);
        CREATE TABLE "GameChallenges"(id INT PRIMARY KEY,game_id INT,ad_self_hosted BOOL,deletion_pending BOOL);
        CREATE TABLE "AdTeamServices"(id INT PRIMARY KEY,game_id INT,participation_id INT,challenge_id INT,container_id TEXT);
        CREATE TABLE "AdPatchStarters"(challenge_id INT PRIMARY KEY,tree_id TEXT);
        INSERT INTO "Games" VALUES(1,false,now()-interval '1 hour',now()+interval '1 hour');
        INSERT INTO "GameChallenges" VALUES(3,1,false,false);"#).execute(&db.pool).await.unwrap();
    sqlx::raw_sql(crate::migrations::m0378_ad_ssh_sources::UP_SQL)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::raw_sql(crate::migrations::m0379_ad_ssh_recovery::UP_SQL)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "AdPatchStarters" VALUES(3,repeat('a',64),'[{"sourcePath":"game.py","containerPath":"/tmp/game.py","retainContent":true}]')"#)
        .execute(&db.pool).await.unwrap();
    let state = docker_state(&db.pool);
    let image = std::env::var("RSCTF_TEST_CONTAINER_IMAGE").unwrap();
    let scope = docker_installation_scope();
    let mut sessions = Vec::new();
    for team in [41, 42] {
        let container = LiveService(docker(&[
            "run",
            "-d",
            "--rm",
            "--network=none",
            "--no-healthcheck",
            "--user=65534:65534",
            "--cap-drop=ALL",
            "--tmpfs",
            "/team:rw,mode=1777,size=1m",
            "--label",
            &format!("rsctf.managed={scope}"),
            "--label",
            &format!("rsctf.scope={scope}"),
            "--entrypoint",
            "/bin/sleep",
            &image,
            "300",
        ]));
        docker(&[
            "exec",
            &container.0,
            "sh",
            "-c",
            "printf v1 >/tmp/game.py; printf private >/tmp/unregistered; printf present >/team/hidden",
        ]);
        sqlx::query(r#"INSERT INTO "AdTeamServices" VALUES($1,1,$1,3,$2)"#)
            .bind(team)
            .bind(&container.0)
            .execute(&db.pool)
            .await
            .unwrap();
        let session = Session {
            st: state.clone(),
            game: 1,
            participation: team,
            challenge: 3,
            service: team,
            backend: container.0.clone(),
        };
        assert!(state
            .containers
            .read_file(&container.0, "/team/hidden", 1024)
            .await
            .is_err_and(|error| matches!(error, AppError::BadRequest(_))));
        assert!(session.capture_current().await.unwrap());
        sessions.push((container, session));
    }
    let (a, session) = &sessions[0];
    for change in [
        "printf v2 >/tmp/game.py",
        "printf v3 >/tmp/game.py",
        "rm /tmp/game.py",
    ] {
        docker(&["exec", &a.0, "sh", "-c", change]);
        assert!(session.capture_current().await.unwrap());
        assert!(session.capture_current().await.unwrap());
    }
    let rows=sqlx::query_as::<_,(i32,String,String)>(r#"SELECT service_id,state,files FROM "AdHostedSshSourceHistory" ORDER BY service_id,revision"#)
        .fetch_all(&db.pool).await.unwrap();
    assert_eq!(rows.len(), 5);
    for (row, expected) in rows
        .iter()
        .zip([Some("v1"), Some("v2"), Some("v3"), None, Some("v1")])
    {
        assert_eq!(row.1, "observed");
        let file=expected.map(|value|serde_json::json!({"blob":hex::encode(git_object("blob",value.as_bytes())),"executable":false}));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&row.2).unwrap(),
            serde_json::json!({"game.py":file})
        );
    }
    assert_eq!(
        rows.iter().map(|r| r.0).collect::<Vec<_>>(),
        [41, 41, 41, 41, 42]
    );
    sqlx::query(r#"UPDATE "AdTeamServices" SET container_id=NULL WHERE id=41"#)
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(!session.capture_current().await.unwrap());
    drop(sessions.remove(0));
    let recovered = sqlx::query_as::<_, (i32, i64, String, Option<Vec<u8>>)>(
        r#"SELECT f.service_id,f.revision,f.path,b.payload
        FROM "AdSshRecoveryFiles" f LEFT JOIN "AdSshRecoveryBlobs" b USING(service_id,oid)
        ORDER BY f.service_id,f.revision"#,
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(recovered.len(), 5);
    for ((service, _, path, value), (team, expected)) in recovered.iter().zip([
        (41, Some("v1")),
        (41, Some("v2")),
        (41, Some("v3")),
        (41, None),
        (42, Some("v1")),
    ]) {
        assert_eq!(
            (*service, path.as_str(), value.as_deref()),
            (team, "game.py", expected.map(str::as_bytes))
        );
    }
    let (peer, session) = &sessions[0];
    docker(&[
        "exec",
        &peer.0,
        "sh",
        "-c",
        "printf private-fixture >/tmp/game.py && chmod 000 /tmp/game.py",
    ]);
    assert!(session.capture_current().await.unwrap());
    let protected = sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS(SELECT 1 FROM "AdSshRecoveryBlobs" WHERE service_id=42 AND payload=$1)"#,
    )
    .bind(b"private-fixture".as_slice())
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(!protected);
    docker(&["exec", &peer.0, "chmod", "644", "/tmp/game.py"]);
    for number in 0..10 {
        docker(&[
            "exec",
            &peer.0,
            "sh",
            "-c",
            "printf '%s' \"$1\" >/tmp/game.py",
            "fixture",
            &number.to_string(),
        ]);
        assert!(session.capture_current().await.unwrap());
    }
    let remaining = sqlx::query_as::<_, (i64, i64, i64)>(
        r#"SELECT
       (SELECT count(*) FROM "AdSshRecovery" WHERE service_id=42),
       (SELECT count(*) FROM "AdSshRecoveryBlobs" WHERE service_id=42),
       (SELECT count(*) FROM "AdSshRecovery" WHERE service_id=41)"#,
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(remaining, (8, 8, 4));
    sqlx::query(r#"DELETE FROM "AdHostedSshSourceHistory" WHERE service_id=42"#)
        .execute(&db.pool)
        .await
        .unwrap();
    let orphans = sqlx::query_scalar::<_, i64>(
        r#"SELECT count(*) FROM "AdSshRecoveryBlobs" WHERE service_id=42"#,
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(orphans, 0);
    drop(sessions);
    db.close().await;
}
