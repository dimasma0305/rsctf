//! Exports disposable profiles from the real download route for native client CI.
use super::*;
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use sea_orm_migration::MigratorTrait;
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires isolated NET_ADMIN namespace, PostgreSQL and RSCTF_VPN_CLIENT_FIXTURE"]
async fn native_client_profiles_keep_teammates_independent() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();
    let output =
        std::env::var("RSCTF_VPN_CLIENT_FIXTURE").expect("explicit disposable fixture path");
    assert_eq!(
        std::env::var("RSCTF_AD_VPN_SERVER_ENDPOINT").unwrap(),
        "127.0.0.1:51822"
    );
    initialize_backend(ContainerBackendKind::Docker).unwrap();
    let db = crate::utils::test_database::TestDatabase::new("native_vpn").await;
    let database = sea_orm::SqlxPostgresConnector::from_sqlx_postgres_pool(db.pool.clone());
    crate::migrations::Migrator::up(&database, None)
        .await
        .unwrap();
    let root = std::env::temp_dir().join(format!("rsctf-native-vpn-{}", Uuid::new_v4()));
    let mut config = crate::models::internal::configs::AppConfig::default();
    config.event_vpn_credential_key = "native-vpn-test-key-0123456789abcdef".into();
    let st = crate::app_state::AppState::new(
        database,
        Arc::new(config),
        Arc::new(crate::services::cache::InMemoryCache::new()),
        Arc::new(crate::storage::LocalBlobStorage::new(root.clone())),
        crate::services::token::TokenService::new("0123456789abcdef0123456789abcdef", 60),
        Arc::new(crate::services::container::NoopContainerManager),
    );
    acquire_instance_lease(&st.db).await.unwrap();
    let monitor = start_instance_lease_monitor();
    let users = [Uuid::new_v4(), Uuid::new_v4()];
    for (index, id) in users.iter().enumerate() {
        sqlx::query(
            r#"INSERT INTO "AspNetUsers"
            (id,user_name,normalized_user_name,email_confirmed,phone_number_confirmed,
             two_factor_enabled,lockout_enabled,access_failed_count,role,ip,
             last_signed_in_utc,last_visited_utc,register_time_utc,bio,real_name,
             std_number,exercise_visible,security_stamp)
            VALUES ($1,$2,upper($2),TRUE,FALSE,FALSE,FALSE,0,1,'',now(),now(),now(),
                    '','','',TRUE,'native-fixture')"#,
        )
        .bind(id)
        .bind(format!("native{index}"))
        .execute(&db.pool)
        .await
        .unwrap();
    }
    let team: i32 = sqlx::query_scalar(r#"INSERT INTO "Teams"
        (name,bio,locked,invite_token,captain_id) VALUES ('Native CI','',FALSE,$1,$2) RETURNING id"#)
        .bind(Uuid::new_v4().to_string()).bind(users[0]).fetch_one(&db.pool).await.unwrap();
    for id in users {
        sqlx::query(r#"INSERT INTO "TeamMembers" (team_id,user_id) VALUES ($1,$2)"#)
            .bind(team)
            .bind(id)
            .execute(&db.pool)
            .await
            .unwrap();
    }
    let router = crate::controllers::game::ad::router().with_state(st.clone());
    let mut cases = Vec::new();
    for (name, kind) in [("ad", 4_i16), ("koth", 5_i16)] {
        let game: i32 = sqlx::query_scalar(r#"INSERT INTO "Games"
            (title,public_key,private_key,hidden,practice_mode,summary,content,
             accept_without_review,allow_user_submissions,writeup_required,
             team_member_count_limit,container_count_limit,start_time_utc,end_time_utc,
             writeup_deadline,writeup_note,blood_bonus_value,ad_allow_snapshot_download,ad_scoring_paused)
            VALUES ($1,'','',FALSE,FALSE,'','',TRUE,FALSE,FALSE,0,3,
                    now()-interval '1 hour',now()+interval '1 hour',now(),'',0,TRUE,FALSE) RETURNING id"#)
            .bind(name).fetch_one(&db.pool).await.unwrap();
        sqlx::query(
            r#"INSERT INTO "GameChallenges"
            (game_id,title,content,category,"Type",is_enabled,submission_limit,
             accepted_count,submission_count,review_status,build_status,original_score,
             min_score_rate,difficulty,score_curve,network_mode,enable_traffic_capture,
             enable_shared_container,disable_blood_bonus,ad_allow_egress,ad_allow_self_reset,
             ad_ssh_requires_flag,ad_self_hosted)
            VALUES ($1,'VPN fixture','',0,$2,TRUE,0,0,0,0,0,100,0.25,5,0,0,
                    FALSE,FALSE,TRUE,FALSE,FALSE,FALSE,FALSE)"#,
        )
        .bind(game)
        .bind(kind)
        .execute(&db.pool)
        .await
        .unwrap();
        let part: i32 = sqlx::query_scalar(
            r#"INSERT INTO "Participations"
            (status,token,game_id,team_id,suspicion_score) VALUES (1,$1,$2,$3,0) RETURNING id"#,
        )
        .bind(Uuid::new_v4().to_string())
        .bind(game)
        .bind(team)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        for id in users {
            sqlx::query(
                r#"INSERT INTO "UserParticipations" (user_id,game_id,team_id,participation_id)
                VALUES ($1,$2,$3,$4)"#,
            )
            .bind(id)
            .bind(game)
            .bind(team)
            .bind(part)
            .execute(&db.pool)
            .await
            .unwrap();
            sqlx::query(r#"INSERT INTO "IdentityObservations"
                (user_id,game_id,team_id,participation_id,observed_at_utc,kind,value_hash,value_hint,source)
                VALUES ($1,$2,$3,$4,clock_timestamp(),'Fingerprint',decode(repeat('00',32),'hex'),'fixture','GameJoin')"#)
                .bind(id).bind(game).bind(team).bind(part).execute(&db.pool).await.unwrap();
        }
        let download = |id| {
            let router = router.clone();
            async move {
                let mut req = Request::builder()
                    .uri(format!("/api/Game/{game}/Ad/Vpn/Config"))
                    .body(Body::empty())
                    .unwrap();
                req.extensions_mut().insert(
                    crate::middlewares::privilege_authentication::CurrentUser {
                        id,
                        role: crate::utils::enums::Role::User,
                        name: "native".into(),
                        security_stamp: "native-fixture".into(),
                    },
                );
                let response = router.oneshot(req).await.unwrap();
                let status = response.status();
                let body = to_bytes(response.into_body(), 16_384).await.unwrap();
                // Only error responses may be printed; successful bodies contain keys.
                assert!(
                    status == StatusCode::OK,
                    "profile download: {status} {}",
                    String::from_utf8_lossy(&body)
                );
                String::from_utf8(body.to_vec()).unwrap()
            }
        };
        // Concurrent downloads from two accounts on the same accepted team.
        let (first, second) = tokio::join!(download(users[0]), download(users[1]));
        let field = |text: &str, key: &str| -> String {
            text.lines()
                .find_map(|line| line.strip_prefix(&format!("{key} = ")))
                .unwrap()
                .to_string()
        };
        assert!(
            field(&first, "PrivateKey") != field(&second, "PrivateKey"),
            "shared player key"
        );
        assert_ne!(
            field(&first, "Address"),
            field(&second, "Address"),
            "shared player address"
        );
        let peers: Vec<(String, String)> = sqlx::query_as(
            r#"SELECT public_key,address FROM "EventVpnUserPeers"
            WHERE game_id=$1 AND revoked_at_utc IS NULL ORDER BY address"#,
        )
        .bind(game)
        .fetch_all(&db.pool)
        .await
        .unwrap();
        assert_eq!(peers.len(), 2);
        assert!(peers[0].0 != peers[1].0, "shared public key");
        let kernel = std::process::Command::new("wg")
            .args(["show", IFNAME, "allowed-ips"])
            .output()
            .unwrap();
        assert!(kernel.status.success());
        let kernel = String::from_utf8(kernel.stdout).unwrap();
        for (key, address) in &peers {
            assert!(
                kernel
                    .lines()
                    .any(|line| line.contains(key) && line.contains(&format!("{address}/32"))),
                "personal peer missing from kernel"
            );
        }
        // Redownload and changing the API gate must not rotate or collapse identities.
        for gate in [false, true, false] {
            sqlx::query(r#"UPDATE "Games" SET vpn_access_required=$2 WHERE id=$1"#)
                .bind(game)
                .bind(gate)
                .execute(&db.pool)
                .await
                .unwrap();
            crate::services::event_security::invalidate_policy(&st, game).await;
            assert_eq!(
                crate::services::event_security::load_policy(&st, game)
                    .await
                    .unwrap()
                    .access_required,
                gate
            );
            for (id, original) in users.into_iter().zip([&first, &second]) {
                let again = download(id).await;
                assert!(
                    field(&again, "PrivateKey") == field(original, "PrivateKey"),
                    "redownload rotated key"
                );
                assert_eq!(field(&again, "Address"), field(original, "Address"));
            }
        }
        cases.push(serde_json::json!({"name":name,"profiles":[first,second]}));
    }
    let fixture = serde_json::json!({
        "schema":1,"sourceSha":std::env::var("RSCTF_VPN_SOURCE_SHA").unwrap(),
        "serverPrivateKey":server_key(&st.db).await.unwrap().to_string(),
        "hubAddress":hub_address(),"serviceAddress":"10.214.0.2",
        "ingressAddress":"10.214.0.253","hostname":"vpn-ci.rsctf.test","cases":cases,
    });
    release_instance_lease().await.unwrap();
    monitor.await.unwrap();
    drop(router);
    drop(st);
    db.close().await;
    if root.exists() {
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
    // This fixture contains only fresh loopback CI credentials. Its DB is already gone.
    std::fs::write(output, serde_json::to_vec(&fixture).unwrap()).unwrap();
}
