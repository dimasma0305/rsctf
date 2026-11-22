use super::*;
use crate::services::container::{ContainerManager, ContainerResourceLimits, ContainerSpec};

async fn check_private_route(manager: &KubernetesContainerManager, pod: &str, ip: &str, port: i32) {
    // Only the disposable fixture's checker Pod is allowlisted for this Service.
    let mark = ["redis-cli", "SET", "serving-pod", pod].map(String::from);
    let response = manager.exec(pod, Vec::from(mark)).await.unwrap();
    assert_eq!(response.trim(), "OK");
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let read = manager
                .exec(
                    "team-route-client",
                    vec![
                        "redis-cli".into(),
                        "--raw".into(),
                        "-h".into(),
                        ip.into(),
                        "-p".into(),
                        port.to_string(),
                        "GET".into(),
                        "serving-pod".into(),
                    ],
                )
                .await;
            if read.is_ok_and(|value| value.trim() == pod) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    })
    .await
    .expect("the original team endpoint must reach the current revision");
}

#[tokio::test]
#[ignore = "requires disposable Kind cluster and CSI storage"]
async fn real_team_storage_survives_recreation() {
    assert_eq!(std::env::var("RSCTF_K8S_LIVE_STORAGE").as_deref(), Ok("1"));
    let _ = tokio_rustls::rustls::crypto::aws_lc_rs::default_provider().install_default();
    let manager = KubernetesContainerManager::connect().await.unwrap();
    let starter = std::env::var("RSCTF_K8S_TEST_STARTER").unwrap();
    let revision_one = std::env::var("RSCTF_K8S_TEST_REVISION_ONE").unwrap();
    let revision_two = std::env::var("RSCTF_K8S_TEST_REVISION_TWO").unwrap();
    let mut spec = ContainerSpec::ad_service(
        starter.clone(),
        ContainerResourceLimits {
            memory_limit: 128,
            cpu_count: 1,
            storage_limit: 64,
        },
        6379,
        7,
        false,
        String::new(),
    );
    spec.team_storage = Some(TeamStorage {
        game_id: 10,
        participation_id: 11,
        challenge_id: 12,
        mount_path: "/data".into(),
        allocation_id: uuid::Uuid::new_v4(),
        claim_uid: None,
        endpoint_uid: None,
    });
    let data = spec.team_storage.as_mut().unwrap();
    let (claim_uid, endpoint_uid) = manager
        .prepare_team_claim(None, data, spec.storage_limit, spec.expose_port)
        .await
        .unwrap();
    data.claim_uid = Some(claim_uid);
    data.endpoint_uid = Some(endpoint_uid);
    spec.operation_id = Some("real-team-claim-first".into());
    let first = manager.create(spec.clone()).await.unwrap();
    check_private_route(&manager, &first.id, &first.ip, first.port).await;
    let exec = |id: String, script: &'static str| {
        let manager = &manager;
        async move {
            manager
                .exec(&id, vec!["sh".into(), "-ec".into(), script.into()])
                .await
                .unwrap()
        }
    };
    assert_eq!(
        exec(
            first.id.clone(),
            "echo team-one > /data/team-record && cat /data/team-record"
        )
        .await
        .trim(),
        "team-one"
    );
    let mut peer = spec.clone();
    peer.operation_id = Some("real-team-claim-peer".into());
    peer.team_storage.as_mut().unwrap().participation_id = 13;
    let data = peer.team_storage.as_mut().unwrap();
    data.allocation_id = uuid::Uuid::new_v4();
    data.claim_uid = None;
    data.endpoint_uid = None;
    let (claim_uid, endpoint_uid) = manager
        .prepare_team_claim(None, data, peer.storage_limit, peer.expose_port)
        .await
        .unwrap();
    data.claim_uid = Some(claim_uid);
    data.endpoint_uid = Some(endpoint_uid);
    let other = manager.create(peer.clone()).await.unwrap();
    check_private_route(&manager, &other.id, &other.ip, other.port).await;
    assert_eq!(exec(other.id.clone(),"test ! -e /data/team-record && echo team-two > /data/team-record && cat /data/team-record").await.trim(),"team-two");
    assert!(manager
        .ad_donor_needs_transfer(&first.id, &peer)
        .await
        .is_err());
    assert!(!manager
        .ad_donor_needs_transfer(&first.id, &spec)
        .await
        .unwrap());
    assert_ne!(first.ip, other.ip);
    let mut missing = spec.clone();
    missing.operation_id = Some("missing-kubernetes-patch".into());
    missing.image = format!("docker.io/library/redis@sha256:{}", "0".repeat(64));
    assert!(manager
        .check_ad_patch_image(&first.id, &missing)
        .await
        .is_err());
    check_private_route(&manager, &first.id, &first.ip, first.port).await;
    assert_eq!(
        exec(first.id.clone(), "cat /data/team-record").await.trim(),
        "team-one"
    );
    let mut current = first.id.clone();
    for (step, image) in [
        ("first", &revision_one),
        ("second", &revision_two),
        ("previous", &revision_one),
        ("starter", &starter),
    ] {
        spec.image = image.clone();
        spec.operation_id = Some(format!("real-team-claim-{step}-rollout"));
        manager.check_ad_patch_image(&current, &spec).await.unwrap();
        assert!(!manager
            .ad_donor_needs_transfer(&current, &spec)
            .await
            .unwrap());
        manager.destroy(&current).await.unwrap();
        let next = manager.create(spec.clone()).await.unwrap();
        assert_ne!(next.id, current);
        assert_eq!((&next.ip, next.port), (&first.ip, first.port));
        assert_eq!(manager.create(spec.clone()).await.unwrap().id, next.id);
        check_private_route(&manager, &next.id, &first.ip, first.port).await;
        assert_eq!(
            exec(next.id.clone(), "redis-cli ping && cat /data/team-record")
                .await
                .trim(),
            "PONG\nteam-one"
        );
        current = next.id;
    }
    assert_eq!(
        exec(other.id.clone(), "cat /data/team-record").await.trim(),
        "team-two"
    );
    manager.destroy(&current).await.unwrap();
    manager.destroy(&other.id).await.unwrap();
}
