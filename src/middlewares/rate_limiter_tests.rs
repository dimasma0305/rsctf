use super::*;

fn ad_submit_capacity() -> u32 {
    match Policy::AdSubmit.kind() {
        Kind::Bucket { capacity, .. } => capacity as u32,
        Kind::Sliding { .. } => panic!("A&D submit budget must remain a token bucket"),
    }
}

#[test]
fn ad_submit_burst_configuration_requires_a_bounded_integer() {
    assert_eq!(parse_ad_submit_burst_flags(None), Ok(400));
    assert_eq!(parse_ad_submit_burst_flags(Some("100")), Ok(100));
    assert_eq!(parse_ad_submit_burst_flags(Some("400")), Ok(400));
    assert_eq!(parse_ad_submit_burst_flags(Some("3200")), Ok(3_200));

    for invalid in ["", "99", "3201", "not-a-number", " 400"] {
        let error = parse_ad_submit_burst_flags(Some(invalid)).unwrap_err();
        assert!(error.contains("RSCTF_AD_SUBMIT_BURST_FLAGS"));
        assert!(error.contains("100 through 3200"));
    }
}

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

async fn set_redis_counter(
    conn: &mut redis::aio::ConnectionManager,
    key: &str,
    count: u32,
    ttl_ms: u64,
) {
    redis::cmd("SET")
        .arg(key)
        .arg(count)
        .arg("PX")
        .arg(ttl_ms)
        .query_async::<()>(conn)
        .await
        .unwrap();
}

async fn redis_counter(conn: &mut redis::aio::ConnectionManager, key: &str) -> u32 {
    redis::cmd("GET").arg(key).query_async(conn).await.unwrap()
}

async fn redis_time_ms(conn: &mut redis::aio::ConnectionManager) -> i64 {
    let parts: Vec<i64> = redis::cmd("TIME").query_async(conn).await.unwrap();
    parts[0] * 1_000 + parts[1] / 1_000
}

async fn set_redis_bucket(
    conn: &mut redis::aio::ConnectionManager,
    key: &str,
    tokens: f64,
    last_ms: i64,
    ttl_ms: u64,
) {
    redis::cmd("HSET")
        .arg(key)
        .arg("tokens")
        .arg(tokens)
        .arg("last_ms")
        .arg(last_ms)
        .query_async::<()>(conn)
        .await
        .unwrap();
    redis::cmd("PEXPIRE")
        .arg(key)
        .arg(ttl_ms)
        .query_async::<()>(conn)
        .await
        .unwrap();
}

async fn redis_bucket_tokens_optional(
    conn: &mut redis::aio::ConnectionManager,
    key: &str,
) -> Option<f64> {
    redis::cmd("HGET")
        .arg(key)
        .arg("tokens")
        .query_async(conn)
        .await
        .unwrap()
}

async fn redis_bucket_tokens(conn: &mut redis::aio::ConnectionManager, key: &str) -> f64 {
    redis_bucket_tokens_optional(conn, key)
        .await
        .unwrap_or_else(|| panic!("Redis bucket disappeared before inspection: {key}"))
}

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
}

#[test]
fn ad_submit_budget_charges_distinct_work_atomically() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let key = format!("weighted-ad-submit-{nonce}");
    let capacity = ad_submit_capacity();
    assert_eq!(DEFAULT_AD_SUBMIT_BURST_FLAGS, 400);
    assert_eq!(
        check_weighted(Policy::AdSubmit, key.clone(), capacity - 1),
        Ok(())
    );
    assert!(check_weighted(Policy::AdSubmit, key.clone(), 2).is_err());
    assert_eq!(check_weighted(Policy::AdSubmit, key.clone(), 1), Ok(()));
    assert!(check_weighted(Policy::AdSubmit, key, 1).is_err());

    // Oversized weighted charges on a sliding policy fail closed without
    // assuming that a prior hit exists.
    let sliding = format!("weighted-sliding-{nonce}");
    assert!(check_weighted(Policy::Login, sliding, 51).is_err());
}

#[test]
fn ad_submit_default_allows_four_max_batches_per_participation() {
    assert_eq!(ad_submit_capacity(), 400);
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let key = format!("ad-submit-default-boundary-{nonce}");
    let other = format!("ad-submit-default-boundary-other-{nonce}");

    for _ in 0..4 {
        assert_eq!(check_weighted(Policy::AdSubmit, key.clone(), 100), Ok(()));
    }
    assert_eq!(check_weighted(Policy::AdSubmit, key.clone(), 100), Err(10));
    assert_eq!(check_weighted(Policy::AdSubmit, other.clone(), 100), Ok(()));

    for partition in [key, other] {
        shard_for(Policy::AdSubmit, &partition)
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&(Policy::AdSubmit, partition));
    }
}

#[test]
fn redis_result_rounds_retry_after_and_uses_local_fallback() {
    assert_eq!(
        redis_or_local(Ok(1), || panic!("unexpected fallback")),
        Err(1)
    );
    assert_eq!(
        redis_or_local(Ok(1_001), || panic!("unexpected fallback")),
        Err(2)
    );
    assert_eq!(
        redis_or_local(Ok(0), || panic!("unexpected fallback")),
        Ok(())
    );
    assert_eq!(
        redis_or_local(Ok(-1), || panic!("unexpected fallback")),
        Ok(())
    );

    let unavailable = redis::RedisError::from((redis::ErrorKind::Io, "test outage"));
    assert_eq!(redis_or_local(Err(unavailable), || Err(7)), Err(7));

    let unavailable = redis::RedisError::from((redis::ErrorKind::Io, "test outage"));
    assert_eq!(redis_or_local(Err(unavailable), || Ok(())), Ok(()));
}

#[test]
fn distributed_fallback_warning_is_rate_limited() {
    let last = std::sync::atomic::AtomicU64::new(0);
    assert!(claim_redis_fallback_log_slot(&last, 1, 30_000));
    assert!(!claim_redis_fallback_log_slot(&last, 29_999, 30_000));
    assert!(claim_redis_fallback_log_slot(&last, 30_001, 30_000));
}

#[tokio::test]
async fn stalled_redis_command_uses_local_fallback_promptly() {
    let fallback = tokio::time::timeout(Duration::from_secs(1), async {
        redis_or_local(
            redis_with_timeout_for(
                std::future::pending::<redis::RedisResult<i64>>(),
                Duration::from_millis(5),
                "test command timed out",
            )
            .await,
            || Err(7),
        )
    })
    .await
    .expect("a stalled Redis command must not hold the request indefinitely");

    assert_eq!(fallback, Err(7));
    assert_eq!(
        redis_with_timeout_for(
            std::future::ready(Ok::<i64, redis::RedisError>(17)),
            Duration::from_millis(5),
            "test command timed out",
        )
        .await,
        Ok(17),
    );
}

#[tokio::test]
async fn stalled_redis_connection_attempt_times_out_promptly() {
    let result = tokio::time::timeout(Duration::from_secs(1), async {
        redis_with_timeout_for(
            std::future::pending::<redis::RedisResult<()>>(),
            Duration::from_millis(5),
            "test connection timed out",
        )
        .await
    })
    .await
    .expect("a stalled Redis connection attempt must not hold startup indefinitely")
    .unwrap_err();

    assert!(result.to_string().contains("test connection timed out"));
}

#[test]
fn redis_outage_still_enforces_local_weighted_budget() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let key = format!("redis-outage-weighted-{nonce}");
    let outage = || redis::RedisError::from((redis::ErrorKind::Io, "test outage"));

    assert_eq!(
        redis_or_local(Err(outage()), || {
            check_weighted(Policy::AdSubmit, key.clone(), ad_submit_capacity())
        }),
        Ok(())
    );
    assert!(redis_or_local(Err(outage()), || {
        check_weighted(Policy::AdSubmit, key.clone(), 1)
    })
    .is_err());

    shard_for(Policy::AdSubmit, &key)
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&(Policy::AdSubmit, key));
}

#[test]
fn local_authenticated_check_short_circuits_before_ip_backstop() {
    let identity = "test-local-identity-denied".to_string();
    let ip = "test-local-ip-not-counted".to_string();
    let (limit, _) = Policy::Global.fixed_window();
    let now = Instant::now();
    {
        let mut shard = shard_for(Policy::Global, &identity)
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        shard.insert(
            (Policy::Global, identity.clone()),
            State::Sliding(VecDeque::from(vec![now; limit as usize])),
        );
    }

    let outage = redis::RedisError::from((redis::ErrorKind::Io, "test outage"));
    assert!(redis_or_local(Err(outage), || {
        check_authenticated_local(identity.clone(), ip.clone())
    })
    .is_err());
    let ip_was_counted = shard_for(Policy::GlobalIpBackstop, &ip)
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .contains_key(&(Policy::GlobalIpBackstop, ip.clone()));
    assert!(!ip_was_counted);

    shard_for(Policy::Global, &identity)
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&(Policy::Global, identity));
}

#[test]
fn sweep_evicts_buckets_that_refilled_while_idle() {
    let now = Instant::now();
    let mut store = HashMap::new();
    for index in 0..2_048 {
        store.insert(
            (Policy::Submit, format!("idle-{index}")),
            State::Bucket {
                tokens: 0.0,
                last: now - Duration::from_secs(120),
            },
        );
    }
    maybe_sweep(&mut store, now);
    assert!(store.is_empty());
}

#[test]
fn high_source_ceilings_have_constant_size_state() {
    for policy in [Policy::GlobalIpBackstop, Policy::CredentialIpAdmission] {
        assert!(matches!(policy.kind(), Kind::Bucket { .. }));
        assert_eq!(policy.fixed_window().1, 60_000);
    }
    assert!(redis_key(Policy::Global, "partition").starts_with("rl:0:"));
    assert!(redis_key(Policy::AdSubmit, "partition").starts_with("rl:tb:9:"));
    assert!(redis_key(Policy::PrivilegedHubAdmission, "partition").starts_with("rl:tb:10:"));
    assert!(matches!(
        Policy::PrivilegedHubAdmission.kind(),
        Kind::Bucket {
            capacity: 120.0,
            refill_per_sec: 10.0,
        }
    ));
    assert_eq!(Policy::PrivilegedHubAdmission.fixed_window(), (120, 12_000));
}

/// Two `DistributedLimiter` instances = two replicas sharing one Redis. Proves
/// the whole point of the distributed limiter: N nodes enforce ONE combined
/// quota, not N independent ones (two in-process stores would each admit `limit`,
/// i.e. `2 × limit` total — the per-replica bug this fixes). Runs only when
/// `RSCTF_TEST_REDIS_URL` points at a reachable Redis; otherwise it's a no-op.
#[tokio::test]
async fn distributed_limiter_shares_one_counter_across_replicas() {
    let Ok(url) = std::env::var("RSCTF_TEST_REDIS_URL") else {
        return;
    };
    // The opt-in live test keeps Redis's short defaults so a stalled test
    // server fails promptly; production construction uses the shared helper.
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

    let ip = "test_two_replica_client";
    let key = format!("rl:{}:{}", Policy::Global as u8, ip);
    let mut admin = connect().await;
    let _: () = redis::cmd("DEL")
        .arg(&key)
        .query_async(&mut admin)
        .await
        .unwrap();

    let (limit, _) = Policy::Global.fixed_window(); // 150 / 60s
    let mut allowed = 0u32;
    for i in 0..(limit + 40) {
        // Alternate replicas — requests are spread across both nodes.
        let node = if i % 2 == 0 { &node_a } else { &node_b };
        if node.check(Policy::Global, ip).await.is_ok() {
            allowed += 1;
        }
    }

    // Exactly `limit` allowed IN TOTAL across BOTH replicas (a shared counter),
    // NOT `limit` per replica — that's the multi-node correctness guarantee.
    assert_eq!(
        allowed, limit,
        "distributed limiter must enforce one combined quota across replicas"
    );

    let _: () = redis::cmd("DEL")
        .arg(&key)
        .query_async(&mut admin)
        .await
        .unwrap();
}

/// The batched script must be observationally identical to the old ordered
/// pair of Redis checks: Global always increments first, an identity denial
/// leaves the backstop unchanged, and a backstop denial retains both hits.
#[tokio::test]
async fn distributed_authenticated_check_preserves_order_and_counters() {
    let Ok(url) = std::env::var("RSCTF_TEST_REDIS_URL") else {
        return;
    };
    // The opt-in live test keeps Redis's short defaults so a stalled test
    // server fails promptly; production construction uses the shared helper.
    let connect = || async {
        redis::Client::open(url.as_str())
            .unwrap()
            .get_connection_manager()
            .await
            .unwrap()
    };
    let limiter = DistributedLimiter {
        conn: connect().await,
    };
    let mut admin = connect().await;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let (identity_limit, _) = Policy::Global.fixed_window();
    let Kind::Bucket {
        capacity: ip_capacity,
        ..
    } = Policy::GlobalIpBackstop.kind()
    else {
        panic!("IP backstop must be a bucket")
    };

    let denied_identity = format!("batch-denied-identity-{nonce}");
    let untouched_ip = format!("batch-untouched-ip-{nonce}");
    let denied_identity_key = redis_key(Policy::Global, &denied_identity);
    let untouched_ip_key = redis_key(Policy::GlobalIpBackstop, &untouched_ip);
    set_redis_counter(&mut admin, &denied_identity_key, identity_limit, 20_000).await;
    let now_ms = redis_time_ms(&mut admin).await;
    set_redis_bucket(&mut admin, &untouched_ip_key, 41.0, now_ms, 50_000).await;

    let retry = limiter
        .check_authenticated(&denied_identity, &untouched_ip)
        .await
        .unwrap_err();
    assert!((1..=20).contains(&retry));
    assert_eq!(
        redis_counter(&mut admin, &denied_identity_key).await,
        identity_limit + 1
    );
    assert_eq!(
        redis_bucket_tokens(&mut admin, &untouched_ip_key).await,
        41.0,
        "identity denial must short-circuit before the IP counter"
    );

    let allowed_identity = format!("batch-allowed-identity-{nonce}");
    let denied_ip = format!("batch-denied-ip-{nonce}");
    let allowed_identity_key = redis_key(Policy::Global, &allowed_identity);
    let denied_ip_key = redis_key(Policy::GlobalIpBackstop, &denied_ip);
    set_redis_counter(
        &mut admin,
        &allowed_identity_key,
        identity_limit - 1,
        50_000,
    )
    .await;
    // A future timestamp emulates a backwards Redis clock adjustment. The
    // limiter must not mint tokens until the server clock catches up.
    set_redis_bucket(&mut admin, &denied_ip_key, 0.0, now_ms + 10_000, 20_000).await;

    let retry = limiter
        .check_authenticated(&allowed_identity, &denied_ip)
        .await
        .unwrap_err();
    assert_eq!(retry, 1);
    assert_eq!(
        redis_counter(&mut admin, &allowed_identity_key).await,
        identity_limit
    );
    assert_eq!(redis_bucket_tokens(&mut admin, &denied_ip_key).await, 0.0);

    let fresh_identity = format!("batch-fresh-identity-{nonce}");
    let fresh_ip = format!("batch-fresh-ip-{nonce}");
    let fresh_identity_key = redis_key(Policy::Global, &fresh_identity);
    let fresh_ip_key = redis_key(Policy::GlobalIpBackstop, &fresh_ip);
    limiter
        .check_authenticated(&fresh_identity, &fresh_ip)
        .await
        .unwrap();
    assert_eq!(redis_counter(&mut admin, &fresh_identity_key).await, 1);
    let fresh_tokens = redis_bucket_tokens_optional(&mut admin, &fresh_ip_key).await;
    assert!(
        fresh_tokens == Some(ip_capacity - 1.0) || fresh_tokens.is_none(),
        "a present fresh bucket must contain the one-token charge; absence means its short refill TTL already elapsed"
    );

    for key in [
        denied_identity_key,
        untouched_ip_key,
        allowed_identity_key,
        denied_ip_key,
        fresh_identity_key,
        fresh_ip_key,
    ] {
        redis::cmd("DEL")
            .arg(key)
            .query_async::<()>(&mut admin)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn distributed_weighted_charge_sets_expiry_on_first_increment() {
    let Ok(url) = std::env::var("RSCTF_TEST_REDIS_URL") else {
        return;
    };
    let mut admin = redis::Client::open(url.as_str())
        .unwrap()
        .get_connection_manager()
        .await
        .unwrap();
    let limiter = DistributedLimiter {
        conn: admin.clone(),
    };
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let partition = format!("weighted-expiry-{nonce}");
    let key = redis_key(Policy::AdSubmit, &partition);

    limiter
        .check_weighted(Policy::AdSubmit, &partition, 100)
        .await
        .unwrap();
    assert_eq!(
        redis_bucket_tokens(&mut admin, &key).await,
        f64::from(ad_submit_capacity() - 100)
    );
    let ttl_ms: i64 = redis::cmd("PTTL")
        .arg(&key)
        .query_async(&mut admin)
        .await
        .unwrap();
    assert!(
        (9_000..=10_000).contains(&ttl_ms),
        "the bucket key must live exactly until it refills"
    );

    redis::cmd("DEL")
        .arg(key)
        .query_async::<()>(&mut admin)
        .await
        .unwrap();
}

/// Live Redis proof for the production boundary: two replicas share exactly
/// four immediate maximum-size batches, elapsed Redis time refills the bucket,
/// retry-after is derived from the precise deficit, and a backwards clock
/// cannot mint tokens.
#[tokio::test]
#[ignore = "requires RSCTF_TEST_REDIS_URL and a live disposable Redis"]
async fn distributed_bucket_refills_continuously_with_accurate_retry_after() {
    let url = std::env::var("RSCTF_TEST_REDIS_URL")
        .expect("set RSCTF_TEST_REDIS_URL to a disposable Redis instance");
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
    let partition = format!("weighted-native-bucket-{nonce}");
    let key = redis_key(Policy::AdSubmit, &partition);

    assert_eq!(ad_submit_capacity(), 400);
    let (first, second, third, fourth) = tokio::join!(
        node_a.check_weighted(Policy::AdSubmit, &partition, 100),
        node_b.check_weighted(Policy::AdSubmit, &partition, 100),
        node_a.check_weighted(Policy::AdSubmit, &partition, 100),
        node_b.check_weighted(Policy::AdSubmit, &partition, 100),
    );
    assert_eq!([first, second, third, fourth], [Ok(()); 4]);

    // The fifth batch sees the shared empty balance and computes 100 / 10 = 10
    // seconds to retry, regardless of which replica handled the first four.
    let retry_after = node_b
        .check_weighted(Policy::AdSubmit, &partition, 100)
        .await
        .unwrap_err();
    assert!(
        (9..=10).contains(&retry_after),
        "four concurrent Redis calls may spend part of the first refill second"
    );

    // Seed the elapsed interval instead of making this test sleep ten seconds.
    let now_ms = redis_time_ms(&mut admin).await;
    set_redis_bucket(&mut admin, &key, 0.0, now_ms - 10_000, 40_000).await;
    node_a
        .check_weighted(Policy::AdSubmit, &partition, 100)
        .await
        .unwrap();

    // Place the bucket five seconds in the past. At 10 tokens/s it has refilled
    // approximately 50 tokens, leaving another five seconds before a 100-token
    // charge is possible.
    let now_ms = redis_time_ms(&mut admin).await;
    set_redis_bucket(&mut admin, &key, 0.0, now_ms - 5_000, 40_000).await;
    assert_eq!(
        node_a
            .check_weighted(Policy::AdSubmit, &partition, 100)
            .await,
        Err(5)
    );

    // A timestamp in the future must not be treated as a huge elapsed interval.
    set_redis_bucket(&mut admin, &key, 0.0, now_ms + 10_000, 40_000).await;
    assert_eq!(
        node_b.check_weighted(Policy::AdSubmit, &partition, 1).await,
        Err(1)
    );
    assert_eq!(redis_bucket_tokens(&mut admin, &key).await, 0.0);

    redis::cmd("DEL")
        .arg(key)
        .query_async::<()>(&mut admin)
        .await
        .unwrap();
}
