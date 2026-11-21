use super::{checkpoint_reset_replacement, prepare_service_reset};
use crate::services::ad::scoring::test_fixture::ad_scoring_fixture;
use sea_orm::SqlxPostgresConnector;
use serde_json::{json, Value};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn resetting_a_hosted_service_checkpoints_the_exact_retirement_atomically() {
    let fixture = ad_scoring_fixture().await;
    let pool = &fixture.pool;
    let db = SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone());
    let job = Uuid::new_v4();
    let lease = Uuid::new_v4();
    sqlx::raw_sql(r#"
        UPDATE "Games" SET end_time_utc=now()+interval '1 hour' WHERE id=900001;
        UPDATE "GameChallenges" SET ad_self_hosted=FALSE WHERE id=900031;
        UPDATE "AdTeamServices" SET container_id='running' WHERE id=900041;
        UPDATE "AdRounds" SET finalized=FALSE WHERE id=900108;
        UPDATE "AdCheckResults" SET sla_credit=NULL WHERE round_id=900108 AND team_service_id=900041;
    "#).execute(pool).await.unwrap();
    sqlx::query(r#"
        INSERT INTO "ControlPlaneJobs"
            (id,kind,scope_key,game_id,operation_id,fingerprint,status,lease_token,lease_expires_at_utc)
        VALUES ($1,'AdReset','reset-fixture',900001,$1,'fixture',1,$2,now()+interval '1 minute')
    "#).bind(job).bind(lease).execute(pool).await.unwrap();

    let image = format!("sha256:{}", "a".repeat(64));
    let prepare = |backend, token| {
        prepare_service_reset(
            &db,
            fixture.game_id,
            900041,
            Some(backend),
            job,
            token,
            &image,
            "fixture retirement",
        )
    };
    let read = || {
        sqlx::query_as::<_, (String, i32, String, bool, Value)>(
            r#"
        SELECT s.host,s.port,s.container_id,c.sla_credit IS NULL,j.input
        FROM "AdTeamServices" s
        JOIN "AdCheckResults" c ON c.team_service_id=s.id AND c.round_id=900108
        CROSS JOIN "ControlPlaneJobs" j WHERE s.id=900041 AND j.id=$1
    "#,
        )
        .bind(job)
        .fetch_one(pool)
    };
    let original = read().await.unwrap();
    assert!(prepare("retired", lease).await.is_err());
    assert!(prepare("running", Uuid::new_v4()).await.is_err());
    assert_eq!(read().await.unwrap(), original);

    let result = prepare("running", lease).await.unwrap();
    assert_eq!(result.prepared_round_id, Some(900108));
    assert_eq!(result.retired_container_id.as_deref(), Some("running"));
    assert_eq!(
        result.current_flag.as_deref(),
        Some("flag{00000000000000000000000000000081}")
    );
    assert_eq!(
        read().await.unwrap(),
        (
            String::new(),
            0,
            "running".into(),
            false,
            json!({"preparedImageRef":image,"preparedRoundId": 900108, "retiredBackendId": "running", "resetPrepared": true})
        )
    );
    let unaffected: i64 =
        sqlx::query_scalar(r#"SELECT count(*) FROM "AdCheckResults" WHERE sla_credit=1.0"#)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(unaffected, 15);

    let checkpoint = |token| {
        checkpoint_reset_replacement(
            pool,
            fixture.game_id,
            900041,
            job,
            token,
            None,
            "replacement",
        )
    };
    // A surviving retired backend cannot be displaced by a stale launch.
    assert!(checkpoint(lease).await.is_err());
    sqlx::query(r#"UPDATE "AdTeamServices" SET container_id=NULL WHERE id=900041"#)
        .execute(pool)
        .await
        .unwrap();
    assert!(checkpoint(Uuid::new_v4()).await.is_err());
    let ownership = || {
        sqlx::query_as::<_, (Option<String>, Option<String>)>(
            r#"SELECT s.container_id, j.input->>'replacementBackendId' FROM "AdTeamServices" s
            CROSS JOIN "ControlPlaneJobs" j WHERE s.id=900041 AND j.id=$1"#,
        )
        .bind(job)
        .fetch_one(pool)
    };
    assert_eq!(ownership().await.unwrap(), (None, None));
    for _ in 0..2 {
        checkpoint(lease).await.unwrap();
        assert_eq!(
            ownership().await.unwrap(),
            (Some("replacement".into()), Some("replacement".into()))
        );
    }
    fixture.cleanup().await;
}
