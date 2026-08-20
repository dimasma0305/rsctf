//! Focused unit regressions for the cached A&D/KotH target projection.

use super::{
    apply_current_round, apply_hill_identities, apply_live_ad_service_identities, exclude_caller,
    invalidate_live_hill_snapshot_cache, live_hill_snapshot_cache_key, AdChallengeTargets,
    AdHillTarget, AdTargetsModel, AdTeamTarget, LiveAdServiceIdentity, LiveHillIdentity,
};
use crate::services::cache::{Cache, InMemoryCache};

#[test]
fn authoritative_round_replaces_the_roster_cache_placeholder() {
    let mut model = cached_model();
    apply_current_round(&mut model, 9);
    assert_eq!(model.current_round, 9);
    assert_eq!(model.challenges.len(), 1);
}

#[test]
fn authoritative_warmup_round_hides_the_prebuilt_roster() {
    let mut model = cached_model();
    apply_current_round(&mut model, 0);
    assert_eq!(model.current_round, 0);
    assert!(model.challenges.is_empty());
}

#[test]
fn every_team_shares_one_live_hill_snapshot_key() {
    assert_eq!(live_hill_snapshot_cache_key(17), "adlivehills:17");
}

#[tokio::test]
async fn lifecycle_transition_evicts_the_shared_live_hill_snapshot() {
    let cache = InMemoryCache::new();
    let key = live_hill_snapshot_cache_key(17);
    cache.set(&key, b"cached", None).await;
    invalidate_live_hill_snapshot_cache(&cache, 17).await;
    assert!(cache.get(&key).await.is_none());
}

#[test]
fn live_ad_identity_replaces_every_cached_endpoint_field() {
    let mut model = cached_ad_model();
    model.challenges[0].teams[0].ip = Some("retired.example".to_string());
    model.challenges[0].teams[0].port = Some(31000);
    model.challenges[0].teams[0].last_check_status = Some("Offline".to_string());

    apply_live_ad_service_identities(
        &mut model,
        &[live_ad_identity(41, "relay.example", 32000, Some(0))],
    );

    let target = &model.challenges[0].teams[0];
    assert_eq!(target.ip.as_deref(), Some("relay.example"));
    assert_eq!(target.port, Some(32000));
    assert_eq!(target.last_check_status.as_deref(), Some("Ok"));
}

#[test]
fn missing_live_ad_identity_removes_a_retired_cached_target() {
    let mut model = cached_ad_model();
    model.challenges[0].teams[0].ip = Some("retired.example".to_string());
    model.challenges[0].teams[0].port = Some(31000);

    apply_live_ad_service_identities(&mut model, &[]);

    assert!(model.challenges[0].teams.is_empty());
}

#[test]
fn caller_is_excluded_after_live_ad_overlay() {
    let mut model = cached_ad_model();
    model.challenges[0].teams.push(AdTeamTarget {
        participation_id: 42,
        team_name: "other".to_string(),
        division: None,
        ip: None,
        port: None,
        last_check_status: None,
    });
    apply_live_ad_service_identities(
        &mut model,
        &[
            live_ad_identity(41, "caller.example", 32000, Some(0)),
            live_ad_identity(42, "other.example", 32001, Some(0)),
        ],
    );
    exclude_caller(&mut model, 41);

    assert_eq!(model.challenges[0].teams.len(), 1);
    assert_eq!(model.challenges[0].teams[0].participation_id, 42);
    assert_eq!(
        model.challenges[0].teams[0].ip.as_deref(),
        Some("other.example")
    );
}

#[test]
fn live_hill_identity_replaces_the_cached_address_and_cycle() {
    let mut model = cached_model();
    let identities = vec![identity("10.40.0.13", 8081, Some("container-b"), true, 4)];
    apply_hill_identities(&mut model, &identities);
    let hill = model.challenges[0].hill.as_ref().unwrap();
    assert_eq!(hill.ip.as_deref(), Some("10.40.0.13"));
    assert_eq!(hill.port, Some(8081));
    assert_eq!(hill.cycle_number, 4);
    assert_eq!(hill.last_check_status.as_deref(), Some("Ok"));
    assert_eq!(hill.last_refresh_round, 4);
    let serialized = serde_json::to_value(hill).unwrap();
    assert!(serialized.get("containerId").is_none());
}

#[test]
fn missing_or_unpublished_live_hill_clears_the_cached_address() {
    for identities in [
        Vec::new(),
        vec![identity("", 0, None, true, 0)],
        vec![identity("10.40.0.13", 8081, None, true, 0)],
    ] {
        let mut model = cached_model();
        apply_hill_identities(&mut model, &identities);
        let hill = model.challenges[0].hill.as_ref().unwrap();
        assert_eq!(hill.ip, None);
        assert_eq!(hill.port, None);
        assert_eq!(
            hill.cycle_number,
            identities.first().map_or(0, |row| row.cycle_number)
        );
    }
}

#[test]
fn external_hill_without_managed_cycle_keeps_its_endpoint() {
    let mut model = cached_model();
    let identities = vec![identity("external.example", 31337, None, false, 0)];
    apply_hill_identities(&mut model, &identities);
    let hill = model.challenges[0].hill.as_ref().unwrap();
    assert_eq!(hill.ip.as_deref(), Some("external.example"));
    assert_eq!(hill.port, Some(31337));
    assert_eq!(hill.cycle_number, 0);
    assert_eq!(hill.last_check_status.as_deref(), Some("Ok"));
    assert_eq!(hill.last_refresh_round, 1);
}

#[test]
fn external_hill_does_not_publish_evidence_from_another_identity() {
    let mut model = cached_model();
    let identities = vec![identity_with_verdict(
        "external.example",
        31337,
        None,
        false,
        0,
        Some("unrelated-runtime"),
        Some(0),
        Some(4),
    )];
    apply_hill_identities(&mut model, &identities);
    let hill = model.challenges[0].hill.as_ref().unwrap();
    assert_eq!(hill.ip.as_deref(), Some("external.example"));
    assert_eq!(hill.last_check_status, None);
    assert_eq!(hill.last_refresh_round, 0);
}

#[test]
fn stale_managed_target_is_not_labeled_as_the_new_cycle() {
    let mut model = cached_model();
    let identities = vec![identity(
        "10.40.0.12",
        8080,
        Some("stale-container"),
        true,
        0,
    )];
    apply_hill_identities(&mut model, &identities);
    let hill = model.challenges[0].hill.as_ref().unwrap();
    assert_eq!(hill.ip, None);
    assert_eq!(hill.port, None);
    assert_eq!(hill.cycle_number, 0);
}

#[test]
fn a_new_target_identity_replaces_all_cached_identity_fields_together() {
    let mut model = cached_model();
    let identities = vec![identity("10.40.0.14", 8082, Some("container-c"), true, 5)];
    apply_hill_identities(&mut model, &identities);
    let hill = model.challenges[0].hill.as_ref().unwrap();
    assert_eq!(hill.ip.as_deref(), Some("10.40.0.14"));
    assert_eq!(hill.port, Some(8082));
    assert_eq!(hill.cycle_number, 5);
}

#[test]
fn replacement_endpoint_never_inherits_the_previous_containers_verdict() {
    let mut model = cached_model();
    let identities = vec![identity_with_verdict(
        "10.40.0.14",
        8082,
        Some("container-c"),
        true,
        5,
        Some("container-b"),
        Some(0),
        Some(5),
    )];
    apply_hill_identities(&mut model, &identities);
    let hill = model.challenges[0].hill.as_ref().unwrap();
    assert_eq!(hill.ip.as_deref(), Some("10.40.0.14"));
    assert_eq!(hill.cycle_number, 5);
    assert_eq!(hill.last_check_status, None);
    assert_eq!(hill.last_refresh_round, 0);
}

#[test]
fn managed_null_identity_never_publishes_a_verdict() {
    let mut model = cached_model();
    let identities = vec![identity_with_verdict(
        "10.40.0.14",
        8082,
        None,
        true,
        5,
        None,
        Some(0),
        Some(5),
    )];
    apply_hill_identities(&mut model, &identities);
    let hill = model.challenges[0].hill.as_ref().unwrap();
    assert_eq!(hill.ip, None);
    assert_eq!(hill.last_check_status, None);
    assert_eq!(hill.last_refresh_round, 0);
}

fn identity(
    host: &str,
    port: i32,
    target_container_id: Option<&str>,
    managed_v2: bool,
    cycle_number: i32,
) -> LiveHillIdentity {
    identity_with_verdict(
        host,
        port,
        target_container_id,
        managed_v2,
        cycle_number,
        target_container_id,
        Some(0),
        Some(cycle_number.max(1)),
    )
}

fn live_ad_identity(
    participation_id: i32,
    host: &str,
    port: i32,
    last_check_status: Option<i16>,
) -> LiveAdServiceIdentity {
    LiveAdServiceIdentity {
        challenge_id: 8,
        participation_id,
        host: host.to_string(),
        port,
        last_check_status,
    }
}

#[allow(clippy::too_many_arguments)]
fn identity_with_verdict(
    host: &str,
    port: i32,
    target_container_id: Option<&str>,
    managed_v2: bool,
    cycle_number: i32,
    verdict_container_id: Option<&str>,
    verdict_status: Option<i16>,
    verdict_round_number: Option<i32>,
) -> LiveHillIdentity {
    LiveHillIdentity {
        challenge_id: 7,
        host: host.to_string(),
        port,
        target_container_id: target_container_id.map(str::to_owned),
        managed_v2,
        cycle_number,
        verdict_container_id: verdict_container_id.map(str::to_owned),
        verdict_status,
        verdict_round_number,
    }
}

fn cached_model() -> AdTargetsModel {
    AdTargetsModel {
        current_round: 4,
        challenges: vec![AdChallengeTargets {
            challenge_id: 7,
            title: "hill".to_string(),
            tick_seconds: 30,
            teams: Vec::new(),
            hill: Some(AdHillTarget {
                ip: Some("10.40.0.12".to_string()),
                port: Some(8080),
                cycle_number: 3,
                last_check_status: Some("Ok".to_string()),
                last_refresh_round: 1,
            }),
        }],
    }
}

fn cached_ad_model() -> AdTargetsModel {
    AdTargetsModel {
        current_round: 4,
        challenges: vec![AdChallengeTargets {
            challenge_id: 8,
            title: "service".to_string(),
            tick_seconds: 30,
            teams: vec![AdTeamTarget {
                participation_id: 41,
                team_name: "caller".to_string(),
                division: None,
                ip: None,
                port: None,
                last_check_status: None,
            }],
            hill: None,
        }],
    }
}
