use super::*;
use crate::services::container::{ContainerManager, ContainerResourceLimits, ContainerSpec};

#[tokio::test]
#[ignore = "requires disposable Kind cluster and CSI storage"]
async fn real_team_storage_survives_recreation() {
    assert_eq!(std::env::var("RSCTF_K8S_LIVE_STORAGE").as_deref(), Ok("1"));
    let _ = tokio_rustls::rustls::crypto::aws_lc_rs::default_provider().install_default();
    let manager = KubernetesContainerManager::connect().await.unwrap();
    let mut spec = ContainerSpec::ad_service(
        "docker.io/library/redis@sha256:57e4255cc971b1c8d6f8a6ca283ac01bc3ecdd914273ef6bfaede87aaecd6541".into(),
        ContainerResourceLimits { memory_limit:128, cpu_count:1, storage_limit:64 }, 6379, 7, false, String::new());
    spec.team_storage = Some(TeamStorage {
        game_id: 10,
        participation_id: 11,
        challenge_id: 12,
        mount_path: "/data".into(),
        allocation_id: uuid::Uuid::new_v4(),
        claim_uid: None,
    });
    let data = spec.team_storage.as_mut().unwrap();
    data.claim_uid = Some(
        manager
            .prepare_team_claim(None, data, spec.storage_limit)
            .await
            .unwrap(),
    );
    spec.operation_id = Some("real-team-claim-first".into());
    let first = manager.create(spec.clone()).await.unwrap();
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
    data.claim_uid = Some(
        manager
            .prepare_team_claim(None, data, peer.storage_limit)
            .await
            .unwrap(),
    );
    let other = manager.create(peer.clone()).await.unwrap();
    assert_eq!(exec(other.id.clone(),"test ! -e /data/team-record && echo team-two > /data/team-record && cat /data/team-record").await.trim(),"team-two");
    assert!(manager
        .ad_donor_needs_transfer(&first.id, &peer)
        .await
        .is_err());
    assert!(!manager
        .ad_donor_needs_transfer(&first.id, &spec)
        .await
        .unwrap());
    manager.destroy(&first.id).await.unwrap();
    spec.operation_id = Some("real-team-claim-recreate".into());
    let next = manager.create(spec.clone()).await.unwrap();
    assert_ne!(next.id, first.id);
    assert_eq!(
        exec(next.id.clone(), "cat /data/team-record").await.trim(),
        "team-one"
    );
    assert_eq!(
        exec(other.id.clone(), "cat /data/team-record").await.trim(),
        "team-two"
    );
    manager.destroy(&next.id).await.unwrap();
    manager.destroy(&other.id).await.unwrap();
}
