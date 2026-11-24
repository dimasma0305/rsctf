use super::*;
use serde_json::{json, Value};

async fn body(response: Response) -> Value {
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap()
}

pub(super) async fn check(pool: &PgPool, player: &Router, editor: &Router, user: &CurrentUser) {
    sqlx::raw_sql(r#"CREATE TABLE "GameManagers"(id serial PRIMARY KEY,game_id int,user_id uuid);
        CREATE TABLE "AdRounds"(game_id int,number int,start_time_utc timestamptz,end_time_utc timestamptz,finalized boolean);
        CREATE TABLE "ControlPlaneJobs"(game_id int,challenge_id int,kind text,status smallint,created_at_utc timestamptz,finished_at_utc timestamptz,result jsonb);
    "#).execute(pool).await.unwrap();
    for _ in 0..2 {
        sqlx::raw_sql(crate::migrations::m0377_ad_patch_activity::UP_SQL)
            .execute(pool)
            .await
            .unwrap();
    }
    let activity = |challenge, actor| {
        request(
            player,
            "GET",
            challenge,
            "patch-activity",
            actor,
            Body::empty(),
        )
    };
    assert_eq!(activity(10, None).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(body(activity(10, Some(user)).await).await, Value::Null);
    let admin = CurrentUser {
        role: crate::utils::enums::Role::Admin,
        ..user.clone()
    };
    let set = |actor: &CurrentUser, enabled, revision| {
        let mut req = Request::put("/api/edit/games/1/ad/patch-activity")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"enabled":enabled,"expectedRevision":revision}).to_string(),
            ))
            .unwrap();
        signed_in(&mut req, actor);
        editor.clone().oneshot(req)
    };
    assert_eq!(
        set(user, true, 0).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        set(&admin, true, 0).await.unwrap().status(),
        StatusCode::CONFLICT
    );
    sqlx::query(r#"UPDATE "Games" SET start_time_utc=now()+interval '1 hour' WHERE id=1"#)
        .execute(pool)
        .await
        .unwrap();
    let enabled = json!({"enabled":true,"revision":1,"editable":true});
    assert_eq!(body(set(&admin, true, 0).await.unwrap()).await, enabled);
    assert_eq!(body(set(&admin, true, 0).await.unwrap()).await, enabled);
    assert_eq!(
        set(&admin, false, 0).await.unwrap().status(),
        StatusCode::CONFLICT
    );
    assert_ne!(activity(10, Some(user)).await.status(), StatusCode::OK);
    sqlx::raw_sql(r#"UPDATE "Games" SET start_time_utc=now()-interval '25 minutes' WHERE id=1;
        UPDATE "AdPatchActivityPolicies" SET changed_at=now()-interval '30 minutes';
        INSERT INTO "AdRounds" VALUES
          (1,1,now()-interval '20 minutes',now()-interval '19 minutes',true),
          (1,2,now()-interval '19 minutes',now()-interval '18 minutes',false);
        INSERT INTO "ControlPlaneJobs" SELECT g,c,k,s,now()-make_interval(mins=>age),now()-interval '19 minutes 30 seconds','{"reset":true}'::jsonb
        FROM (VALUES (1,10,'AdActivate',2,20),(2,10,'AdActivate',2,20),(1,12,'AdActivate',2,20),
          (1,10,'AdActivate',3,20),(1,10,'AdActivate',1,20),(1,10,'AdReset',2,20),(1,10,'AdActivate',2,35)) v(g,c,k,s,age);
        INSERT INTO "ControlPlaneJobs" SELECT 1,10,'AdActivate',2,now()-interval '20 minutes',now()-interval '19 minutes 30 seconds',receipt
        FROM (VALUES ('{"reset":false,"alreadyReplaced":true}'::jsonb),
                     ('{"reset":true,"alreadyReplaced":true}'::jsonb), (NULL::jsonb)) AS retries(receipt);
    "#).execute(pool).await.unwrap();
    assert_eq!(body(activity(10, Some(user)).await).await, json!([]));
    sqlx::query(r#"UPDATE "AdRounds" SET finalized=true WHERE number=2"#)
        .execute(pool)
        .await
        .unwrap();
    let report = activity(10, Some(user)).await;
    assert_eq!(report.headers()[header::CACHE_CONTROL], "private, no-store");
    assert_eq!(body(report).await, json!([{"round":1,"deployments":2}]));
    assert_ne!(activity(11, Some(user)).await.status(), StatusCode::OK);
    assert_ne!(activity(12, Some(user)).await.status(), StatusCode::OK);
    assert_eq!(
        set(&admin, false, 1).await.unwrap().status(),
        StatusCode::CONFLICT
    );
    check_archive(pool, player, user).await;
}

async fn check_archive(pool: &PgPool, player: &Router, user: &CurrentUser) {
    let archive = |challenge, actor| {
        request(
            player,
            "GET",
            challenge,
            "patch-archive",
            actor,
            Body::empty(),
        )
    };
    sqlx::raw_sql(r#"ALTER TABLE "Teams" ADD COLUMN name TEXT NOT NULL DEFAULT 'Owls';
        UPDATE "AdPatchStarters" SET ssh_sources='[{"sourcePath":"Dockerfile","containerPath":"/app/Dockerfile","shareLineRangesAfterClose":true}]' WHERE challenge_id=10"#)
        .execute(pool).await.unwrap();
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (path, content) in [
        ("Dockerfile", "FROM scratch\n# private code"),
        ("private-name.txt", "private payload"),
        (
            ".rsctf-ssh.json",
            r#"[{"sourcePath":"private-name.txt","containerPath":"/private","shareAfterClose":true}]"#,
        ),
    ] {
        zip.start_file(path, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(content.as_bytes()).unwrap();
    }
    let mut req = Request::post("/api/game/1/ad/challenges/10/patch-revisions")
        .header(header::CONTENT_TYPE, "application/zip")
        .header("x-rsctf-expected-revision", "0")
        .header("x-rsctf-expected-service", "20")
        .body(Body::from(zip.finish().unwrap().into_inner()))
        .unwrap();
    signed_in(&mut req, user);
    assert!(player
        .clone()
        .oneshot(req)
        .await
        .unwrap()
        .status()
        .is_success());
    let changes = serde_json::json!([{"path":"Dockerfile","kind":"Modified","hunks":[
        {"oldStart":1,"oldCount":0,"newStart":2,"newCount":1}
    ]}]);
    assert_eq!(sqlx::query_as::<_,(Value,bool)>(r#"SELECT changes,published FROM "AdSharedPatchChanges" WHERE service_id=20 AND revision=1"#)
        .fetch_one(pool).await.unwrap(), (changes.clone(),false));
    assert_eq!(
        archive(10, Some(user)).await.status(),
        StatusCode::NOT_FOUND
    );
    sqlx::query(r#"UPDATE "Games" SET end_time_utc=now()-interval '1 minute'"#)
        .execute(pool)
        .await
        .unwrap();
    assert_eq!(
        archive(10, Some(user)).await.status(),
        StatusCode::NOT_FOUND
    );
    sqlx::query(r#"UPDATE "Games" SET practice_mode=false"#)
        .execute(pool)
        .await
        .unwrap();
    assert_eq!(
        body(archive(10, Some(user)).await).await,
        json!({"revisions":[],"hasMore":false})
    );
    sqlx::query(r#"UPDATE "AdSharedPatchChanges" SET published=true WHERE service_id=20"#)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(r#"UPDATE "AdRounds" SET finalized=false WHERE number=2"#)
        .execute(pool)
        .await
        .unwrap();
    assert_eq!(
        archive(10, Some(user)).await.status(),
        StatusCode::NOT_FOUND
    );
    sqlx::query(r#"UPDATE "AdRounds" SET finalized=true WHERE number=2"#)
        .execute(pool)
        .await
        .unwrap();
    let res = archive(10, Some(user)).await;
    assert_eq!(res.headers()[header::CACHE_CONTROL], "private, no-store");
    assert_eq!(
        body(res).await,
        json!({"revisions":[{"teamId":2,"teamName":"Owls","revision":1,"changes":changes}],"hasMore":false})
    );
    assert_eq!(archive(10, None).await.status(), StatusCode::UNAUTHORIZED);
    assert_ne!(archive(11, Some(user)).await.status(), StatusCode::OK);
    assert_eq!(
        body(
            request(
                player,
                "GET",
                10,
                "patch-archive?afterTeam=2&afterRevision=1",
                Some(user),
                Body::empty()
            )
            .await
        )
        .await,
        json!({"revisions":[],"hasMore":false})
    );
    // A different team's published receipts enter its division's archive only
    // while the team is accepted. List pages use the last public team/revision.
    sqlx::raw_sql(r#"INSERT INTO "Teams"(id,name) VALUES(5,'Finches');
        INSERT INTO "AdTeamServices" VALUES(24,1,6,10);
        INSERT INTO "AdPatchSourceTrees" SELECT 24,tree_id,manifest FROM "AdPatchSourceTrees" WHERE service_id=20;
        INSERT INTO "AdPatchRevisions" SELECT 24,n,tree_id,parent_id,commit_id,actor_id,created_at
            FROM "AdPatchRevisions",generate_series(1,22) n WHERE service_id=20;
        INSERT INTO "AdSharedPatchChanges" SELECT 24,n,changes,true FROM "AdSharedPatchChanges",generate_series(1,22) n WHERE service_id=20;
        UPDATE "Participations" SET division_id=NULL WHERE id=6"#)
        .execute(pool).await.unwrap();
    assert_eq!(
        body(archive(10, Some(user)).await).await["revisions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    sqlx::query(r#"UPDATE "Participations" SET division_id=7,status=0 WHERE id=6"#)
        .execute(pool)
        .await
        .unwrap();
    assert_eq!(
        body(archive(10, Some(user)).await).await["revisions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    sqlx::query(r#"UPDATE "Participations" SET status=1 WHERE id=6"#)
        .execute(pool)
        .await
        .unwrap();
    let page = body(archive(10, Some(user)).await).await;
    assert_eq!(page["hasMore"], true);
    assert_eq!(page["revisions"].as_array().unwrap().len(), 20);
    assert_eq!(page["revisions"][19]["revision"], 19);
    let next = body(
        request(
            player,
            "GET",
            10,
            "patch-archive?afterTeam=5&afterRevision=19",
            Some(user),
            Body::empty(),
        )
        .await,
    )
    .await;
    assert_eq!(next["hasMore"], false);
    assert_eq!(next["revisions"].as_array().unwrap().len(), 3);
    assert_eq!(next["revisions"][0]["revision"], 20);
    sqlx::query(r#"UPDATE "AdPatchActivityPolicies" SET enabled=false"#)
        .execute(pool)
        .await
        .unwrap();
    assert_eq!(
        archive(10, Some(user)).await.status(),
        StatusCode::NOT_FOUND
    );
}
