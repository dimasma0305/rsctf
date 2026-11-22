use super::*;
use serde_json::{json, Value};

async fn body(response: Response) -> Value {
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap()
}

pub(super) async fn check(pool: &PgPool, player: &Router, editor: &Router, user: &CurrentUser) {
    sqlx::raw_sql(r#"CREATE TABLE "GameManagers"(id serial PRIMARY KEY,game_id int,user_id uuid);
        CREATE TABLE "AdRounds"(game_id int,number int,start_time_utc timestamptz,end_time_utc timestamptz,finalized boolean);
        CREATE TABLE "ControlPlaneJobs"(game_id int,challenge_id int,kind text,status smallint,created_at_utc timestamptz,finished_at_utc timestamptz);
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
        INSERT INTO "ControlPlaneJobs" SELECT g,c,k,s,now()-make_interval(mins=>age),now()-interval '19 minutes 30 seconds'
        FROM (VALUES (1,10,'AdActivate',2,20),(2,10,'AdActivate',2,20),(1,12,'AdActivate',2,20),
          (1,10,'AdActivate',3,20),(1,10,'AdActivate',1,20),(1,10,'AdReset',2,20),(1,10,'AdActivate',2,35)) v(g,c,k,s,age);
    "#).execute(pool).await.unwrap();
    assert_eq!(body(activity(10, Some(user)).await).await, json!([]));
    sqlx::query(r#"UPDATE "AdRounds" SET finalized=true WHERE number=2"#)
        .execute(pool)
        .await
        .unwrap();
    let report = activity(10, Some(user)).await;
    assert_eq!(report.headers()[header::CACHE_CONTROL], "private, no-store");
    assert_eq!(body(report).await, json!([{"round":1,"deployments":1}]));
    assert_ne!(activity(11, Some(user)).await.status(), StatusCode::OK);
    assert_ne!(activity(12, Some(user)).await.status(), StatusCode::OK);
    assert_eq!(
        set(&admin, false, 1).await.unwrap().status(),
        StatusCode::CONFLICT
    );
    sqlx::raw_sql(
        r#"DROP TABLE "AdPatchActivityPolicies","ControlPlaneJobs","AdRounds","GameManagers";"#,
    )
    .execute(pool)
    .await
    .unwrap();
}
