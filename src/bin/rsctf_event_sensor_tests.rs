use super::*;

fn ipv4_udp(payload: &[u8], source: [u8; 4], destination: [u8; 4]) -> Vec<u8> {
    let mut packet = vec![0u8; 20 + 8];
    packet[0] = 0x45;
    packet[9] = 17;
    packet[12..16].copy_from_slice(&source);
    packet[16..20].copy_from_slice(&destination);
    packet[20..22].copy_from_slice(&12345u16.to_be_bytes());
    packet[22..24].copy_from_slice(&53u16.to_be_bytes());
    packet.extend_from_slice(payload);
    packet
}

#[test]
fn parser_never_retains_or_emits_raw_packet_data() {
    let packet = ipv4_udp(b"secret-payload", [10, 13, 0, 2], [1, 1, 1, 1]);
    let parsed = parse_packet(&packet).unwrap();
    assert_eq!(parsed.payload, b"secret-payload");
    let mut tail = Vec::new();
    append_tail(&mut tail, &vec![b'x'; 512]);
    assert_eq!(tail.len(), MAX_FLOW_TAIL);
    assert!(std::mem::size_of::<FlowBucketInput>() < 256);
}

#[test]
fn dns_parser_is_bounded_and_suffix_classifier_is_safe() {
    let mut dns = vec![0u8; 12];
    dns[5] = 1;
    dns.extend_from_slice(&[3, b'a', b'p', b'i', 6]);
    dns.extend_from_slice(b"openai");
    dns.extend_from_slice(&[3]);
    dns.extend_from_slice(b"com");
    dns.extend_from_slice(&[0, 0, 1, 0, 1]);
    assert_eq!(
        parse_dns_question(&dns, false).as_deref(),
        Some("api.openai.com")
    );
    assert!(provider_category("api.openai.com").is_some());
}

#[test]
fn sensor_api_url_requires_exact_loopback_http_or_https() {
    assert_eq!(
        validate_api_url("http://127.0.0.1:8080/").unwrap(),
        "http://127.0.0.1:8080"
    );
    assert!(validate_api_url("http://[::1]:8080").is_ok());
    assert!(validate_api_url("https://sensor.example.test/internal").is_ok());
    assert!(validate_api_url("http://127.0.0.1.attacker.test").is_err());
    assert!(validate_api_url("http://10.13.0.1:8080").is_err());
    assert!(validate_api_url("https://user:secret@sensor.example.test").is_err());
    assert!(validate_api_url("https://sensor.example.test?token=leak").is_err());
}

#[test]
fn startup_snapshot_recovery_is_jittered_and_capped() {
    for attempt in 0..64 {
        let delay = snapshot_retry_delay(attempt, 0x1234_5678_9abc_def0);
        let cap = Duration::from_secs(1_u64.checked_shl(attempt.min(5)).unwrap())
            .min(Duration::from_secs(30));
        assert!(
            delay >= cap / 2,
            "attempt {attempt} was below equal-jitter floor"
        );
        assert!(delay <= cap, "attempt {attempt} exceeded cap");
    }
    assert_ne!(snapshot_retry_delay(3, 1), snapshot_retry_delay(3, 2));
    assert!(snapshot_retry_delay(u32::MAX, 7) <= Duration::from_secs(30));
}

fn flag_input(peer_id: Uuid, value_hash: &str) -> FlagTransportInput {
    FlagTransportInput {
        challenge_id: 7,
        receiving_user_id: Uuid::from_u128(1),
        receiving_participation_id: 11,
        owning_participation_id: 12,
        peer_id,
        flag_value_hash: value_hash.to_string(),
        transport: 0,
        direction: 1,
        observed_at_utc: Utc::now(),
    }
}

#[test]
fn flag_dedup_is_retained_until_durable_batch_acknowledgement() {
    let mut state = CaptureState::default();
    let flag = flag_input(Uuid::from_u128(2), "flag-hash");
    let key = flag_dedup_key(3, &flag);

    assert_eq!(
        track_flag(&mut state.flags, &mut state.seen_flags, 3, flag.clone()),
        TrackFlagResult::Queued
    );
    let batches = completed_batches(&mut state, Utc::now());
    assert_eq!(batches.len(), 1);
    assert!(state.seen_flags.contains(&key));
    assert_eq!(
        track_flag(&mut state.flags, &mut state.seen_flags, 3, flag.clone()),
        TrackFlagResult::Duplicate
    );

    release_flag_dedup(&mut state.seen_flags, std::slice::from_ref(&key));
    assert_eq!(
        track_flag(&mut state.flags, &mut state.seen_flags, 3, flag),
        TrackFlagResult::Queued
    );
}

#[test]
fn acknowledged_flag_dedup_capacity_accepts_new_matches_again() {
    let mut state = CaptureState::default();
    for index in 0..MAX_TRACKED_FLOWS {
        state.seen_flags.insert((
            3,
            Uuid::from_u128(index as u128),
            "flag-hash".to_string(),
            0,
            1,
        ));
    }
    let released = (3, Uuid::from_u128(0), "flag-hash".to_string(), 0, 1);
    let next = flag_input(Uuid::from_u128(MAX_TRACKED_FLOWS as u128 + 1), "next-hash");

    assert_eq!(
        track_flag(&mut state.flags, &mut state.seen_flags, 3, next.clone()),
        TrackFlagResult::Capacity
    );
    release_flag_dedup(&mut state.seen_flags, &[released]);
    assert_eq!(
        track_flag(&mut state.flags, &mut state.seen_flags, 3, next),
        TrackFlagResult::Queued
    );
    assert_eq!(state.seen_flags.len(), MAX_TRACKED_FLOWS);
}
