//! Bounded, aggregate-only sensor for traffic crossing the managed event VPN.
//!
//! Packet payloads are inspected only in memory. The process emits five-minute
//! flow counters, fifteen-minute provider categories, keyed peer endpoint
//! observations, and hashes of exact platform-issued dynamic flags. It never
//! writes a pcap, DNS name, raw endpoint, or packet payload.

mod event_sensor_spool;

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::time::Duration;

use aho_corasick::AhoCorasick;
use chrono::{DateTime, TimeZone, Utc};
use hmac::{Hmac, KeyInit, Mac};
use ipnet::IpNet;
use mimalloc::MiMalloc;
use rsctf::services::event_security::{
    provider_category, DnsProviderBucketInput, FlagTransportInput, FlowBucketInput,
    PeerNetworkInput, SensorFlagPattern, SensorPeer, SensorSnapshot, TelemetryBatch,
    MAX_INGEST_ROWS, MAX_PATTERN_BYTES, MAX_TRACKED_FLOWS,
};
use sha2::Sha256;
use tokio::sync::{mpsc, watch};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use event_sensor_spool::{drain_spool, enqueue_batch, DrainError, DurableSpool};

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

const SNAPLEN: i32 = 4_096;
const CAPTURE_BUFFER_BYTES: i32 = 4 * 1024 * 1024;
const FLOW_BUCKET_SECONDS: i64 = 5 * 60;
const DNS_BUCKET_SECONDS: i64 = 15 * 60;
const MAX_FLOW_TAIL: usize = 126;
const MAX_DESTINATIONS_PER_BUCKET: usize = 1_024;
const BATCH_QUEUE: usize = 2;

#[derive(Clone)]
struct Config {
    api: String,
    token: String,
    interface: String,
    asn_file: Option<String>,
    spool_dir: std::path::PathBuf,
}

impl Config {
    fn from_env() -> anyhow::Result<Self> {
        let api = validate_api_url(
            &std::env::var("RSCTF_EVENT_SENSOR_API_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:8080".to_string()),
        )?;
        let token = std::env::var("RSCTF_EVENT_SENSOR_TOKEN").unwrap_or_default();
        if token.len() < 32 || token.chars().any(char::is_whitespace) {
            anyhow::bail!("RSCTF_EVENT_SENSOR_TOKEN must contain 32 non-whitespace characters");
        }
        Ok(Self {
            api,
            token,
            interface: std::env::var("RSCTF_EVENT_SENSOR_INTERFACE")
                .unwrap_or_else(|_| "wg0".to_string()),
            asn_file: std::env::var("RSCTF_EVENT_SENSOR_ASN_FILE")
                .ok()
                .filter(|value| !value.trim().is_empty()),
            spool_dir: std::env::var_os("RSCTF_EVENT_SENSOR_SPOOL_DIR")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| "/var/lib/rsctf-event-sensor/spool".into()),
        })
    }
}

fn validate_api_url(value: &str) -> anyhow::Result<String> {
    let parsed = reqwest::Url::parse(value)?;
    let loopback_http = parsed.scheme() == "http"
        && parsed
            .host_str()
            .and_then(|host| {
                host.trim_matches(|character| matches!(character, '[' | ']'))
                    .parse::<IpAddr>()
                    .ok()
            })
            .is_some_and(|address| address.is_loopback());
    if !(loopback_http || parsed.scheme() == "https")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        anyhow::bail!("RSCTF_EVENT_SENSOR_API_URL must be loopback HTTP or an HTTPS endpoint");
    }
    Ok(parsed.as_str().trim_end_matches('/').to_string())
}

#[derive(Clone)]
struct PeerContext {
    game_id: i32,
    peer: SensorPeer,
    behavior: bool,
    flag_scan: bool,
    dns: bool,
    asn: bool,
    devices: bool,
}

#[derive(Clone)]
struct PatternContext {
    game_id: i32,
    pattern: SensorFlagPattern,
}

#[derive(Clone, Default)]
struct CompiledSnapshot {
    peers_by_ip: HashMap<IpAddr, PeerContext>,
    peers_by_key: HashMap<String, PeerContext>,
    patterns: Vec<PatternContext>,
    matcher: Option<AhoCorasick>,
}

impl CompiledSnapshot {
    fn build(snapshot: SensorSnapshot) -> anyhow::Result<Self> {
        let mut peers_by_ip = HashMap::new();
        let mut peers_by_key = HashMap::new();
        let mut patterns = Vec::new();
        for game in snapshot.games {
            for peer in game.peers {
                let context = PeerContext {
                    game_id: game.game_id,
                    peer: peer.clone(),
                    behavior: game.behavior_telemetry_enabled,
                    flag_scan: game.flag_scan_enabled,
                    dns: game.provider_dns_telemetry_enabled,
                    asn: game.source_asn_telemetry_enabled,
                    devices: game.device_sharing_telemetry_enabled,
                };
                if let Ok(address) = peer.address.parse::<IpAddr>() {
                    peers_by_ip.insert(address, context.clone());
                }
                peers_by_key.insert(peer.public_key.clone(), context);
            }
            if game.flag_scan_enabled {
                patterns.extend(
                    game.flag_patterns
                        .into_iter()
                        .map(|pattern| PatternContext {
                            game_id: game.game_id,
                            pattern,
                        }),
                );
            }
        }
        let pattern_bytes = patterns
            .iter()
            .map(|pattern| pattern.pattern.pattern.len())
            .try_fold(0usize, usize::checked_add)
            .unwrap_or(usize::MAX);
        if pattern_bytes > MAX_PATTERN_BYTES {
            anyhow::bail!("event flag patterns exceed the sensor memory budget");
        }
        let matcher = if patterns.is_empty() {
            None
        } else {
            Some(AhoCorasick::new(
                patterns
                    .iter()
                    .map(|pattern| pattern.pattern.pattern.as_bytes()),
            )?)
        };
        Ok(Self {
            peers_by_ip,
            peers_by_key,
            patterns,
            matcher,
        })
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct FlowKey {
    game_id: i32,
    peer_id: Uuid,
    remote: IpAddr,
    remote_port: u16,
    protocol: u8,
    bucket: i64,
}

struct FlowState {
    peer: SensorPeer,
    emit_behavior: bool,
    packets_up: i64,
    packets_down: i64,
    bytes_up: i64,
    bytes_down: i64,
    connection_count: i32,
    first_seen_at_utc: DateTime<Utc>,
    last_seen_at_utc: DateTime<Utc>,
    tail: Vec<u8>,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct DnsKey {
    game_id: i32,
    peer_id: Uuid,
    category: i16,
    bucket: i64,
}

struct DnsState {
    peer: SensorPeer,
    count: i32,
    first: DateTime<Utc>,
    last: DateTime<Utc>,
}

#[derive(Default)]
struct CaptureState {
    flows: HashMap<FlowKey, FlowState>,
    destinations: HashMap<(i32, Uuid, i64), HashSet<IpAddr>>,
    dns: HashMap<DnsKey, DnsState>,
    flags: Vec<(i32, FlagTransportInput)>,
    seen_flags: HashSet<(i32, Uuid, String, i16, i16)>,
    pending_drops: HashMap<i32, (u64, u64)>,
}

impl CaptureState {
    fn record_drop(&mut self, game_id: i32, rows: u64, bytes: u64) {
        let entry = self.pending_drops.entry(game_id).or_default();
        entry.0 = entry.0.saturating_add(rows);
        entry.1 = entry.1.saturating_add(bytes);
    }
}

struct ParsedPacket<'a> {
    source: IpAddr,
    destination: IpAddr,
    source_port: u16,
    destination_port: u16,
    protocol: u8,
    tcp_flags: u8,
    payload: &'a [u8],
}

fn parse_packet(data: &[u8]) -> Option<ParsedPacket<'_>> {
    let offset = match data.first().map(|byte| byte >> 4) {
        Some(4 | 6) => 0,
        _ if data.len() >= 14
            && matches!(u16::from_be_bytes([data[12], data[13]]), 0x0800 | 0x86dd) =>
        {
            14
        }
        _ => return None,
    };
    let version = data.get(offset)? >> 4;
    let (source, destination, protocol, transport) = match version {
        4 => {
            if data.len() < offset + 20 {
                return None;
            }
            let header = usize::from(data[offset] & 0x0f) * 4;
            if header < 20 || data.len() < offset + header {
                return None;
            }
            (
                IpAddr::V4(Ipv4Addr::new(
                    data[offset + 12],
                    data[offset + 13],
                    data[offset + 14],
                    data[offset + 15],
                )),
                IpAddr::V4(Ipv4Addr::new(
                    data[offset + 16],
                    data[offset + 17],
                    data[offset + 18],
                    data[offset + 19],
                )),
                data[offset + 9],
                offset + header,
            )
        }
        6 => {
            if data.len() < offset + 40 {
                return None;
            }
            let source = <[u8; 16]>::try_from(&data[offset + 8..offset + 24]).ok()?;
            let destination = <[u8; 16]>::try_from(&data[offset + 24..offset + 40]).ok()?;
            (
                IpAddr::V6(Ipv6Addr::from(source)),
                IpAddr::V6(Ipv6Addr::from(destination)),
                data[offset + 6],
                offset + 40,
            )
        }
        _ => return None,
    };
    if data.len() < transport + 8 {
        return None;
    }
    let source_port = u16::from_be_bytes([data[transport], data[transport + 1]]);
    let destination_port = u16::from_be_bytes([data[transport + 2], data[transport + 3]]);
    let (payload_offset, tcp_flags) = match protocol {
        6 => {
            if data.len() < transport + 20 {
                return None;
            }
            let header = usize::from(data[transport + 12] >> 4) * 4;
            if header < 20 || data.len() < transport + header {
                return None;
            }
            (transport + header, data[transport + 13])
        }
        17 => (transport + 8, 0),
        _ => return None,
    };
    Some(ParsedPacket {
        source,
        destination,
        source_port,
        destination_port,
        protocol,
        tcp_flags,
        payload: data.get(payload_offset..).unwrap_or_default(),
    })
}

fn bucket(timestamp: DateTime<Utc>, seconds: i64) -> i64 {
    timestamp.timestamp().div_euclid(seconds) * seconds
}

fn parse_dns_question(payload: &[u8], tcp: bool) -> Option<String> {
    let payload = if tcp { payload.get(2..)? } else { payload };
    if payload.len() < 12
        || payload[2] & 0x80 != 0
        || u16::from_be_bytes([payload[4], payload[5]]) == 0
    {
        return None;
    }
    let mut offset = 12usize;
    let mut labels = Vec::new();
    while offset < payload.len() {
        let length = usize::from(payload[offset]);
        offset += 1;
        if length == 0 {
            break;
        }
        if length > 63 || offset.saturating_add(length) > payload.len() {
            return None;
        }
        labels.push(std::str::from_utf8(&payload[offset..offset + length]).ok()?);
        offset += length;
        if labels.len() > 32 {
            return None;
        }
    }
    (!labels.is_empty()).then(|| labels.join("."))
}

fn append_tail(tail: &mut Vec<u8>, payload: &[u8]) {
    if payload.len() >= MAX_FLOW_TAIL {
        tail.clear();
        tail.extend_from_slice(&payload[payload.len() - MAX_FLOW_TAIL..]);
        return;
    }
    let keep = MAX_FLOW_TAIL.saturating_sub(payload.len()).min(tail.len());
    if keep < tail.len() {
        tail.drain(..tail.len() - keep);
    }
    tail.extend_from_slice(payload);
}

fn inspect_packet(
    state: &mut CaptureState,
    snapshot: &CompiledSnapshot,
    packet: ParsedPacket<'_>,
    now: DateTime<Utc>,
) {
    let (peer, outbound, remote, remote_port) =
        if let Some(peer) = snapshot.peers_by_ip.get(&packet.source) {
            (peer, true, packet.destination, packet.destination_port)
        } else if let Some(peer) = snapshot.peers_by_ip.get(&packet.destination) {
            (peer, false, packet.source, packet.source_port)
        } else {
            return;
        };
    if !(peer.behavior || peer.flag_scan || peer.dns) {
        return;
    }
    let flow_bucket = bucket(now, FLOW_BUCKET_SECONDS);
    let key = FlowKey {
        game_id: peer.game_id,
        peer_id: peer.peer.peer_id,
        remote,
        remote_port,
        protocol: packet.protocol,
        bucket: flow_bucket,
    };
    if !state.flows.contains_key(&key) && state.flows.len() >= MAX_TRACKED_FLOWS {
        state.record_drop(
            peer.game_id,
            1,
            u64::try_from(packet.payload.len()).unwrap_or(u64::MAX),
        );
        return;
    }
    let flow = state.flows.entry(key).or_insert_with(|| FlowState {
        peer: peer.peer.clone(),
        emit_behavior: peer.behavior,
        packets_up: 0,
        packets_down: 0,
        bytes_up: 0,
        bytes_down: 0,
        connection_count: i32::from(packet.protocol == 17 || packet.tcp_flags & 0x02 != 0),
        first_seen_at_utc: now,
        last_seen_at_utc: now,
        tail: Vec::new(),
    });
    flow.last_seen_at_utc = now;
    if outbound {
        flow.packets_up = flow.packets_up.saturating_add(1);
        flow.bytes_up = flow
            .bytes_up
            .saturating_add(i64::try_from(packet.payload.len()).unwrap_or(i64::MAX));
    } else {
        flow.packets_down = flow.packets_down.saturating_add(1);
        flow.bytes_down = flow
            .bytes_down
            .saturating_add(i64::try_from(packet.payload.len()).unwrap_or(i64::MAX));
    }
    if peer.behavior {
        let destinations = state
            .destinations
            .entry((peer.game_id, peer.peer.peer_id, flow_bucket))
            .or_default();
        if destinations.len() < MAX_DESTINATIONS_PER_BUCKET {
            destinations.insert(remote);
        }
    }

    if peer.dns && outbound && packet.destination_port == 53 {
        if let Some(name) = parse_dns_question(packet.payload, packet.protocol == 6) {
            if let Some(category) = provider_category(&name) {
                let dns_bucket = bucket(now, DNS_BUCKET_SECONDS);
                let dns = state
                    .dns
                    .entry(DnsKey {
                        game_id: peer.game_id,
                        peer_id: peer.peer.peer_id,
                        category,
                        bucket: dns_bucket,
                    })
                    .or_insert_with(|| DnsState {
                        peer: peer.peer.clone(),
                        count: 0,
                        first: now,
                        last: now,
                    });
                dns.count = dns.count.saturating_add(1);
                dns.last = now;
            }
        }
    }

    if !peer.flag_scan {
        return;
    }
    let Some(matcher) = snapshot.matcher.as_ref() else {
        append_tail(&mut flow.tail, packet.payload);
        return;
    };
    let mut scan = Vec::with_capacity(flow.tail.len().saturating_add(packet.payload.len()));
    scan.extend_from_slice(&flow.tail);
    scan.extend_from_slice(packet.payload);
    let mut dropped_flag_matches = 0u64;
    for found in matcher.find_iter(&scan) {
        let pattern = &snapshot.patterns[found.pattern().as_usize()];
        if pattern.game_id != peer.game_id
            || pattern.pattern.owning_participation_id == peer.peer.participation_id
        {
            continue;
        }
        let transport = if packet.protocol == 6 { 0 } else { 1 };
        let direction = if outbound { 0 } else { 1 };
        let dedup = (
            peer.game_id,
            peer.peer.peer_id,
            pattern.pattern.value_hash.clone(),
            transport,
            direction,
        );
        if state.seen_flags.contains(&dedup) {
            continue;
        }
        if state.seen_flags.len() < MAX_TRACKED_FLOWS {
            state.seen_flags.insert(dedup);
            state.flags.push((
                peer.game_id,
                FlagTransportInput {
                    challenge_id: pattern.pattern.challenge_id,
                    receiving_user_id: peer.peer.user_id,
                    receiving_participation_id: peer.peer.participation_id,
                    owning_participation_id: pattern.pattern.owning_participation_id,
                    peer_id: peer.peer.peer_id,
                    flag_value_hash: pattern.pattern.value_hash.clone(),
                    transport,
                    direction,
                    observed_at_utc: now,
                },
            ));
        } else {
            dropped_flag_matches = dropped_flag_matches.saturating_add(1);
        }
    }
    append_tail(&mut flow.tail, packet.payload);
    if dropped_flag_matches > 0 {
        state.record_drop(
            peer.game_id,
            dropped_flag_matches,
            u64::try_from(packet.payload.len())
                .unwrap_or(u64::MAX)
                .saturating_mul(dropped_flag_matches),
        );
    }
}

fn completed_batches(state: &mut CaptureState, now: DateTime<Utc>) -> Vec<TelemetryBatch> {
    let flow_cutoff = bucket(now, FLOW_BUCKET_SECONDS);
    let dns_cutoff = bucket(now, DNS_BUCKET_SECONDS);
    let mut batches = HashMap::<i32, TelemetryBatch>::new();
    let completed_flow_keys = state
        .flows
        .keys()
        .filter(|key| key.bucket < flow_cutoff)
        .cloned()
        .collect::<Vec<_>>();
    let mut aggregate = HashMap::<(i32, Uuid, i64), FlowBucketInput>::new();
    for key in completed_flow_keys {
        let Some(flow) = state.flows.remove(&key) else {
            continue;
        };
        if !flow.emit_behavior {
            continue;
        }
        let row = aggregate
            .entry((key.game_id, key.peer_id, key.bucket))
            .or_insert_with(|| FlowBucketInput {
                user_id: flow.peer.user_id,
                participation_id: flow.peer.participation_id,
                peer_id: flow.peer.peer_id,
                challenge_id: None,
                container_generation: None,
                bucket_start_utc: Utc.timestamp_opt(key.bucket, 0).single().unwrap_or(now),
                packets_up: 0,
                packets_down: 0,
                bytes_up: 0,
                bytes_down: 0,
                distinct_destinations: 0,
                connection_count: 0,
                active_seconds: 0,
            });
        row.packets_up = row.packets_up.saturating_add(flow.packets_up);
        row.packets_down = row.packets_down.saturating_add(flow.packets_down);
        row.bytes_up = row.bytes_up.saturating_add(flow.bytes_up);
        row.bytes_down = row.bytes_down.saturating_add(flow.bytes_down);
        row.connection_count = row.connection_count.saturating_add(flow.connection_count);
        row.active_seconds = row.active_seconds.max(
            i32::try_from(
                (flow.last_seen_at_utc - flow.first_seen_at_utc)
                    .num_seconds()
                    .saturating_add(1)
                    .clamp(1, FLOW_BUCKET_SECONDS),
            )
            .unwrap_or(300),
        );
    }
    for ((game_id, peer_id, bucket), mut row) in aggregate {
        row.distinct_destinations = i32::try_from(
            state
                .destinations
                .remove(&(game_id, peer_id, bucket))
                .map_or(0, |destinations| destinations.len()),
        )
        .unwrap_or(i32::MAX);
        batches
            .entry(game_id)
            .or_insert_with(|| empty_batch(game_id))
            .flows
            .push(row);
    }
    let dns_keys = state
        .dns
        .keys()
        .filter(|key| key.bucket < dns_cutoff)
        .cloned()
        .collect::<Vec<_>>();
    for key in dns_keys {
        let Some(dns) = state.dns.remove(&key) else {
            continue;
        };
        batches
            .entry(key.game_id)
            .or_insert_with(|| empty_batch(key.game_id))
            .dns_providers
            .push(DnsProviderBucketInput {
                user_id: dns.peer.user_id,
                participation_id: dns.peer.participation_id,
                peer_id: dns.peer.peer_id,
                provider_category: key.category,
                bucket_start_utc: Utc.timestamp_opt(key.bucket, 0).single().unwrap_or(now),
                query_count: dns.count,
                first_seen_at_utc: dns.first,
                last_seen_at_utc: dns.last,
            });
    }
    for (game_id, flag) in state.flags.drain(..) {
        batches
            .entry(game_id)
            .or_insert_with(|| empty_batch(game_id))
            .flag_transports
            .push(flag);
    }
    for (game_id, (rows, bytes)) in state.pending_drops.drain() {
        let batch = batches
            .entry(game_id)
            .or_insert_with(|| empty_batch(game_id));
        batch.sensor_dropped_rows = i64::try_from(rows).unwrap_or(i64::MAX);
        batch.sensor_dropped_bytes = i64::try_from(bytes).unwrap_or(i64::MAX);
    }
    batches.into_values().collect()
}

fn empty_batch(game_id: i32) -> TelemetryBatch {
    TelemetryBatch {
        batch_id: Uuid::new_v4(),
        game_id,
        flows: Vec::new(),
        dns_providers: Vec::new(),
        peer_networks: Vec::new(),
        flag_transports: Vec::new(),
        sensor_dropped_rows: 0,
        sensor_dropped_bytes: 0,
    }
}

fn trim_batch(batch: &mut TelemetryBatch) -> u64 {
    let original = batch.flows.len()
        + batch.dns_providers.len()
        + batch.peer_networks.len()
        + batch.flag_transports.len();
    let mut remaining = MAX_INGEST_ROWS;
    batch.flows.truncate(remaining);
    remaining = remaining.saturating_sub(batch.flows.len());
    batch.dns_providers.truncate(remaining);
    remaining = remaining.saturating_sub(batch.dns_providers.len());
    batch.peer_networks.truncate(remaining);
    remaining = remaining.saturating_sub(batch.peer_networks.len());
    batch.flag_transports.truncate(remaining);
    u64::try_from(original.saturating_sub(
        batch.flows.len()
            + batch.dns_providers.len()
            + batch.peer_networks.len()
            + batch.flag_transports.len(),
    ))
    .unwrap_or(u64::MAX)
}

fn capture_loop(
    interface: String,
    mut snapshots: watch::Receiver<Arc<CompiledSnapshot>>,
    batches: mpsc::Sender<TelemetryBatch>,
) -> anyhow::Result<()> {
    let mut snapshot = snapshots.borrow_and_update().clone();
    let mut state = CaptureState::default();
    let mut last_flush = std::time::Instant::now();
    loop {
        let opened = pcap::Capture::from_device(interface.as_str()).and_then(|capture| {
            capture
                .promisc(false)
                .immediate_mode(true)
                .snaplen(SNAPLEN)
                .buffer_size(CAPTURE_BUFFER_BYTES)
                .timeout(200)
                .open()
        });
        let mut capture = match opened {
            Ok(mut capture) => {
                if let Err(error) = capture.filter("(ip or ip6) and (tcp or udp)", true) {
                    tracing::warn!(%error, %interface, "event sensor capture filter unavailable; retrying");
                    std::thread::sleep(Duration::from_secs(5));
                    continue;
                }
                capture
            }
            Err(error) => {
                tracing::warn!(%error, %interface, "event sensor interface unavailable; retrying");
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };
        tracing::info!(%interface, "event sensor capture active");
        loop {
            if snapshots.has_changed().unwrap_or(false) {
                snapshot = snapshots.borrow_and_update().clone();
            }
            match capture.next_packet() {
                Ok(packet) => {
                    if let Some(packet) = parse_packet(packet.data) {
                        inspect_packet(&mut state, &snapshot, packet, Utc::now());
                    }
                }
                Err(pcap::Error::TimeoutExpired) => {}
                Err(error) => {
                    tracing::warn!(%error, %interface, "event sensor capture interrupted; reopening");
                    break;
                }
            }
            if last_flush.elapsed() >= Duration::from_secs(30) {
                for mut batch in completed_batches(&mut state, Utc::now()) {
                    let trimmed = trim_batch(&mut batch);
                    batch.sensor_dropped_rows = batch
                        .sensor_dropped_rows
                        .saturating_add(i64::try_from(trimmed).unwrap_or(i64::MAX));
                    if let Err(error) = batches.try_send(batch) {
                        let lost = error.into_inner();
                        let lost_rows = lost
                            .flows
                            .len()
                            .saturating_add(lost.dns_providers.len())
                            .saturating_add(lost.peer_networks.len())
                            .saturating_add(lost.flag_transports.len());
                        state.record_drop(
                            lost.game_id,
                            u64::try_from(lost_rows).unwrap_or(u64::MAX).saturating_add(
                                u64::try_from(lost.sensor_dropped_rows).unwrap_or(u64::MAX),
                            ),
                            u64::try_from(lost.sensor_dropped_bytes).unwrap_or(u64::MAX),
                        );
                    }
                }
                last_flush = std::time::Instant::now();
            }
        }
    }
}

#[derive(Clone)]
struct AsnPrefix {
    network: IpNet,
    asn: i64,
    class: i16,
}

fn load_asn_prefixes(path: Option<&str>) -> anyhow::Result<Vec<AsnPrefix>> {
    let Some(path) = path else {
        return Ok(Vec::new());
    };
    let mut prefixes = Vec::new();
    for (index, line) in std::fs::read_to_string(path)?.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields = line.split(',').map(str::trim).collect::<Vec<_>>();
        if fields.len() != 3 {
            anyhow::bail!("invalid ASN prefix line {}", index + 1);
        }
        let network = fields[0].parse::<IpNet>()?;
        let asn = fields[1].parse::<i64>()?;
        let class = fields[2].parse::<i16>()?;
        if !(0..=4_294_967_295).contains(&asn) || !(0..=7).contains(&class) {
            anyhow::bail!("invalid ASN/class on line {}", index + 1);
        }
        prefixes.push(AsnPrefix {
            network,
            asn,
            class,
        });
    }
    prefixes.sort_by_key(|entry| std::cmp::Reverse(entry.network.prefix_len()));
    Ok(prefixes)
}

fn endpoint_hash(
    token: &str,
    game_id: i32,
    peer_id: Uuid,
    endpoint: IpAddr,
) -> anyhow::Result<String> {
    let mut mac = Hmac::<Sha256>::new_from_slice(token.as_bytes())?;
    mac.update(b"rsctf:event-sensor:endpoint:v1\0");
    mac.update(&game_id.to_be_bytes());
    mac.update(peer_id.as_bytes());
    match endpoint {
        IpAddr::V4(value) => mac.update(&value.octets()),
        IpAddr::V6(value) => mac.update(&value.octets()),
    }
    Ok(hex::encode(mac.finalize().into_bytes()))
}

async fn network_observations(
    config: &Config,
    snapshot: &CompiledSnapshot,
    prefixes: &[AsnPrefix],
) -> Vec<TelemetryBatch> {
    let now = Utc::now();
    let observation_bucket = Utc
        .timestamp_opt(bucket(now, DNS_BUCKET_SECONDS), 0)
        .single()
        .unwrap_or(now);
    let mut batches = HashMap::<i32, TelemetryBatch>::new();
    for peer in snapshot.peers_by_key.values() {
        if !(peer.asn || peer.devices) {
            continue;
        }
        let Some(endpoint) = peer
            .peer
            .endpoint
            .as_deref()
            .and_then(|value| value.parse::<IpAddr>().ok())
        else {
            continue;
        };
        let (asn, class) = prefixes
            .iter()
            .find(|entry| entry.network.contains(&endpoint))
            .map_or((None, 0), |entry| (Some(entry.asn), entry.class));
        let Ok(hash) = endpoint_hash(&config.token, peer.game_id, peer.peer.peer_id, endpoint)
        else {
            continue;
        };
        batches
            .entry(peer.game_id)
            .or_insert_with(|| empty_batch(peer.game_id))
            .peer_networks
            .push(PeerNetworkInput {
                user_id: peer.peer.user_id,
                participation_id: peer.peer.participation_id,
                peer_id: peer.peer.peer_id,
                endpoint_hash: hash,
                source_asn: asn,
                network_class: class,
                first_seen_at_utc: observation_bucket,
                last_seen_at_utc: now,
                handshake_count: 1,
            });
    }
    batches.into_values().collect()
}

async fn fetch_snapshot(
    client: &reqwest::Client,
    config: &Config,
) -> anyhow::Result<SensorSnapshot> {
    Ok(client
        .get(format!(
            "{}/api/internal/event-security/snapshot",
            config.api
        ))
        .bearer_auth(&config.token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

fn terminal_snapshot_error(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<reqwest::Error>()
        .and_then(reqwest::Error::status)
        .is_some_and(|status| {
            status == reqwest::StatusCode::UNAUTHORIZED
                || status == reqwest::StatusCode::FORBIDDEN
                || status == reqwest::StatusCode::NOT_FOUND
        })
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let config = Config::from_env()?;
    let prefixes = load_asn_prefixes(config.asn_file.as_deref())?;
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()?;
    let mut spool = DurableSpool::open(config.spool_dir.clone()).await?;
    let initial = loop {
        match fetch_snapshot(&client, &config)
            .await
            .and_then(CompiledSnapshot::build)
        {
            Ok(snapshot) => break Arc::new(snapshot),
            Err(error) if terminal_snapshot_error(&error) => return Err(error),
            Err(error) => {
                tracing::warn!(%error, "event sensor initial snapshot unavailable; retrying");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    };
    let (snapshot_tx, snapshot_rx) = watch::channel(initial.clone());
    let (batch_tx, mut batch_rx) = mpsc::channel(BATCH_QUEUE);
    let interface = config.interface.clone();
    tokio::task::spawn_blocking(move || {
        if let Err(error) = capture_loop(interface, snapshot_rx, batch_tx) {
            tracing::error!(%error, "event sensor capture stopped");
        }
    });

    let mut snapshot = initial;
    let mut refresh = tokio::time::interval(Duration::from_secs(30));
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = refresh.tick() => {
                match fetch_snapshot(&client, &config).await.and_then(CompiledSnapshot::build) {
                    Ok(next) => {
                        snapshot = Arc::new(next);
                        let _ = snapshot_tx.send(snapshot.clone());
                    }
                    Err(error) if terminal_snapshot_error(&error) => return Err(error),
                    Err(error) => tracing::warn!(%error, "event sensor snapshot refresh failed"),
                }
                for batch in network_observations(&config, &snapshot, &prefixes).await {
                    enqueue_batch(&mut spool, batch).await?;
                }
                if let Err(error) = drain_spool(&client, &config.api, &config.token, &mut spool).await {
                    if matches!(&error, DrainError::Permanent(_)) {
                        return Err(error.into());
                    }
                    tracing::warn!(%error, "event sensor telemetry spool remains pending");
                }
            }
            Some(batch) = batch_rx.recv() => {
                enqueue_batch(&mut spool, batch).await?;
                if let Err(error) = drain_spool(&client, &config.api, &config.token, &mut spool).await {
                    if matches!(&error, DrainError::Permanent(_)) {
                        return Err(error.into());
                    }
                    tracing::warn!(%error, "event sensor aggregate spool remains pending");
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "rsctf_event_sensor_tests.rs"]
mod tests;
