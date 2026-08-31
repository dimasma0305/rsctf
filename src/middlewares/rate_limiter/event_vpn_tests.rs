use super::*;

fn claims(subject: &str) -> crate::services::token::Claims {
    crate::services::token::Claims {
        sub: subject.to_string(),
        role: 1,
        name: "player".to_string(),
        stamp: "stamp".to_string(),
        iat: 1,
        exp: i64::MAX,
    }
}

#[test]
fn mint_budgets_are_appended_and_partitioned() {
    assert_eq!(
        Policy::EventVpnMint as u8,
        Policy::ProxyTraffic as u8 + 1,
        "new policies must not renumber shipped Redis namespaces"
    );
    assert_eq!(
        Policy::EventVpnMintGlobal as u8,
        Policy::EventVpnMint as u8 + 1
    );
    assert!(matches!(
        Policy::EventVpnMint.kind(),
        Kind::Bucket {
            capacity: 12.0,
            refill_per_sec: 1.0,
        }
    ));
    assert!(matches!(
        Policy::EventVpnMintGlobal.kind(),
        Kind::Bucket {
            capacity: 512.0,
            refill_per_sec: 64.0,
        }
    ));

    let mut request = Request::builder()
        .header("x-real-ip", "192.0.2.44")
        .body(axum::body::Body::empty())
        .unwrap();
    request.extensions_mut().insert(
        crate::middlewares::privilege_authentication::VerifiedSessionClaims(claims("vpn-user")),
    );
    assert_eq!(
        partition_key(Policy::EventVpnMint, &request),
        session_partition_key(&claims("vpn-user"))
    );
    assert_eq!(
        partition_key(Policy::EventVpnMintGlobal, &request),
        "event-vpn-mint-global"
    );
}
