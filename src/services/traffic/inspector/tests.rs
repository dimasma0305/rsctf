use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::atomic::Ordering;

use super::*;
use crate::services::traffic::{write_capture, TrafficPacket};

fn scratch(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "rsctf-flow-inspector-{label}-{}.pcap",
        uuid::Uuid::new_v4()
    ))
}

fn packet(source: SocketAddr, dest: SocketAddr, data: &[u8], millis: u64) -> TrafficPacket {
    TrafficPacket {
        source,
        dest,
        data: data.to_vec(),
        timestamp: Duration::from_millis(millis),
    }
}

#[tokio::test]
async fn real_pcap_contract_groups_directions_and_serves_functional_detail() {
    let path = scratch("contract");
    let team = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 8, 0, 7)), 45_123);
    let container = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(172, 20, 0, 4)), 8080);
    write_capture(
        &path,
        &[
            packet(team, container, b"GET /flag{alpha}", 1_001),
            packet(container, team, b"HTTP/1.1 200 OK", 1_025),
            packet(team, container, b"tail", 1_030),
        ],
    )
    .unwrap();

    let snapshot = load_flow_snapshot(&path, container.port(), None)
        .await
        .unwrap();
    assert_eq!(snapshot.flows().len(), 1);
    let flow = &snapshot.flows()[0];
    assert!(validate_flow_id(&flow.flow_id).is_ok());
    assert_eq!(flow.connection_port, 45_123);
    assert_eq!(flow.peer_ip, "10.8.0.7");
    assert_eq!((flow.first_seen_utc, flow.last_seen_utc), (1_001, 1_030));
    assert_eq!((flow.packets_out, flow.packets_in), (2, 1));
    assert_eq!(flow.flag_hits, 1);
    assert_eq!(flow.chunks.len(), 3);
    assert_eq!(flow.chunks[0].payload, b"GET /flag{alpha}");
    assert_eq!(flow.chunks[0].flag_offsets, vec![5]);

    let same = load_flow_snapshot(&path, container.port(), Some(snapshot.version()))
        .await
        .unwrap();
    assert!(Arc::ptr_eq(&snapshot, &same));
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn real_pcap_duplicate_ports_require_the_canonical_flow_id_for_detail() {
    let path = scratch("duplicate-port");
    let team = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 8, 0, 7)), 45_123);
    let first_container = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(172, 20, 0, 4)), 8_080);
    let second_container = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(172, 20, 0, 5)), 8_080);
    write_capture(
        &path,
        &[
            packet(team, first_container, b"first", 1_001),
            packet(team, second_container, b"second", 1_002),
        ],
    )
    .unwrap();

    let snapshot = load_flow_snapshot(&path, first_container.port(), None)
        .await
        .unwrap();
    assert_eq!(snapshot.flows().len(), 2);
    assert_ne!(snapshot.flows()[0].flow_id, snapshot.flows()[1].flow_id);
    assert_eq!(
        snapshot.flow(45_123, None).unwrap_err(),
        InspectionError::AmbiguousFlow
    );
    for expected in snapshot.flows() {
        let selected = snapshot
            .flow(45_123, Some(&expected.flow_id))
            .unwrap()
            .unwrap();
        assert_eq!(selected.flow_id, expected.flow_id);
        assert_eq!(selected.chunks[0].payload, expected.chunks[0].payload);
    }

    let same = load_flow_snapshot(&path, first_container.port(), Some(snapshot.version()))
        .await
        .unwrap();
    assert_eq!(same.flows()[0].flow_id, snapshot.flows()[0].flow_id);
    assert_eq!(same.flows()[1].flow_id, snapshot.flows()[1].flow_id);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn concurrent_filters_parse_one_file_version_once() {
    let path = scratch("singleflight");
    let team = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 50_001);
    let container = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9000);
    write_capture(&path, &[packet(team, container, b"needle", 20)]).unwrap();
    let before = PARSE_COUNT.load(Ordering::SeqCst);

    let requests = (0..12)
        .map(|_| load_flow_snapshot(&path, container.port(), None))
        .collect::<Vec<_>>();
    let snapshots = futures::future::join_all(requests).await;
    assert!(snapshots.iter().all(Result::is_ok));
    assert_eq!(PARSE_COUNT.load(Ordering::SeqCst) - before, 1);

    let filter =
        ValidatedFlowFilter::new(Some("needle"), Some("127.0"), None, None, None, false).unwrap();
    let page = filter_flow_page(snapshots[0].as_ref().unwrap(), &filter, 1, 25).unwrap();
    assert_eq!((page.indices.len(), page.total_items), (1, 1));
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn replacement_gets_a_new_version_while_cached_detail_keeps_its_snapshot() {
    let path = scratch("replace");
    let team = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 52_000);
    let container = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 7000);
    write_capture(&path, &[packet(team, container, b"old", 10)]).unwrap();
    let old = load_flow_snapshot(&path, container.port(), None)
        .await
        .unwrap();

    write_capture(
        &path,
        &[
            packet(team, container, b"new-version", 20),
            packet(container, team, b"reply", 21),
        ],
    )
    .unwrap();
    let new = load_flow_snapshot(&path, container.port(), None)
        .await
        .unwrap();
    assert_ne!(old.version(), new.version());
    assert_eq!(new.flows()[0].chunks[0].payload, b"new-version");
    let retained_old = load_flow_snapshot(&path, container.port(), Some(old.version()))
        .await
        .unwrap();
    assert_eq!(retained_old.flows()[0].chunks[0].payload, b"old");
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn file_size_and_filter_complexity_are_rejected_before_unbounded_work() {
    let path = scratch("oversize");
    let file = File::create(&path).unwrap();
    file.set_len(MAX_INSPECT_CAPTURE_BYTES + 1).unwrap();
    drop(file);
    assert_eq!(
        load_flow_snapshot(&path, 8080, None).await.unwrap_err(),
        InspectionError::TooLarge
    );
    assert!(matches!(
        ValidatedFlowFilter::new(Some("("), None, None, None, None, false),
        Err(InspectionError::Invalid(_))
    ));
    assert!(matches!(
        ValidatedFlowFilter::new(
            Some(&"a".repeat(MAX_REGEX_PATTERN_BYTES + 1)),
            None,
            None,
            None,
            None,
            false,
        ),
        Err(InspectionError::Invalid(_))
    ));
    assert!(matches!(
        ValidatedFlowFilter::new(None, Some("127.0.0.1/24"), None, None, None, false),
        Err(InspectionError::Invalid(_))
    ));
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn configured_service_port_orients_high_port_response_first_capture() {
    let path = scratch("high-service-port");
    let team = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 8, 0, 9)), 1_234);
    let container = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(172, 20, 0, 9)), 60_000);
    write_capture(
        &path,
        &[
            packet(container, team, b"response-first", 10),
            packet(team, container, b"request", 11),
        ],
    )
    .unwrap();

    let snapshot = load_flow_snapshot(&path, container.port(), None)
        .await
        .unwrap();
    let flow = &snapshot.flows()[0];
    assert_eq!(flow.connection_port, team.port());
    assert_eq!(flow.peer_ip, team.ip().to_string());
    assert_eq!((flow.packets_in, flow.packets_out), (1, 1));
    assert_eq!(flow.chunks[0].direction, FlowDirection::ContainerToTeam);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn ambiguous_service_port_flow_does_not_discard_valid_capture_flows() {
    let path = scratch("ambiguous-service-port");
    let ambiguous_peer = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 8, 0, 10)), 8_080);
    let container = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(172, 20, 0, 10)), 8_080);
    let valid_peer = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 8, 0, 11)), 41_234);
    write_capture(
        &path,
        &[
            packet(ambiguous_peer, container, b"ambiguous-one", 10),
            packet(container, ambiguous_peer, b"ambiguous-two", 11),
            packet(valid_peer, container, b"valid-request", 12),
            packet(container, valid_peer, b"valid-response", 13),
        ],
    )
    .unwrap();

    let snapshot = load_flow_snapshot(&path, container.port(), None)
        .await
        .unwrap();
    assert_eq!(snapshot.flows().len(), 1);
    let flow = &snapshot.flows()[0];
    assert_eq!(flow.connection_port, valid_peer.port());
    assert_eq!(flow.peer_ip, valid_peer.ip().to_string());
    assert_eq!((flow.packets_out, flow.packets_in), (1, 1));
    assert_eq!(flow.chunks[0].payload, b"valid-request");
    assert_eq!(flow.chunks[1].payload, b"valid-response");
    let _ = std::fs::remove_file(path);
}

#[test]
fn an_active_parse_version_cannot_be_registered_twice() {
    let version = format!("active-{}", uuid::Uuid::new_v4());
    let first = ActiveParse::try_begin(&version).expect("first parse owns the version");
    assert!(ActiveParse::try_begin(&version).is_none());
    drop(first);
    assert!(ActiveParse::try_begin(&version).is_some());
}

#[test]
fn dense_flag_prefixes_keep_correct_bounded_marker_semantics() {
    let mut payload = b"flag{".repeat(20_000);
    payload.push(b'}');
    let (hits, offsets) = find_flag_markers(&payload);
    assert_eq!(hits, 52);
    assert_eq!(offsets.len(), 52);

    let (hits, offsets) = find_flag_markers(&b"flag{".repeat(20_000));
    assert_eq!(hits, 0);
    assert!(offsets.is_empty());
}
