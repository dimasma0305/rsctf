use axum::http::Method;

use super::*;

#[test]
fn capability_source_configuration_preserves_roster_capacity() {
    assert_eq!(parse_koth_capability_ip_admission(None), Ok(6_000));
    assert_eq!(parse_koth_capability_ip_admission(Some("3000")), Ok(3_000));
    assert_eq!(parse_koth_capability_ip_admission(Some("6000")), Ok(6_000));
    assert_eq!(
        parse_koth_capability_ip_admission(Some("1000000")),
        Ok(1_000_000)
    );

    for invalid in ["", "2999", "1000001", "not-a-number", " 3000"] {
        let error = parse_koth_capability_ip_admission(Some(invalid)).unwrap_err();
        assert!(error.contains("RSCTF_KOTH_CAPABILITY_IP_ADMISSION_PER_MINUTE"));
        assert!(error.contains("3000 through 1000000"));
    }
}

#[test]
fn policy_is_appended_and_has_constant_size_state() {
    assert!(matches!(
        Policy::KothCapabilityAdmission.kind(),
        Kind::Bucket { .. }
    ));
    assert_eq!(Policy::KothCapabilityAdmission.fixed_window().1, 60_000);
    assert_eq!(
        Policy::KothCapabilityAdmission as u8,
        Policy::EventVpnMintGlobal as u8 + 1,
        "KotH admission must append without renumbering integrated policies"
    );
    assert!(redis_key(Policy::KothCapabilityAdmission, "partition").starts_with("rl:tb:26:"));
}

#[test]
fn only_the_exact_capability_exchange_uses_dedicated_admission() {
    assert!(is_koth_capability_auth_request(
        &Method::POST,
        "/api/v1/koth/capability/authenticate"
    ));
    assert!(!is_koth_capability_auth_request(
        &Method::GET,
        "/api/v1/koth/capability/authenticate"
    ));
    for nearby_path in [
        "/api/v1/koth/capability/authenticate/",
        "/api/v1/koth/capability/authentication",
        "/api/v1/koth/context",
        "/api/v1/koth/observations",
    ] {
        assert!(!is_koth_capability_auth_request(&Method::POST, nearby_path));
    }
}

#[test]
fn two_thousand_capabilities_share_one_source_without_sharing_fairness() {
    let roster_size = crate::services::ad::engine::koth_api::MAX_LEADERBOARD_TEAMS;
    assert_eq!(roster_size, 2_000);
    assert!(Policy::KothCapabilityAdmission.fixed_window().0 >= roster_size as u32);

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let source = format!("managed-koth-source-{nonce}");
    let mut identity_keys = Vec::with_capacity(roster_size);

    for participation_id in 1..=roster_size as i32 {
        assert_eq!(
            check(Policy::KothCapabilityAdmission, source.clone()),
            Ok(()),
            "shared managed source denied participation {participation_id}"
        );
        let identity = koth_capability_partition_key(700_007, 900_009, participation_id);
        assert_eq!(check(Policy::Global, identity.clone()), Ok(()));
        identity_keys.push(identity);
    }

    identity_keys.sort_unstable();
    identity_keys.dedup();
    assert_eq!(identity_keys.len(), roster_size);

    let (reporter_limit, _) = Policy::Global.fixed_window();
    assert_eq!(
        check_weighted(Policy::Global, source.clone(), reporter_limit),
        Ok(())
    );
    assert!(check(Policy::Global, source.clone()).is_err());

    for identity in identity_keys {
        shard_for(Policy::Global, &identity)
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&(Policy::Global, identity));
    }
    for policy in [Policy::KothCapabilityAdmission, Policy::Global] {
        shard_for(policy, &source)
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&(policy, source.clone()));
    }
}

#[test]
fn invalid_capability_abuse_is_bounded_without_starving_reporter() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let source = format!("invalid-managed-koth-source-{nonce}");
    let (source_limit, _) = Policy::KothCapabilityAdmission.fixed_window();

    assert_eq!(
        check_weighted(
            Policy::KothCapabilityAdmission,
            source.clone(),
            source_limit,
        ),
        Ok(())
    );
    assert!(check_weighted(
        Policy::KothCapabilityAdmission,
        source.clone(),
        source_limit,
    )
    .is_err());

    let (reporter_limit, _) = Policy::Global.fixed_window();
    assert_eq!(
        check_weighted(Policy::Global, source.clone(), reporter_limit),
        Ok(())
    );

    for policy in [Policy::KothCapabilityAdmission, Policy::Global] {
        shard_for(policy, &source)
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&(policy, source.clone()));
    }
}

#[test]
fn authenticated_capability_fairness_is_canonical_and_scoped() {
    let first = koth_capability_partition_key(700_008, 900_010, 11);
    assert_eq!(first, koth_capability_partition_key(700_008, 900_010, 11));
    assert_ne!(first, koth_capability_partition_key(700_009, 900_010, 11));
    assert_ne!(first, koth_capability_partition_key(700_008, 900_011, 11));
    assert_ne!(first, koth_capability_partition_key(700_008, 900_010, 12));

    let (identity_limit, _) = Policy::Global.fixed_window();
    assert_eq!(
        check_weighted(Policy::Global, first.clone(), identity_limit),
        Ok(())
    );
    assert!(check(Policy::Global, first.clone()).is_err());
    let other = koth_capability_partition_key(700_008, 900_010, 12);
    assert_eq!(check(Policy::Global, other.clone()), Ok(()));

    for identity in [first, other] {
        shard_for(Policy::Global, &identity)
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&(Policy::Global, identity));
    }
}
