//! Credential authentication and mutation admission contracts.

use super::super::*;

#[test]
fn ad_bearer_authentication_is_limited_to_dual_auth_routes() {
    for path in [
        "/api/Game/7/Ad/Submit",
        "/api/Game/7/Ad/Targets",
        "/api/Game/7/Ad/Koth/Token",
        "/api/Game/7/Ad/Koth/Hills",
        "/api/game/7/ad/targets",
    ] {
        assert!(
            route_supports_ad_bearer(path),
            "missing supported route {path}"
        );
    }
    for path in [
        "/api/Game/7/Ad/State",
        "/api/game/7/details",
        "/api/admin/logs",
        "/api/Game/not-a-number/Ad/Submit",
        "/api/Game/-1/Ad/Submit",
    ] {
        assert!(
            !route_supports_ad_bearer(path),
            "unexpected A&D lookup on {path}"
        );
    }
}

#[test]
fn every_ad_bearer_rejection_quota_runs_before_authentication_sql() {
    let source = include_str!("../rate_limiter/global.rs");
    let function = &source[source.find("async fn authenticate_ad_bearer").unwrap()..];
    let source_admission = function.find("Policy::AdBearerSourceAdmission").unwrap();
    let digest_admission = function.find("Policy::AdBearerAdmission").unwrap();
    let identity_admission = function.find("check_authenticated_async").unwrap();
    let query = function
        .find("api_token::authenticate(st.pg(), token)")
        .unwrap();
    assert!(source_admission < query);
    assert!(digest_admission < query);
    assert!(identity_admission < query);
}

#[test]
fn pre_database_admission_has_tight_bounded_buckets() {
    let Kind::Bucket {
        capacity: token_capacity,
        refill_per_sec: token_refill,
    } = Policy::AdBearerAdmission.kind()
    else {
        panic!("token admission must use a bucket");
    };
    let Kind::Bucket {
        capacity: source_capacity,
        refill_per_sec: source_refill,
    } = Policy::AdBearerSourceAdmission.kind()
    else {
        panic!("source admission must use a bucket");
    };
    assert!(token_capacity <= 100.0 && token_refill <= 2.0);
    assert!(source_capacity <= 300.0 && source_refill <= 20.0);
    assert!(AD_AUTH_CONCURRENCY <= 32);
    assert!(AD_AUTH_QUERY_TIMEOUT <= Duration::from_secs(2));
}

#[test]
fn every_personal_token_rejection_budget_runs_before_authentication_sql() {
    let source = include_str!("../rate_limiter/global.rs");
    let function = &source[source
        .find("async fn authenticate_personal_bearer")
        .unwrap()..];
    let source_admission = function
        .find("Policy::PersonalTokenSourceAdmission")
        .unwrap();
    let digest_admission = function.find("Policy::PersonalTokenAdmission").unwrap();
    let query = function.find("api_token::authenticate(st, token)").unwrap();
    assert!(source_admission < query);
    assert!(digest_admission < query);
}

#[test]
fn rotating_invalid_personal_tokens_have_bounded_source_digest_and_query_work() {
    assert!(matches!(
        Policy::PersonalTokenSourceAdmission.kind(),
        Kind::Bucket {
            capacity: 120.0,
            refill_per_sec: 5.0,
        }
    ));
    assert!(matches!(
        Policy::PersonalTokenAdmission.kind(),
        Kind::Bucket {
            capacity: 60.0,
            refill_per_sec: 2.0,
        }
    ));
    assert!(PERSONAL_TOKEN_AUTH_CONCURRENCY <= 16);
    assert!(PERSONAL_TOKEN_AUTH_QUERY_TIMEOUT <= Duration::from_secs(2));
    assert!(redis_key(Policy::PersonalTokenSourceAdmission, "source").starts_with("rl:tb:27:"));
    assert!(redis_key(Policy::PersonalTokenAdmission, "digest").starts_with("rl:tb:28:"));

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let source_key = format!("personal-token-source-{nonce}");
    for _ in 0..120 {
        assert_eq!(
            check(Policy::PersonalTokenSourceAdmission, source_key.clone()),
            Ok(())
        );
    }
    assert_eq!(
        check(Policy::PersonalTokenSourceAdmission, source_key.clone()),
        Err(1)
    );
    shard_for(Policy::PersonalTokenSourceAdmission, &source_key)
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&(Policy::PersonalTokenSourceAdmission, source_key));

    let digest_key = format!("personal-token-digest-{nonce}");
    for _ in 0..60 {
        assert_eq!(
            check(Policy::PersonalTokenAdmission, digest_key.clone()),
            Ok(())
        );
    }
    assert_eq!(
        check(Policy::PersonalTokenAdmission, digest_key.clone()),
        Err(1)
    );
    shard_for(Policy::PersonalTokenAdmission, &digest_key)
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&(Policy::PersonalTokenAdmission, digest_key));
}

#[test]
fn credential_mutations_have_a_named_tight_budget() {
    let Kind::Bucket {
        capacity,
        refill_per_sec,
    } = Policy::CredentialMutation.kind()
    else {
        panic!("credential mutation admission must use a bucket");
    };
    assert_eq!(capacity, 4.0);
    assert_eq!(refill_per_sec, 0.1);
}
