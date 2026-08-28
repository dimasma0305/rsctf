use super::*;

#[test]
fn public_crypto_routes_have_distinct_source_and_deployment_budgets() {
    assert!(matches!(
        Policy::TeamSignatureAggregate.kind(),
        Kind::Bucket {
            capacity: 256.0,
            refill_per_sec: 32.0,
        }
    ));
    assert!(matches!(
        Policy::PowChallengeAggregate.kind(),
        Kind::Bucket {
            capacity: 64.0,
            refill_per_sec: 4.0,
        }
    ));
    assert!(redis_key(Policy::TeamSignatureAggregate, "deployment").starts_with("rl:tb:23:"));
    assert!(redis_key(Policy::PowChallengeAggregate, "deployment").starts_with("rl:tb:24:"));
    assert!(include_str!("rate_limiter/public_security.rs").contains("\"deployment\""));

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let key = format!("pow-aggregate-{nonce}");
    for _ in 0..64 {
        assert_eq!(check(Policy::PowChallengeAggregate, key.clone()), Ok(()));
    }
    assert_eq!(check(Policy::PowChallengeAggregate, key.clone()), Err(1));
    shard_for(Policy::PowChallengeAggregate, &key)
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&(Policy::PowChallengeAggregate, key));
}

#[test]
fn credential_mutation_routes_keep_the_named_source_budget() {
    let account = include_str!("../controllers/account/mod.rs");
    let admin = include_str!("../controllers/admin/mod.rs");
    for path in [
        "/api/account/changeemail",
        "/api/account/changepassword",
        "/api/account/passwordreset",
    ] {
        let route = &account[account.find(path).expect("credential route")..];
        assert!(
            route[..route.find(".route").unwrap_or(route.len())]
                .contains("Policy::CredentialMutation"),
            "{path} lost credential-mutation admission"
        );
    }
    let reset = &admin[admin
        .find("/api/admin/users/{userid}/password")
        .expect("admin password-reset route")..];
    assert!(reset.contains("Policy::CredentialMutation"));
}

/// Two limiter instances emulate two replicas sharing the same Redis bucket.
#[tokio::test]
async fn public_verifier_budget_is_shared_across_replicas() {
    let Ok(url) = std::env::var("RSCTF_TEST_REDIS_URL") else {
        return;
    };
    let connect = || async {
        redis::Client::open(url.as_str())
            .unwrap()
            .get_connection_manager()
            .await
            .unwrap()
    };
    let node_a = DistributedLimiter {
        conn: connect().await,
    };
    let node_b = DistributedLimiter {
        conn: connect().await,
    };
    let mut admin = connect().await;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let partition = format!("deployment-test-{nonce}");
    let key = redis_key(Policy::TeamSignatureAggregate, &partition);
    redis::cmd("DEL")
        .arg(&key)
        .query_async::<()>(&mut admin)
        .await
        .unwrap();

    let Kind::Bucket { capacity, .. } = Policy::TeamSignatureAggregate.kind() else {
        panic!("team-signature aggregate must use a bucket")
    };
    let mut allowed = 0_u32;
    for index in 0..(capacity as u32 + 40) {
        let node = if index % 2 == 0 { &node_a } else { &node_b };
        if node
            .check(Policy::TeamSignatureAggregate, &partition)
            .await
            .is_ok()
        {
            allowed += 1;
        }
    }
    assert_eq!(
        allowed, capacity as u32,
        "public verifier quota must be one deployment-wide budget"
    );
    redis::cmd("DEL")
        .arg(key)
        .query_async::<()>(&mut admin)
        .await
        .unwrap();
}
