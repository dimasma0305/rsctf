use super::inspection::{
    cache_entry_expired, inspect_flows_bounded, inspect_flows_bounded_cancellable,
    parse_work_units, retryable_inspection_error, FlowCacheEntry, FLOW_CACHE_TTL,
    FLOW_PARSE_WORK_UNITS, FLOW_PARSE_WORK_UNIT_BYTES, MAX_RETAINED_PAYLOAD_BYTES,
};
use super::*;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::time::Instant;

#[test]
fn inspection_overload_exposes_retry_after() {
    use axum::response::IntoResponse;

    let response = retryable_inspection_error("busy").into_response();
    assert_eq!(
        response.status(),
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::RETRY_AFTER)
            .unwrap(),
        "2"
    );
}

fn scratch(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("rsctf-traffic-test-{name}.pcap"));
    p
}

#[test]
fn round_trip_v4_flows() {
    let path = scratch("v4");
    let a = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 1234);
    let b = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)), 80);
    let packets = vec![
        TrafficPacket {
            source: a,
            dest: b,
            data: b"GET / HTTP/1.1\r\n".to_vec(),
            timestamp: Duration::from_secs(1),
        },
        TrafficPacket {
            source: a,
            dest: b,
            data: b"more".to_vec(),
            timestamp: Duration::from_secs(2),
        },
        TrafficPacket {
            source: b,
            dest: a,
            data: b"HTTP/1.1 200 OK".to_vec(),
            timestamp: Duration::from_secs(3),
        },
    ];
    write_capture(&path, &packets).unwrap();

    let flows = list_flows(&path);
    assert_eq!(flows.len(), 2);
    let ab = flows.iter().find(|f| f.src == a.to_string()).unwrap();
    assert_eq!(ab.packet_count, 2);
    assert_eq!(
        ab.bytes,
        (b"GET / HTTP/1.1\r\n".len() + b"more".len()) as u64
    );
    let ba = flows.iter().find(|f| f.src == b.to_string()).unwrap();
    assert_eq!(ba.packet_count, 1);
    assert_eq!(ba.bytes, b"HTTP/1.1 200 OK".len() as u64);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn round_trip_v6_flow() {
    let path = scratch("v6");
    let a = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 4444);
    let b = SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1)), 9);
    let packets = vec![TrafficPacket {
        source: a,
        dest: b,
        data: b"payload".to_vec(),
        timestamp: Duration::from_secs(5),
    }];
    write_capture(&path, &packets).unwrap();
    let flows = list_flows(&path);
    assert_eq!(flows.len(), 1);
    assert_eq!(flows[0].bytes, b"payload".len() as u64);
    assert_eq!(flows[0].packet_count, 1);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn missing_file_yields_no_flows() {
    assert!(list_flows("/nonexistent/path/does-not-exist.pcap").is_empty());
}

#[test]
fn bounded_flow_reader_enforces_file_and_cardinality_limits() {
    let path = scratch("bounded");
    let a = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1000);
    let b = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 2000);
    let c = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 3000);
    write_capture(
        &path,
        &[
            TrafficPacket::new(a, b, vec![1]),
            TrafficPacket::new(a, c, vec![2]),
        ],
    )
    .unwrap();
    assert!(list_flows_bounded(&path, u64::MAX, 1).is_err());
    assert!(list_flows_bounded(&path, 1, usize::MAX).is_err());
    assert_eq!(list_flows_bounded(&path, u64::MAX, 2).unwrap().len(), 2);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn inspection_work_is_weighted_and_never_exceeds_the_global_budget() {
    assert_eq!(parse_work_units(0), 1);
    assert_eq!(parse_work_units(FLOW_PARSE_WORK_UNIT_BYTES), 1);
    assert_eq!(parse_work_units(FLOW_PARSE_WORK_UNIT_BYTES + 1), 2);
    assert_eq!(parse_work_units(u64::MAX), FLOW_PARSE_WORK_UNITS);
}

#[test]
fn cancelled_blocking_parse_stops_before_consuming_capture_rows() {
    let path = scratch("cancelled-inspection");
    let source = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 50001);
    let destination = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 80);
    write_capture(
        &path,
        &[TrafficPacket::new(source, destination, b"request".to_vec())],
    )
    .unwrap();
    let cancelled = AtomicBool::new(true);
    let error = inspect_flows_bounded_cancellable(
        &path,
        u64::MAX,
        10,
        MAX_RETAINED_PAYLOAD_BYTES,
        Some(&cancelled),
    )
    .unwrap_err();
    assert!(error.to_string().contains("superseded"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn completed_flow_indexes_expire_from_the_process_cache() {
    let entry = FlowCacheEntry {
        inserted_at: Instant::now() - FLOW_CACHE_TTL - Duration::from_millis(1),
        cell: tokio::sync::OnceCell::new(),
    };
    assert!(cache_entry_expired(&entry, Instant::now()));
}

#[test]
fn inspected_flow_merges_directions_and_retains_real_payload_detail() {
    let path = scratch("inspected-detail");
    let team = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)), 42000);
    let container = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 3)), 31337);
    write_capture(
        &path,
        &[
            TrafficPacket {
                source: team,
                dest: container,
                data: b"hello".to_vec(),
                timestamp: Duration::from_millis(1000),
            },
            TrafficPacket {
                source: container,
                dest: team,
                data: b"flag{proof}".to_vec(),
                timestamp: Duration::from_millis(1250),
            },
        ],
    )
    .unwrap();
    let flows = inspect_flows_bounded(&path, u64::MAX, 10, 1024).unwrap();
    assert_eq!(flows.len(), 1);
    let flow = &flows[0];
    assert_eq!(flow.connection_port, 42000);
    assert_eq!(flow.peer_ip, container.ip());
    assert_eq!((flow.packets_out, flow.packets_in), (1, 1));
    assert_eq!(
        (flow.first_seen_millis, flow.last_seen_millis),
        (1000, 1250)
    );
    assert_eq!(flow.flag_hits, 1);
    assert_eq!(flow.chunks.len(), 2);
    assert_eq!(flow.chunks[1].payload, b"flag{proof}");
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn immutable_file_version_is_parsed_once_and_shared() {
    let path = scratch("cached-index");
    let a = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 50000);
    let b = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 80);
    write_capture(&path, &[TrafficPacket::new(a, b, b"GET /".to_vec())]).unwrap();
    let first = inspect_flows_cached(path.clone(), u64::MAX, 10)
        .await
        .unwrap();
    let second = inspect_flows_cached(path.clone(), u64::MAX, 10)
        .await
        .unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn replaced_file_version_never_reuses_the_stale_index() {
    let path = scratch("replaced-index");
    let a = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 50000);
    let b = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 80);
    write_capture(&path, &[TrafficPacket::new(a, b, b"one".to_vec())]).unwrap();
    let first = inspect_flows_cached(path.clone(), u64::MAX, 10)
        .await
        .unwrap();

    write_capture(
        &path,
        &[
            TrafficPacket::new(a, b, b"replacement-one".to_vec()),
            TrafficPacket::new(a, b, b"replacement-two".to_vec()),
        ],
    )
    .unwrap();
    let replacement = inspect_flows_cached(path.clone(), u64::MAX, 10)
        .await
        .unwrap();

    assert!(!Arc::ptr_eq(&first, &replacement));
    assert_eq!(replacement[0].packets_out, 2);
    let _ = std::fs::remove_file(path);
}

#[test]
fn parse_rejects_short_and_non_ip() {
    assert!(parse_frame(&[]).is_none());
    assert!(parse_frame(&[0u8; 10]).is_none());
    let mut arp = vec![0u8; ETH_HDR_LEN + 4];
    arp[12] = 0x08;
    arp[13] = 0x06;
    assert!(parse_frame(&arp).is_none());
}
