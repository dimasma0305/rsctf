use crate::services::ad::reset_fixture::{docker, docker_state, LiveService};
use crate::services::ad::scoring::test_fixture::ad_scoring_fixture;
use crate::services::{ad_engine, container::docker_installation_scope};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::time::Instant;

#[tokio::test]
#[ignore = "requires disposable PostgreSQL and Docker with RSCTF_TEST_CONTAINER_IMAGE"]
async fn native_round_publication_records_each_managed_service_acknowledgement() {
    let image = std::env::var("RSCTF_TEST_CONTAINER_IMAGE").unwrap();
    let fixture = ad_scoring_fixture().await;
    let state = docker_state(&fixture.pool);
    let image_id = docker(&["image", "inspect", "--format", "{{.Id}}", &image]);
    let game = seed_grid(&fixture.pool, &image_id).await;
    let scope = docker_installation_scope();
    let mut containers = HashMap::new();
    let services: Vec<i32> = sqlx::query_scalar(
        r#"SELECT id FROM "AdTeamServices" WHERE game_id=$1 ORDER BY participation_id,challenge_id"#,
    )
    .bind(game)
    .fetch_all(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(services.len(), 25);
    for id in &services {
        let container = LiveService(docker(&[
            "run",
            "--detach",
            "--rm",
            "--network=none",
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--user=65534:65534",
            "--no-healthcheck",
            "--tmpfs",
            "/team:rw,mode=1777,size=1m",
            "--env",
            "RSCTF_FLAG_FILE=/team/flag",
            "--label",
            &format!("rsctf.managed={scope}"),
            "--label",
            &format!("rsctf.scope={scope}"),
            "--entrypoint",
            "/bin/sleep",
            &image,
            "300",
        ]));
        sqlx::query(r#"UPDATE "AdTeamServices" SET container_id=$1 WHERE id=$2"#)
            .bind(&container.0)
            .bind(id)
            .execute(&fixture.pool)
            .await
            .unwrap();
        containers.insert(*id, container);
    }
    let round = ad_engine::prepare_round(&state.db, game, None, Some(false), chrono::Utc::now())
        .await
        .unwrap();
    assert_eq!(round.flags.len(), services.len());
    let (sender, mut receipts) = tokio::sync::mpsc::unbounded_channel();
    let started = Instant::now();
    let result = super::super::publish_round_flags(
        &state,
        game,
        round.id,
        round.number,
        chrono::Utc::now(),
        3,
        round.flags.clone(),
        false,
        Instant::now() + Duration::from_secs(60),
        sender,
    )
    .await
    .unwrap();
    assert_eq!(result.failure_count, 0);
    eprintln!(
        "Published round {} to {} containers in {:?}",
        round.number,
        services.len(),
        started.elapsed()
    );
    let mut acknowledged = HashSet::new();
    while let Some(receipt) = receipts.recv().await {
        assert!(acknowledged.insert(receipt.team_service_id));
    }
    assert_eq!(acknowledged, services.iter().copied().collect());
    let stored = read_receipts(&fixture.pool, round.id).await;
    assert_eq!(stored.len(), services.len());
    for receipt in &stored {
        assert_eq!(receipt.container_id, containers[&receipt.team_service_id].0);
        assert!(receipt.attempts > 0);
        assert!(docker(&["exec", &receipt.container_id, "cat", "/team/flag"]) == receipt.flag);
    }
    // A worker recovering after the receipt transaction must not write the
    // previously acknowledged flag into a container that has since moved on.
    docker(&[
        "exec",
        &containers[&services[0]].0,
        "sh",
        "-c",
        "printf sentinel > /team/flag",
    ]);
    let (sender, mut receipts) = tokio::sync::mpsc::unbounded_channel();
    let replay = super::super::publish_round_flags(
        &state,
        game,
        round.id,
        round.number,
        chrono::Utc::now(),
        3,
        round.flags,
        false,
        Instant::now() + Duration::from_secs(60),
        sender,
    )
    .await
    .unwrap();
    assert_eq!(replay.failure_count, 0);
    assert!(receipts.recv().await.is_none());
    assert!(
        stored == read_receipts(&fixture.pool, round.id).await,
        "replay changed a durable receipt"
    );
    assert_eq!(
        docker(&["exec", &containers[&services[0]].0, "cat", "/team/flag"]),
        "sentinel"
    );
    drop(containers);
    fixture.cleanup().await;
}

#[derive(sqlx::FromRow, PartialEq)]
struct Receipt {
    team_service_id: i32,
    container_id: String,
    flag: String,
    attempts: i16,
    completed_at: chrono::DateTime<chrono::Utc>,
}

async fn read_receipts(pool: &sqlx::PgPool, round_id: i32) -> Vec<Receipt> {
    sqlx::query_as(
        r#"SELECT f.team_service_id, d.container_id, f.flag, d.attempts, d.completed_at
             FROM "AdFlags" f
             JOIN "AdFlagDeliveryResults" d USING (round_id, team_service_id)
            WHERE f.round_id=$1 AND d.delivered AND d.delivery_kind='Managed'
            ORDER BY f.team_service_id"#,
    )
    .bind(round_id)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn seed_grid(pool: &sqlx::PgPool, image_id: &str) -> i32 {
    let game: i32 = sqlx::query_scalar(r#"INSERT INTO "Games"
        (title,public_key,private_key,hidden,practice_mode,summary,content,accept_without_review,
         allow_user_submissions,writeup_required,team_member_count_limit,container_count_limit,
         start_time_utc,end_time_utc,writeup_deadline,writeup_note,blood_bonus_value,
         ad_allow_snapshot_download,ad_scoring_paused,ad_warmup_seconds,ad_tick_seconds)
        VALUES ('Native publication','','',TRUE,FALSE,'','',FALSE,FALSE,FALSE,4,5,
            now()-interval '1 minute',now()+interval '1 hour',now(),'',0,FALSE,FALSE,0,120) RETURNING id"#)
        .fetch_one(pool).await.unwrap();
    sqlx::query(r#"WITH accounts AS (
        INSERT INTO "AspNetUsers"(id,user_name,normalized_user_name,email_confirmed,
            phone_number_confirmed,two_factor_enabled,lockout_enabled,access_failed_count,role,ip,
            last_signed_in_utc,last_visited_utc,register_time_utc,bio,real_name,std_number,exercise_visible)
        SELECT gen_random_uuid(),'captain-'||n,'CAPTAIN-'||n,FALSE,FALSE,FALSE,FALSE,0,1,'',
            now(),now(),now(),'','','',FALSE FROM generate_series(1,5) n RETURNING id,user_name
        ), teams AS (
        INSERT INTO "Teams"(name,locked,invite_token,captain_id)
        SELECT user_name,FALSE,gen_random_uuid()::text,id FROM accounts RETURNING id
        ) INSERT INTO "Participations"(game_id,team_id,status,token,suspicion_score)
        SELECT $1,id,1,gen_random_uuid()::text,0 FROM teams"#)
        .bind(game).execute(pool).await.unwrap();
    sqlx::query(r#"WITH challenges AS (
        INSERT INTO "GameChallenges"(game_id,title,content,category,"Type",is_enabled,submission_limit,
            accepted_count,submission_count,review_status,build_status,enable_traffic_capture,
            enable_shared_container,disable_blood_bonus,original_score,min_score_rate,difficulty,
            score_curve,ad_allow_egress,ad_allow_self_reset,ad_ssh_requires_flag,ad_self_hosted,build_image_digest)
        SELECT $1,'service-'||n,'',0,4,TRUE,0,0,0,0,1,FALSE,FALSE,TRUE,100,0.25,5,0,
            FALSE,FALSE,FALSE,FALSE,$2 FROM generate_series(1,5) n RETURNING id
        ) INSERT INTO "AdTeamServices"(game_id,participation_id,challenge_id,host,port,status)
        SELECT $1,p.id,c.id,'127.0.0.1',8080,0 FROM "Participations" p
        CROSS JOIN challenges c WHERE p.game_id=$1"#)
        .bind(game).bind(image_id).execute(pool).await.unwrap();
    game
}
