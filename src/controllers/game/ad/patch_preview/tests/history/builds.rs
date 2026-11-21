use super::*;

fn archive(dockerfile: &[u8], payload_bytes: usize) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file("Dockerfile", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(dockerfile).unwrap();
    if payload_bytes > 0 {
        let mut payload = vec![0; payload_bytes];
        rand::fill(&mut payload[..]);
        zip.start_file("payload", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&payload).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn path(revision: &Value) -> String {
    format!(
        "patch-revisions/{}/build?service=30&commit={}",
        revision["revision"],
        revision["commitId"].as_str().unwrap()
    )
}

async fn build_request(
    router: &Router,
    method: &str,
    route: &str,
    owner: &CurrentUser,
) -> Response {
    let mut reply = request(router, method, 10, route, Some(owner), Body::empty()).await;
    if reply.status() == StatusCode::TOO_MANY_REQUESTS {
        let seconds: u64 = reply.headers()[header::RETRY_AFTER]
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!(seconds <= 10);
        tokio::time::sleep(std::time::Duration::from_secs(seconds + 1)).await;
        reply = request(router, method, 10, route, Some(owner), Body::empty()).await;
    }
    reply
}

pub(super) async fn check(
    pool: &PgPool,
    router: &Router,
    owner: &CurrentUser,
    outsider: &CurrentUser,
) {
    for _ in 0..2 {
        sqlx::raw_sql(crate::migrations::m0370_ad_patch_build_queue::UP_SQL)
            .execute(pool)
            .await
            .unwrap();
    }
    // A pending team deletion fences both source export and candidate publication.
    // Other eligible teams in the same challenge keep their own queue.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query(r#"UPDATE "Teams" SET deletion_pending=true WHERE id=30"#)
        .execute(&mut *tx)
        .await
        .unwrap();
    let visible: Vec<i32> = sqlx::query_scalar(
        r#"SELECT service_id FROM "AdPatchBuildEligibleServices" WHERE service_id IN (20,30)"#,
    )
    .fetch_all(&mut *tx)
    .await
    .unwrap();
    assert_eq!(visible, [20]);
    tx.rollback().await.unwrap();
    let good = path(
        &json(
            save_to(
                router,
                owner,
                30,
                5,
                &archive(b"FROM scratch\nCOPY payload /version\n", 1200 * 1024),
            )
            .await,
        )
        .await,
    );
    let bad = path(
        &json(
            save_to(
                router,
                owner,
                30,
                6,
                &archive(b"FROM scratch\nRUN [\"/missing\"]\n", 0),
            )
            .await,
        )
        .await,
    );
    let read = |route| build_request(router, "GET", route, owner);
    let post = |route| build_request(router, "POST", route, owner);
    assert_eq!(json(read(&good).await).await["available"], false);
    assert_eq!(post(&good).await.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        request(router, "POST", 10, &good, Some(outsider), Body::empty())
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(router, "GET", 10, &good, None, Body::empty())
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );

    let mut worker = None;
    let mut worker_command = None;
    let mut simulated = None;
    if let Ok(guest) = std::env::var("RSCTF_PATCH_VM_GUEST") {
        let schema: String = sqlx::query_scalar("SELECT current_schema()")
            .fetch_one(pool)
            .await
            .unwrap();
        let mut url = url::Url::parse(&std::env::var("RSCTF_TEST_DATABASE_URL").unwrap()).unwrap();
        url.query_pairs_mut()
            .append_pair("options", &format!("-csearch_path={schema}"));
        let mut command =
            tokio::process::Command::new(std::env::var("RSCTF_PATCH_WORKER_PYTHON").unwrap());
        command
            .args([
                "scripts/ad-patch-builder/worker.py",
                "--challenge",
                "10",
                "--guest",
                &guest,
                "--rsctf",
                &std::env::var("RSCTF_PATCH_TEST_BINARY").unwrap(),
            ])
            .env("RSCTF_DATABASE_URL", url.as_str())
            .kill_on_drop(true);
        worker = Some(command.spawn().unwrap());
        worker_command = Some((command, url));
    } else {
        let mut db = pool.acquire().await.unwrap();
        sqlx::query("SELECT pg_advisory_lock(1734271011,10)")
            .execute(&mut *db)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "AdPatchBuilders" VALUES(10,pg_backend_pid(),repeat('a',64))"#)
            .execute(&mut *db)
            .await
            .unwrap();
        simulated = Some(db);
    }
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        while !online(pool).await {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(json(read(&good).await).await["available"], true);
    let teammate = CurrentUser {
        id: Uuid::new_v4(),
        ..owner.clone()
    };
    for sql in [
        r#"INSERT INTO "AspNetUsers" VALUES ($1,1,true,'stamp')"#,
        r#"INSERT INTO "TeamMembers" VALUES (30,$1)"#,
        r#"INSERT INTO "UserParticipations" VALUES ($1,1,30,30)"#,
        r#"INSERT INTO "IdentityObservations" VALUES ($1,1,30,30,now())"#,
    ] {
        sqlx::query(sql)
            .bind(teammate.id)
            .execute(pool)
            .await
            .unwrap();
    }
    let (captain_reply, member_reply) =
        tokio::join!(post(&good), build_request(router, "POST", &good, &teammate));
    let submitted = json(captain_reply).await;
    let concurrent = json(member_reply).await;
    assert_eq!(submitted["attempt"], concurrent["attempt"]);
    assert!(submitted["status"] == "Queued" || concurrent["status"] == "Queued");
    assert_eq!(
        json(post(&good).await).await["attempt"],
        submitted["attempt"]
    );
    assert_eq!(post(&bad).await.status(), StatusCode::CONFLICT);
    if let Some(mut worker) = worker {
        wait_status(pool, 6, "Building").await;
        let (attempt, old_claim): (Uuid, Uuid) = sqlx::query_as(
            r#"SELECT attempt,claim FROM "AdPatchBuilds" WHERE service_id=30 AND revision=6"#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        worker.kill().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while online(pool).await {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap();
        assert!(
            crate::services::ad::patches::claimed_source(pool, attempt, old_claim)
                .await
                .is_err()
        );
        let (mut command, url) = worker_command.unwrap();
        let mut worker = command.spawn().unwrap();
        wait_status(pool, 6, "Ready").await;
        assert!(
            crate::services::ad::patches::claimed_source(pool, attempt, old_claim)
                .await
                .is_err()
        );
        let digest = sqlx::query_scalar::<_, String>(
            r#"SELECT digest FROM "AdPatchCandidates" WHERE service_id=30"#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let parts: Vec<Vec<u8>> = sqlx::query_scalar(
            r#"SELECT payload FROM "AdPatchCandidateChunks" WHERE service_id=30 ORDER BY part"#,
        )
        .fetch_all(pool)
        .await
        .unwrap();
        use sha2::Digest;
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].len(), 1024 * 1024);
        assert_eq!(hex::encode(sha2::Sha256::digest(parts.concat())), digest);
        let exported =
            std::env::temp_dir().join(format!("rsctf-patch-test-{}.oci.tar", Uuid::new_v4()));
        let mut export =
            tokio::process::Command::new(std::env::var("RSCTF_PATCH_WORKER_PYTHON").unwrap());
        export
            .args([
                "scripts/ad-patch-builder/export.py",
                "--service",
                "30",
                "--revision",
                "6",
                "--attempt",
            ])
            .arg(attempt.to_string())
            .arg("--output")
            .arg(&exported)
            .env("RSCTF_DATABASE_URL", url.as_str())
            .kill_on_drop(true);
        let result = export.output().await.unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            String::from_utf8(result.stdout).unwrap().trim(),
            format!("sha256:{digest}")
        );
        assert_eq!(tokio::fs::read(&exported).await.unwrap(), parts.concat());
        tokio::fs::remove_file(&exported).await.unwrap();
        let failed = json(post(&bad).await).await;
        wait_status(pool, 7, "Failed").await;
        // A replay cannot re-run a failed Dockerfile unless the requester
        // explicitly retries the previously observed attempt.
        assert_eq!(json(post(&bad).await).await["attempt"], failed["attempt"]);
        let retry = format!(
            "{bad}&previousAttempt={}",
            failed["attempt"].as_str().unwrap()
        );
        assert_ne!(
            json(build_request(router, "POST", &retry, owner).await).await["attempt"],
            failed["attempt"]
        );
        assert_eq!(json(read(&good).await).await["status"], "Ready");
        wait_status(pool, 7, "Failed").await;
        let replacement = path(
            &json(
                save_to(
                    router,
                    owner,
                    30,
                    7,
                    &archive(b"FROM scratch\nCOPY payload /new-version\n", 1200 * 1024),
                )
                .await,
            )
            .await,
        );
        sqlx::raw_sql(
            r#"
            CREATE FUNCTION fail_second_chunk() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN RAISE EXCEPTION 'fixture chunk write failed'; END $$;
            CREATE TRIGGER fail_patch_write BEFORE INSERT ON "AdPatchCandidateChunks"
            FOR EACH ROW WHEN (NEW.part = 1) EXECUTE FUNCTION fail_second_chunk();
        "#,
        )
        .execute(pool)
        .await
        .unwrap();
        let rejected = json(build_request(router, "POST", &replacement, owner).await).await;
        assert_eq!(rejected["status"], "Queued");
        wait_status(pool, 8, "Failed").await;
        let retained: (i32, String) = sqlx::query_as(
            r#"SELECT revision,digest FROM "AdPatchCandidates" WHERE service_id=30"#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(retained, (6, digest));
        let surviving: Vec<Vec<u8>> = sqlx::query_scalar(
            r#"SELECT payload FROM "AdPatchCandidateChunks" WHERE service_id=30 ORDER BY part"#,
        )
        .fetch_all(pool)
        .await
        .unwrap();
        assert_eq!(surviving, parts);
        assert_eq!(json(read(&good).await).await["status"], "Ready");
        sqlx::query(r#"DROP TRIGGER fail_patch_write ON "AdPatchCandidateChunks""#)
            .execute(pool)
            .await
            .unwrap();
        let retry = format!(
            "{replacement}&previousAttempt={}",
            rejected["attempt"].as_str().unwrap()
        );
        assert_eq!(
            json(build_request(router, "POST", &retry, owner).await).await["status"],
            "Queued"
        );
        wait_status(pool, 8, "Ready").await;
        assert_eq!(
            json(build_request(router, "GET", &replacement, owner).await).await["status"],
            "Ready"
        );
        assert_eq!(json(read(&good).await).await["status"], "Replaced");
        assert!(!export.output().await.unwrap().status.success());
        assert!(!exported.exists());
        let cached: i64 =
            sqlx::query_scalar(r#"SELECT count(*) FROM "AdPatchCandidates" WHERE service_id=30"#)
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(cached, 1);
        worker.kill().await.unwrap();
    }
    if let Some(mut db) = simulated {
        sqlx::query("SELECT pg_advisory_unlock(1734271011,10)")
            .execute(&mut *db)
            .await
            .unwrap();
    }
}

async fn online(pool: &PgPool) -> bool {
    sqlx::query_scalar(
        r#"SELECT EXISTS(SELECT 1 FROM "AdPatchOnlineBuilders" WHERE challenge_id=10)"#,
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn wait_status(pool: &PgPool, revision: i32, status: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(100), async {
        loop {
            let current: String = sqlx::query_scalar(
                r#"SELECT status FROM "AdPatchBuilds" WHERE service_id=30 AND revision=$1"#,
            )
            .bind(revision)
            .fetch_one(pool)
            .await
            .unwrap();
            if current == status {
                break;
            }
            assert_ne!(current, "Failed", "unexpected build failure");
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    })
    .await
    .unwrap();
}
