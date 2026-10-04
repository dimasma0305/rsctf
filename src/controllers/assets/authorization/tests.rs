use super::*;

#[test]
fn public_immutable_304_uses_cached_grant_but_private_grants_revalidate() {
    let public = AuthorizedAsset {
        cache_policy: AssetCachePolicy::Public,
        file_size: Some(4),
        signed_delivery_allowed: false,
        final_grant: AssetFinalGrant::Public {
            content_hash: "a".repeat(64),
        },
    };
    assert!(!public.requires_conditional_revalidation());

    let monitor = AuthorizedAsset {
        cache_policy: AssetCachePolicy::PrivateNoStore,
        file_size: Some(4),
        signed_delivery_allowed: false,
        final_grant: AssetFinalGrant::Monitor {
            user_id: Uuid::new_v4(),
            expected_security_stamp: "stamp".to_string(),
        },
    };
    assert!(monitor.requires_conditional_revalidation());
}

#[test]
fn static_attachments_are_privately_cacheable_but_team_files_are_not() {
    let static_gate = AssetGate::Protected {
        file_size: Some(42),
        targets: vec![AssetTarget {
            game_id: 1,
            source_team: None,
            challenge_id: Some(2),
        }],
    };
    let sensitive_gate = AssetGate::Protected {
        file_size: Some(42),
        targets: vec![AssetTarget {
            game_id: 1,
            source_team: Some(3),
            challenge_id: Some(2),
        }],
    };

    let static_delivery = delivery_for_gate(&static_gate);
    assert_eq!(
        static_delivery.cache_policy,
        AssetCachePolicy::PrivateImmutable
    );
    assert!(static_delivery.signed_delivery_allowed);

    let sensitive_delivery = delivery_for_gate(&sensitive_gate);
    assert_eq!(
        sensitive_delivery.cache_policy,
        AssetCachePolicy::PrivateNoStore
    );
    assert!(!sensitive_delivery.signed_delivery_allowed);

    let many_delivery = delivery_for_gate(&AssetGate::ProtectedMany {
        file_size: Some(42),
    });
    assert_eq!(many_delivery.cache_policy, AssetCachePolicy::PrivateNoStore);
    assert!(!many_delivery.signed_delivery_allowed);

    let private_delivery = delivery_for_gate(&AssetGate::Private {
        file_size: Some(42),
    });
    assert_eq!(
        private_delivery.cache_policy,
        AssetCachePolicy::PrivateNoStore
    );
    assert!(!private_delivery.signed_delivery_allowed);

    let public_delivery = delivery_for_gate(&AssetGate::Public {
        file_size: Some(42),
    });
    assert!(
        matches!(public_delivery.final_grant, AssetFinalGrant::None),
        "public downloads must never become participant anti-cheat evidence"
    );
}

#[test]
fn malformed_hashes_never_enter_the_shared_cache_namespace() {
    assert!(valid_content_hash(&"a".repeat(64)));
    assert!(!valid_content_hash("../secrets"));
    assert!(!valid_content_hash(&"g".repeat(64)));
}

#[test]
fn cache_generation_accepts_only_internal_fixed_width_tokens() {
    let token = Uuid::new_v4().simple().to_string();
    assert_eq!(decode_generation(Some(token.as_bytes())), token);
    assert_eq!(decode_generation(None), "0");
    assert_eq!(decode_generation(Some(b"attacker-controlled")), "0");
    assert_ne!(
        asset_gate_cache_key(&"a".repeat(64), &"1".repeat(32), 7),
        asset_gate_cache_key(&"a".repeat(64), &"2".repeat(32), 7)
    );
}

#[test]
fn many_owner_authorization_is_set_based_and_bounded() {
    assert!(MANY_OWNER_AUTHORIZATION_SQL.contains("WITH file AS MATERIALIZED"));
    assert!(MANY_OWNER_AUTHORIZATION_SQL.contains("historical.user_id = $2"));
    assert!(MANY_OWNER_AUTHORIZATION_SQL.contains("LIMIT 1"));
    assert!(!MANY_OWNER_AUTHORIZATION_SQL.contains("OFFSET"));
}

#[test]
fn public_finalization_reproves_every_public_hash_relation() {
    for relation in [
        r#""AspNetUsers" WHERE avatar_hash = $1"#,
        r#""Teams" WHERE avatar_hash = $1"#,
        r#""Games" WHERE poster_hash = $1"#,
        "GlobalConfig:LogoHash",
        "GlobalConfig:FaviconHash",
    ] {
        assert!(
            PUBLIC_ASSET_FINAL_SQL.contains(relation),
            "missing public relation: {relation}"
        );
    }
    assert!(PUBLIC_ASSET_FINAL_SQL.matches("FOR SHARE").count() >= 4);
}
