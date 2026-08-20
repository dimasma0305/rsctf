use super::*;

#[test]
fn fingerprint_policy_requires_collection_and_hashes_are_domain_separated() {
    let invalid = PolicyFlags {
        require_unique_fingerprint_global: true,
        ..Default::default()
    };
    assert!(invalid.validate().is_err());
    let valid = PolicyFlags {
        enable_browser_fingerprint: true,
        require_unique_fingerprint_global: true,
        ..Default::default()
    };
    assert!(valid.validate().is_ok());
    let value = "a".repeat(64);
    assert!(valid_browser_fingerprint(&value));
    assert_ne!(
        hash_value(b"test-key", "Ip", &value),
        hash_value(b"test-key", "Fingerprint", &value)
    );
    assert_ne!(
        hash_value(b"test-key", "Ip", &value),
        hash_value(b"other-key", "Ip", &value)
    );
    assert_eq!(hash_value(b"test-key", "Ip", &value).len(), 32);
}

#[test]
fn canonical_exemption_pairs_are_symmetric() {
    let left = Uuid::from_u128(1);
    let right = Uuid::from_u128(2);
    assert_eq!(
        exemption::canonical_pair(left, right),
        exemption::canonical_pair(right, left)
    );
    assert_eq!(exemption::canonical_pair(left, right), (left, right));
}

#[test]
fn ipv4_mapped_ipv6_normalizes_to_ipv4() {
    assert_eq!(
        normalize_ip("::ffff:192.0.2.8".parse().unwrap()),
        "192.0.2.8"
    );
}

#[test]
fn identity_constants_stay_bounded() {
    assert_eq!(IDENTITY_WINDOW_HOURS, 24);
    assert_eq!(EXEMPTION_TTL_DAYS, 7);
    assert_eq!(IdentitySource::Registration.as_str(), "Registration");
    assert_eq!(IdentitySource::Password.as_str(), "Password");
    assert_eq!(IdentitySource::OAuth.as_str(), "OAuth");
    assert_eq!(IdentitySource::TeamJoin.as_str(), "TeamJoin");
    assert_eq!(IdentitySource::GameJoin.as_str(), "GameJoin");
    assert_eq!(IdentitySource::Legacy.as_str(), "Legacy");
}

#[test]
fn network_hashes_use_canonical_prefixes_and_hints_are_masked() {
    let identity = prepare_identity(b"test-key", Some("192.0.2.129"), None);
    let value = &identity.values[0];
    assert_eq!(identity.ip.as_deref(), Some("192.0.2.129"));
    assert_eq!(value.hint, "192.0.2.x");
    assert_eq!(value.subnet_group_hash.as_ref().unwrap().len(), 32);
    assert_eq!(value.broad_network_hash.as_ref().unwrap().len(), 32);
    assert_ne!(value.hint, "192.0.2.129");
    assert_eq!(
        network_prefix("192.0.2.129".parse().unwrap(), 28),
        "192.0.2.128/28"
    );
    assert_eq!(
        network_prefix("192.0.2.129".parse().unwrap(), 20),
        "192.0.0.0/20"
    );
}

#[test]
fn fingerprint_redaction_never_derives_a_hint_from_raw_input() {
    assert_eq!(
        redacted_identity_hint("Fingerprint", &"a".repeat(64)),
        "masked"
    );
    assert_eq!(
        redacted_identity_hint("Fingerprint", "A1B2C3D4E5F6…"),
        "a1b2c3d4e5f6…"
    );
}
