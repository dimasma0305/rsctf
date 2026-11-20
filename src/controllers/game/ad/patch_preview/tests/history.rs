use super::*;
use serde_json::Value;
use std::io::Read;

async fn save(router: &Router, user: &CurrentUser, expected: i32, zip: &[u8]) -> Response {
    save_to(router, user, 20, expected, zip).await
}

async fn save_to(
    router: &Router,
    user: &CurrentUser,
    service: i32,
    expected: i32,
    zip: &[u8],
) -> Response {
    let mut req = Request::post("/api/game/1/ad/challenges/10/patch-revisions")
        .header(header::CONTENT_TYPE, "application/zip")
        .header("x-rsctf-expected-revision", expected)
        .header("x-rsctf-expected-service", service)
        .body(Body::from(zip.to_vec()))
        .unwrap();
    signed_in(&mut req, user);
    router.clone().oneshot(req).await.unwrap()
}

async fn json(response: Response) -> Value {
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap()).unwrap()
}

pub(super) async fn check(
    pool: &PgPool,
    router: &Router,
    user: &CurrentUser,
    starter: &[u8],
    candidate: &[u8],
) {
    let list = |actor| {
        request(
            router,
            "GET",
            10,
            "patch-revisions",
            Some(actor),
            Body::empty(),
        )
    };
    let other = CurrentUser {
        id: sqlx::query_scalar(r#"SELECT captain_id FROM "Teams" WHERE id=30"#)
            .fetch_one(pool)
            .await
            .unwrap(),
        ..user.clone()
    };
    assert_eq!(json(list(user).await).await, serde_json::json!([]));
    // Fail after blobs and manifest have been staged. Nothing may survive a
    // lost revision write, and the same parent must remain retryable.
    sqlx::raw_sql(
        r#"CREATE FUNCTION reject_revision() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN RAISE EXCEPTION 'injected revision failure'; END $$;
        CREATE TRIGGER fail_revision BEFORE INSERT ON "AdPatchRevisions"
        FOR EACH ROW EXECUTE FUNCTION reject_revision();"#,
    )
    .execute(pool)
    .await
    .unwrap();
    assert_eq!(
        save(router, user, 0, candidate).await.status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    let residue: i64 = sqlx::query_scalar(
        r#"SELECT (SELECT COUNT(*) FROM "AdPatchSourceTrees") +
        (SELECT COUNT(*) FROM "AdPatchSourceBlobs") + (SELECT COUNT(*) FROM "AdPatchRevisions")
        + (SELECT COUNT(*) FROM "AdPatchSourceUsage")"#,
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(residue, 0);
    sqlx::raw_sql(r#"DROP TRIGGER fail_revision ON "AdPatchRevisions""#)
        .execute(pool)
        .await
        .unwrap();
    sqlx::raw_sql(r#"INSERT INTO "AdPatchSourceUsage" VALUES (20,536870912)"#)
        .execute(pool)
        .await
        .unwrap();
    assert_eq!(
        save(router, user, 0, candidate).await.status(),
        StatusCode::CONFLICT
    );
    assert!(json(list(user).await).await.as_array().unwrap().is_empty());
    sqlx::raw_sql(r#"DELETE FROM "AdPatchSourceUsage" WHERE service_id=20"#)
        .execute(pool)
        .await
        .unwrap();
    let first = json(save(router, user, 0, candidate).await).await;
    assert_eq!(first["revision"], 1);
    assert_eq!(
        first["treeId"],
        PatchManifest::from_zip(candidate).unwrap().tree_id()
    );
    assert_eq!(json(list(&other).await).await, serde_json::json!([]));
    assert_eq!(
        save(router, user, 0, starter).await.status(),
        StatusCode::CONFLICT
    );
    let member = CurrentUser {
        id: Uuid::new_v4(),
        ..user.clone()
    };
    for sql in [
        r#"INSERT INTO "AspNetUsers" VALUES ($1,1,true,'stamp')"#,
        r#"INSERT INTO "TeamMembers" VALUES (2,$1)"#,
        r#"INSERT INTO "UserParticipations" VALUES ($1,1,2,3)"#,
        r#"INSERT INTO "IdentityObservations" VALUES ($1,1,2,3,now())"#,
    ] {
        sqlx::query(sql)
            .bind(member.id)
            .execute(pool)
            .await
            .unwrap();
    }
    let second = json(save(router, &member, 1, starter).await).await;
    assert_eq!(second["parentId"], first["commitId"]);
    assert_eq!(json(save(router, user, 0, candidate).await).await, first);
    let (a, b) = tokio::join!(
        save(router, user, 2, candidate),
        save(router, &member, 2, candidate)
    );
    let statuses = [a.status(), b.status()];
    assert!(statuses.contains(&StatusCode::OK));
    assert!(statuses
        .iter()
        .all(|s| *s == StatusCode::OK || *s == StatusCode::SERVICE_UNAVAILABLE));
    let rows = json(list(user).await).await;
    assert_eq!(rows.as_array().unwrap().len(), 3);
    assert_eq!(rows[0]["parentId"], second["commitId"]);
    assert_eq!(rows[0]["treeId"], first["treeId"]);
    let older = request(
        router,
        "GET",
        10,
        "patch-revisions?before=2",
        Some(user),
        Body::empty(),
    )
    .await;
    assert_eq!(json(older).await, serde_json::json!([first]));
    let entries = sqlx::query_as::<_, (bool, Vec<u8>)>(
        r#"SELECT deflated,payload FROM "AdPatchSourceBlobs" WHERE service_id=20"#,
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(entries.len(), 1);
    let (deflated, bytes) = &entries[0];
    let mut unpacked = Vec::new();
    if *deflated {
        flate2::read::DeflateDecoder::new(&bytes[..])
            .read_to_end(&mut unpacked)
            .unwrap();
    } else {
        unpacked.extend(bytes);
    }
    assert_eq!(unpacked, b"private candidate contents");
    let tree_count: i64 =
        sqlx::query_scalar(r#"SELECT COUNT(*) FROM "AdPatchSourceTrees" WHERE service_id=20"#)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(tree_count, 2);
    for (revision, kind) in [(1, "Added"), (2, "Deleted"), (3, "Added")] {
        let route = format!("patch-revisions/{revision}/changes");
        let changes =
            json(request(router, "GET", 10, &route, Some(&member), Body::empty()).await).await;
        assert_eq!(
            changes,
            serde_json::json!([{"path":"src/version.txt","kind":kind}])
        );
        assert_eq!(
            request(router, "GET", 10, &route, Some(&other), Body::empty())
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
    }
    let rejected = request(
        router,
        "GET",
        11,
        "patch-revisions",
        Some(user),
        Body::empty(),
    )
    .await;
    assert_eq!(rejected.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        save(router, &other, 0, candidate).await.status(),
        StatusCode::CONFLICT
    );
    check_storage_reuse(pool, router, &other).await;
}

async fn check_storage_reuse(pool: &PgPool, router: &Router, user: &CurrentUser) {
    let mut asset = vec![0; 4 * 1024 * 1024];
    rand::fill(&mut asset[..]);
    let mut uploaded = 0;
    for revision in 0..5 {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (path, data) in [
            ("Dockerfile", &b"FROM scratch\n"[..]),
            (
                if revision < 2 {
                    "asset.bin"
                } else {
                    "assets/renamed.bin"
                },
                &asset[..],
            ),
            ("src/config", &[revision as u8][..]),
        ] {
            writer.start_file(path, opts).unwrap();
            writer.write_all(data).unwrap();
        }
        let archive = writer.finish().unwrap().into_inner();
        uploaded += archive.len();
        assert_eq!(
            save_to(router, user, 30, revision, &archive).await.status(),
            StatusCode::OK
        );
    }
    let (files,payload): (i64,i64) = sqlx::query_as(r#"SELECT COUNT(*),SUM(octet_length(payload)) FROM "AdPatchSourceBlobs" WHERE service_id=30"#)
        .fetch_one(pool).await.unwrap();
    let accounted: i64 =
        sqlx::query_scalar(r#"SELECT used_bytes FROM "AdPatchSourceUsage" WHERE service_id=30"#)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!((files, payload), (6, 4 * 1024 * 1024 + 5));
    assert!(accounted < payload + 8192);
    println!("Five ZIP revisions: {uploaded} bytes uploaded, {payload} unique file bytes, {accounted} accounted bytes with manifests and row allowances.");
}
