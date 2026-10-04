use std::net::SocketAddr;

use axum::extract::ConnectInfo;

use super::*;

#[test]
fn authenticated_partitions_do_not_share_a_nat_bucket() {
    let mut first = Request::builder()
        .header("x-real-ip", "192.0.2.10")
        .body(axum::body::Body::empty())
        .unwrap();
    first.extensions_mut().insert(
        crate::middlewares::privilege_authentication::VerifiedSessionClaims(claims("user-a")),
    );
    first.extensions_mut().insert(ConnectInfo(
        "192.0.2.10:1234".parse::<SocketAddr>().unwrap(),
    ));
    let mut second = Request::builder()
        .header("x-real-ip", "192.0.2.10")
        .body(axum::body::Body::empty())
        .unwrap();
    second.extensions_mut().insert(
        crate::middlewares::privilege_authentication::VerifiedSessionClaims(claims("user-b")),
    );
    second.extensions_mut().insert(ConnectInfo(
        "192.0.2.10:5678".parse::<SocketAddr>().unwrap(),
    ));
    assert_eq!(partition_key(Policy::Submit, &first).len(), 68);
    assert_eq!(partition_key(Policy::Submit, &second).len(), 68);
    assert_ne!(
        partition_key(Policy::Submit, &first),
        partition_key(Policy::Submit, &second)
    );
    assert_eq!(
        partition_key(Policy::Login, &first),
        partition_key(Policy::Login, &second)
    );
    assert_eq!(partition_key(Policy::Register, &first), "192.0.2.10");
}

#[test]
fn session_partition_binds_subject_and_security_stamp_without_exposing_either() {
    let a = claims("user-a");
    let mut rotated = a.clone();
    rotated.stamp = "stamp-2".to_string();
    let key = session_partition_key(&a);
    assert_eq!(key.len(), 68);
    assert!(key.starts_with("jwt:"));
    assert!(!key.contains(&a.sub));
    assert!(!key.contains(&a.stamp));
    assert_ne!(key, session_partition_key(&rotated));
    assert_ne!(key, session_partition_key(&claims("user-b")));
}

#[test]
fn named_policy_reuses_verified_session_partition_key() {
    let session = claims("user-a");
    let expected = session_partition_key(&session);
    let mut request = Request::builder()
        .header("x-real-ip", "192.0.2.10")
        .body(axum::body::Body::empty())
        .unwrap();
    request
        .extensions_mut()
        .insert(crate::middlewares::privilege_authentication::VerifiedSessionClaims(session));
    request.extensions_mut().insert(ConnectInfo(
        "192.0.2.10:1234".parse::<SocketAddr>().unwrap(),
    ));

    // The fallback remains available to callers that construct the verified
    // claims extension without passing through global_middleware.
    assert_eq!(partition_key(Policy::Submit, &request), expected);

    let cached = "jwt:already-computed".to_string();
    request
        .extensions_mut()
        .insert(VerifiedSessionPartitionKey(cached.clone()));
    assert_eq!(partition_key(Policy::Submit, &request), cached);
    // Anonymous-facing policies must remain source-IP partitioned even when a
    // verified session key is present.
    assert_eq!(partition_key(Policy::Login, &request), "192.0.2.10");
    assert_eq!(
        partition_key(Policy::PrivilegedHubAdmission, &request),
        "192.0.2.10"
    );
    assert_eq!(
        partition_key(Policy::PublicHubAdmission, &request),
        "192.0.2.10"
    );
}

#[test]
fn team_signature_admission_has_source_and_deployment_partitions() {
    let mut request = Request::builder()
        .header("x-real-ip", "192.0.2.44")
        .body(axum::body::Body::empty())
        .unwrap();
    request.extensions_mut().insert(ConnectInfo(
        "192.0.2.44:1234".parse::<SocketAddr>().unwrap(),
    ));
    assert_eq!(
        partition_key(Policy::TeamSignatureSource, &request),
        "192.0.2.44"
    );
    assert_eq!(
        partition_key(Policy::TeamSignatureGlobal, &request),
        "team-signature-global"
    );
    assert!(matches!(
        Policy::TeamSignatureSource.kind(),
        Kind::Bucket {
            capacity: 20.0,
            refill_per_sec: 1.0
        }
    ));
    assert!(matches!(
        Policy::TeamSignatureGlobal.kind(),
        Kind::Bucket {
            capacity: 256.0,
            refill_per_sec: 32.0
        }
    ));
}
