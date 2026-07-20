//! services/traffic.rs — ported from RSCTF
//! `Services/Traffic/{TrafficRecorder,PcapFlowExtractor,TrafficWriter}.cs`.
//!
//! Traffic-capture record/replay for container challenges. The file-based
//! record/replay path is built on the **pure-Rust** `pcap-file` crate (no
//! libpcap dependency); the **live** path ([`capture_live`] / [`list_devices`])
//! is built on the libpcap-backed `pcap` crate, mirroring RSCTF's
//! `SharpPcap.LibPcap.LibPcapLiveDevice`.
//!
//! ## What this module does
//!
//! * [`TrafficRecorder`] — wraps a [`pcap_file::pcap::PcapWriter`] over a
//!   `std::fs::File` and records [`TrafficPacket`]s (a src/dst endpoint pair
//!   plus a payload and timestamp) into a `.pcap` file. Each packet is
//!   synthesised into an Ethernet / IPv4|IPv6 / TCP frame by hand — mirroring
//!   RSCTF's `TrafficRecorder.WritePcapPacket`, which builds an
//!   Ethernet/IPv6/UDP frame with `PacketDotNet`. We build **TCP** frames
//!   (rather than RSCTF's UDP) so the flow extractor can group by the TCP
//!   four-tuple, matching this file's task.
//! * [`write_capture`] — one-shot convenience: create a recorder, write every
//!   packet, flush.
//! * [`list_flows`] — the replay side (RSCTF `PcapFlowExtractor.ReadFlows`).
//!   Reads a `.pcap` back with [`pcap_file::pcap::PcapReader`], parses the
//!   Ethernet / IP / TCP headers out of the raw bytes with a small, defensive
//!   hand-rolled parser, and groups packets into [`Flow`]s keyed by the ordered
//!   `(src addr:port, dst addr:port)` pair.
//!
//! ## Live capture runtime requirements
//!
//! Live capture straight off a NIC ([`capture_live`]) needs libpcap and a real
//! interface. Opening a device in promiscuous mode requires the `CAP_NET_RAW`
//! capability (or root). The code below compiles and is correct against
//! libpcap (verified against the `pcap` v2 API), but it **cannot be exercised
//! in a sandbox** without `CAP_NET_RAW` and a live NIC — in that environment
//! `pcap::Capture::open` (or `Device::list`) returns a permission/no-device
//! error, which surfaces as an [`AppError`].
//!
//! RSCTF's on-disk pcaps are gzip-compressed; here we keep plain `.pcap` (no
//! compression dep is available, and the extractor reads the file directly).

use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufWriter;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pcap_file::pcap::{PcapPacket, PcapReader, PcapWriter};

use crate::utils::error::{AppError, AppResult};

/// Dummy source/destination MAC stamped on every synthesised Ethernet frame.
///
/// Mirrors RSCTF `TrafficRecorder.DummyMac` (`00-11-00-11-00-11`); the link
/// layer carries no real hosts, it only frames the synthetic IP/TCP packet.
const DUMMY_MAC: [u8; 6] = [0x00, 0x11, 0x00, 0x11, 0x00, 0x11];

/// EtherType for an IPv4 payload.
const ETHERTYPE_IPV4: u16 = 0x0800;
/// EtherType for an IPv6 payload.
const ETHERTYPE_IPV6: u16 = 0x86DD;
/// IP protocol number for TCP.
const IP_PROTO_TCP: u8 = 6;

/// Ethernet header length (dst MAC + src MAC + ethertype).
const ETH_HDR_LEN: usize = 14;
/// Fixed IPv4 header length (no options).
const IPV4_HDR_LEN: usize = 20;
/// Fixed IPv6 header length (no extension headers).
const IPV6_HDR_LEN: usize = 40;
/// Fixed TCP header length (no options).
const TCP_HDR_LEN: usize = 20;

/// Pcap default snap length (see `PcapHeader::default`); `write_packet` rejects
/// any frame longer than this, so we skip oversize synthesised frames.
const SNAP_LEN: usize = 65535;

/// A single captured traffic segment to be written into a pcap file.
///
/// Ported from RSCTF `TrafficPacket` (`Services/Traffic/TrafficPacket.cs`): a
/// source/destination endpoint, the raw payload bytes, and a capture timestamp.
/// The timestamp is stored as a duration since the Unix epoch to line up with
/// [`PcapPacket::timestamp`].
#[derive(Debug, Clone)]
pub struct TrafficPacket {
    /// Source endpoint (address + port).
    pub source: SocketAddr,
    /// Destination endpoint (address + port).
    pub dest: SocketAddr,
    /// Payload bytes carried in the synthesised TCP segment.
    pub data: Vec<u8>,
    /// Capture time, as a duration since the Unix epoch.
    pub timestamp: Duration,
}

impl TrafficPacket {
    /// Build a packet, defaulting the timestamp to "now".
    pub fn new(source: SocketAddr, dest: SocketAddr, data: Vec<u8>) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO);
        Self {
            source,
            dest,
            data,
            timestamp,
        }
    }
}

/// One reconstructed TCP flow, keyed by the ordered `(src, dst)` endpoint pair.
///
/// The RSCTF extractor produces a much richer `TrafficFlowSummary`
/// (direction, per-side byte/packet counts, flag hits, retained payload
/// chunks); the task here reduces a flow to the four fields the caller needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flow {
    /// Source endpoint rendered as `addr:port` (`[addr]:port` for IPv6).
    pub src: String,
    /// Destination endpoint rendered as `addr:port`.
    pub dst: String,
    /// Number of packets observed in this direction.
    pub packet_count: u64,
    /// Total TCP payload bytes observed in this direction.
    pub bytes: u64,
}

/// Records synthesised traffic frames into one `.pcap` file.
///
/// Ported from RSCTF `TrafficRecorder`: it owns a [`PcapWriter`] over a
/// buffered `std::fs::File` and appends one Ethernet/IP/TCP frame per
/// [`TrafficPacket`]. Unlike the C# original this is a plain synchronous
/// writer — the channel/ref-count/idle-timer archival lifecycle is a
/// concurrency concern handled a layer up, not part of the pcap format work.
pub struct TrafficRecorder {
    writer: PcapWriter<BufWriter<File>>,
    packet_count: u64,
}

impl TrafficRecorder {
    /// Create (or truncate) `path` and write the pcap global header.
    pub fn create(path: impl AsRef<Path>) -> AppResult<Self> {
        let file = File::create(path.as_ref()).map_err(|e| {
            AppError::internal(format!(
                "failed to create pcap file {}: {e}",
                path.as_ref().display()
            ))
        })?;
        let writer = PcapWriter::new(BufWriter::new(file))
            .map_err(|e| AppError::internal(format!("failed to write pcap header: {e}")))?;
        Ok(Self {
            writer,
            packet_count: 0,
        })
    }

    /// Synthesise an Ethernet/IP/TCP frame for `packet` and append it.
    ///
    /// Mirrors RSCTF `TrafficRecorder.WritePcapPacket`. Frames whose src/dst
    /// address families differ, or whose total length would exceed the pcap
    /// snap length, are silently skipped (best-effort, matching the C# recorder
    /// which drops packets rather than fail the capture).
    pub fn record(&mut self, packet: &TrafficPacket) -> AppResult<()> {
        let Some(frame) = build_frame(packet.source, packet.dest, &packet.data) else {
            return Ok(());
        };
        if frame.len() > SNAP_LEN {
            return Ok(());
        }
        let orig_len = frame.len() as u32;
        let pcap_packet = PcapPacket::new(packet.timestamp, orig_len, &frame);
        self.writer
            .write_packet(&pcap_packet)
            .map_err(|e| AppError::internal(format!("failed to write pcap packet: {e}")))?;
        self.packet_count += 1;
        Ok(())
    }

    /// Number of frames written so far.
    pub fn packet_count(&self) -> u64 {
        self.packet_count
    }

    /// Flush the buffered writer to disk, consuming the recorder.
    pub fn finish(self) -> AppResult<()> {
        use std::io::Write;
        let mut inner = self.writer.into_writer();
        inner
            .flush()
            .map_err(|e| AppError::internal(format!("failed to flush pcap file: {e}")))?;
        Ok(())
    }
}

/// One-shot capture: write every packet in `packets` to a fresh pcap at `path`.
///
/// Convenience wrapper over [`TrafficRecorder`] for callers that already have
/// the full packet set in memory (test fixtures, batch re-encode, …).
pub fn write_capture(path: impl AsRef<Path>, packets: &[TrafficPacket]) -> AppResult<()> {
    let mut recorder = TrafficRecorder::create(path)?;
    for packet in packets {
        recorder.record(packet)?;
    }
    recorder.finish()
}

/// Read a `.pcap` back and group its packets into TCP [`Flow`]s.
///
/// Ported from RSCTF `PcapFlowExtractor.ReadFlows`. Returns an empty vec (and
/// logs a warning) if the file cannot be opened or is not a valid pcap; per
/// packet, read or parse errors skip that packet rather than aborting — so a
/// truncated or partly-corrupt capture still yields the flows it can. Flows are
/// keyed by the ordered `(src, dst)` endpoint pair and returned sorted by that
/// key (deterministic ordering).
///
/// Signature is fixed by the task (`list_flows(path) -> Vec<Flow>`), so all
/// failure is swallowed into an empty/partial result rather than surfaced as a
/// `Result`.
pub fn list_flows(path: impl AsRef<Path>) -> Vec<Flow> {
    let path = path.as_ref();

    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "failed to open pcap file");
            return Vec::new();
        }
    };

    let mut reader = match PcapReader::new(file) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "not a valid pcap file");
            return Vec::new();
        }
    };

    // Keyed by (src, dst) so both directions of a connection are distinct
    // flows, matching the task's ordered-pair definition.
    let mut flows: BTreeMap<(String, String), Flow> = BTreeMap::new();

    while let Some(next) = reader.next_packet() {
        let pkt = match next {
            Ok(p) => p,
            // A read/parse error mid-stream: stop rather than spin, but keep
            // whatever we have already grouped.
            Err(e) => {
                tracing::debug!(error = %e, "stopping pcap read at packet error");
                break;
            }
        };

        let Some(parsed) = parse_frame(&pkt.data) else {
            continue;
        };

        let src = parsed.source.to_string();
        let dst = parsed.dest.to_string();
        let entry = flows
            .entry((src.clone(), dst.clone()))
            .or_insert_with(|| Flow {
                src,
                dst,
                packet_count: 0,
                bytes: 0,
            });
        entry.packet_count += 1;
        entry.bytes += parsed.payload_len as u64;
    }

    flows.into_values().collect()
}

/// Read-timeout applied to the live capture handle, in milliseconds.
///
/// libpcap's blocking read returns [`pcap::Error::TimeoutExpired`] after this
/// interval with no packet, which is how the loop below gets a chance to notice
/// the `stop` flag. Kept short so a stop request is honoured promptly.
const LIVE_READ_TIMEOUT_MS: i32 = 200;

/// Capture live traffic off a NIC into a `.pcap` file until `stop` is signalled.
///
/// Ported from RSCTF's `SharpPcap.LibPcap.LibPcapLiveDevice` capture loop. Opens
/// `device` in promiscuous + immediate mode, applies the optional `bpf_filter`
/// (tcpdump/BPF syntax, e.g. `"tcp port 80"`), and dumps every captured frame to
/// a libpcap savefile at `out_path`, returning the number of packets written.
///
/// The loop polls `stop` between reads: it sets a short read timeout so that
/// libpcap unblocks periodically ([`pcap::Error::TimeoutExpired`] is treated as
/// "keep going"), letting a caller on another thread flip the [`AtomicBool`] to
/// end the capture. Reaching the end of a bounded source (a savefile replay)
/// stops the loop via [`pcap::Error::NoMorePackets`]; any other libpcap error
/// aborts with an [`AppError`].
///
/// # Runtime requirements
///
/// This opens a raw device and needs the `CAP_NET_RAW` capability (or root) plus
/// a real interface. Without them, `open`/`filter` return a permission or
/// no-such-device error, mapped here to [`AppError::Internal`]. See the module
/// docs — this path is correct against libpcap but not exercisable in a sandbox.
///
/// This is a **blocking** call (libpcap reads are synchronous); run it on a
/// dedicated thread (`std::thread::spawn` / `tokio::task::spawn_blocking`) and
/// share `stop` with the controlling task.
pub fn capture_live(
    device: &str,
    bpf_filter: Option<&str>,
    out_path: &Path,
    stop: Arc<AtomicBool>,
) -> AppResult<u64> {
    capture_live_inner(device, bpf_filter, out_path, stop, None)
}

/// Capture entry point used by the durable owner. The startup channel is
/// completed only after the device, BPF program, and savefile are all open, so
/// a reconciliation acknowledgement never mistakes a spawned thread for an
/// operational capture.
pub(super) fn capture_live_with_startup(
    device: &str,
    bpf_filter: Option<&str>,
    out_path: &Path,
    stop: Arc<AtomicBool>,
    startup: tokio::sync::oneshot::Sender<Result<(), String>>,
) -> AppResult<u64> {
    capture_live_inner(device, bpf_filter, out_path, stop, Some(startup))
}

fn capture_live_inner(
    device: &str,
    bpf_filter: Option<&str>,
    out_path: &Path,
    stop: Arc<AtomicBool>,
    startup: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
) -> AppResult<u64> {
    // Build an inactive handle, configure it, then activate. `from_device`
    // accepts an `&str` device name via `From<&str> for pcap::Device`.
    let initialized = (|| -> AppResult<_> {
        let mut capture = pcap::Capture::from_device(device)
            .map_err(|e| pcap_err("open capture device", e))?
            .promisc(true)
            .immediate_mode(true)
            .timeout(LIVE_READ_TIMEOUT_MS)
            .open()
            .map_err(|e| pcap_err("activate capture device", e))?;

        // Apply the BPF filter (optimised) if the caller supplied one.
        if let Some(filter) = bpf_filter {
            capture
                .filter(filter, true)
                .map_err(|e| pcap_err("apply BPF filter", e))?;
        }

        // `savefile` returns an owned `pcap::Savefile` (no borrow of
        // `capture`), so it can coexist with mutable capture reads below.
        let savefile = capture
            .savefile(out_path)
            .map_err(|e| pcap_err("create capture savefile", e))?;
        Ok((capture, savefile))
    })();
    let (mut capture, mut savefile) = match initialized {
        Ok(handles) => {
            if let Some(startup) = startup {
                let _ = startup.send(Ok(()));
            }
            handles
        }
        Err(error) => {
            if let Some(startup) = startup {
                let _ = startup.send(Err(error.to_string()));
            }
            return Err(error);
        }
    };

    let mut packet_count: u64 = 0;
    while !stop.load(Ordering::Relaxed) {
        match capture.next_packet() {
            Ok(packet) => {
                savefile.write(&packet);
                packet_count += 1;
            }
            // Read timeout: no packet this interval — re-check `stop` and retry.
            Err(pcap::Error::TimeoutExpired) => continue,
            // Bounded source (e.g. a savefile) drained — done.
            Err(pcap::Error::NoMorePackets) => break,
            // Any other libpcap error aborts the capture.
            Err(e) => return Err(pcap_err("read packet", e)),
        }
    }

    // Flush buffered dump writes before returning the count.
    savefile
        .flush()
        .map_err(|e| pcap_err("flush capture savefile", e))?;

    Ok(packet_count)
}

/// List the names of capture-capable network devices known to libpcap.
///
/// Thin wrapper over `pcap::Device::list()` (RSCTF enumerates devices via
/// `LibPcapLiveDeviceList`). Returns just the interface names — the identifiers
/// [`capture_live`] accepts. Requires the same privileges as opening a device;
/// on a restricted host this may return an empty list or an [`AppError`].
pub fn list_devices() -> AppResult<Vec<String>> {
    let devices = pcap::Device::list().map_err(|e| pcap_err("list capture devices", e))?;
    Ok(devices.into_iter().map(|d| d.name).collect())
}

/// Map a libpcap error into an [`AppError::Internal`], tagged with `context`.
///
/// `pcap::Error` is not one of `AppError`'s `#[from]` sources, so live-capture
/// call sites funnel through here rather than using `?` directly.
fn pcap_err(context: &str, err: pcap::Error) -> AppError {
    AppError::internal(format!("libpcap {context}: {err}"))
}

/// Result of parsing one raw Ethernet frame down to its TCP four-tuple.
struct ParsedFrame {
    source: SocketAddr,
    dest: SocketAddr,
    payload_len: usize,
}

/// Parse an Ethernet / IPv4|IPv6 / TCP frame out of raw bytes.
///
/// Small and defensive: every layer bounds-checks before indexing and returns
/// `None` on anything it does not understand (non-IP ethertype, non-TCP
/// protocol, IPv6 extension headers, truncation). Mirrors the guarded parse in
/// RSCTF `PcapFlowExtractor.ReadFlows`.
fn parse_frame(data: &[u8]) -> Option<ParsedFrame> {
    if data.len() < ETH_HDR_LEN {
        return None;
    }
    let ethertype = u16::from_be_bytes([data[12], data[13]]);
    let l3 = &data[ETH_HDR_LEN..];

    match ethertype {
        ETHERTYPE_IPV4 => parse_ipv4(l3),
        ETHERTYPE_IPV6 => parse_ipv6(l3),
        _ => None,
    }
}

/// Parse an IPv4 packet (`l3` starts at the IP header) into its TCP tuple.
fn parse_ipv4(l3: &[u8]) -> Option<ParsedFrame> {
    if l3.len() < IPV4_HDR_LEN {
        return None;
    }
    // Version must be 4; IHL gives the header length in 32-bit words.
    if l3[0] >> 4 != 4 {
        return None;
    }
    let ihl = (l3[0] & 0x0f) as usize * 4;
    if ihl < IPV4_HDR_LEN || l3.len() < ihl {
        return None;
    }
    if l3[9] != IP_PROTO_TCP {
        return None;
    }
    let src_ip = IpAddr::from([l3[12], l3[13], l3[14], l3[15]]);
    let dst_ip = IpAddr::from([l3[16], l3[17], l3[18], l3[19]]);
    parse_tcp(&l3[ihl..], src_ip, dst_ip)
}

/// Parse an IPv6 packet (`l3` starts at the IP header) into its TCP tuple.
///
/// Only a bare IPv6 header with `next_header == TCP` is handled; extension
/// headers make the frame `None` (defensive — the synthesiser never emits
/// them).
fn parse_ipv6(l3: &[u8]) -> Option<ParsedFrame> {
    if l3.len() < IPV6_HDR_LEN {
        return None;
    }
    if l3[0] >> 4 != 6 {
        return None;
    }
    if l3[6] != IP_PROTO_TCP {
        return None;
    }
    let mut src = [0u8; 16];
    let mut dst = [0u8; 16];
    src.copy_from_slice(&l3[8..24]);
    dst.copy_from_slice(&l3[24..40]);
    let src_ip = IpAddr::from(src);
    let dst_ip = IpAddr::from(dst);
    parse_tcp(&l3[IPV6_HDR_LEN..], src_ip, dst_ip)
}

/// Parse a TCP segment (`l4` starts at the TCP header) into a [`ParsedFrame`].
fn parse_tcp(l4: &[u8], src_ip: IpAddr, dst_ip: IpAddr) -> Option<ParsedFrame> {
    if l4.len() < TCP_HDR_LEN {
        return None;
    }
    let src_port = u16::from_be_bytes([l4[0], l4[1]]);
    let dst_port = u16::from_be_bytes([l4[2], l4[3]]);
    // Data-offset field: high nibble of byte 12, in 32-bit words.
    let data_offset = (l4[12] >> 4) as usize * 4;
    if data_offset < TCP_HDR_LEN || l4.len() < data_offset {
        return None;
    }
    let payload_len = l4.len() - data_offset;
    Some(ParsedFrame {
        source: SocketAddr::new(src_ip, src_port),
        dest: SocketAddr::new(dst_ip, dst_port),
        payload_len,
    })
}

/// Build a full Ethernet/IP/TCP frame carrying `payload`.
///
/// Returns `None` when the src/dst address families differ (a mixed v4/v6
/// endpoint pair has no single valid frame encoding).
fn build_frame(source: SocketAddr, dest: SocketAddr, payload: &[u8]) -> Option<Vec<u8>> {
    match (source.ip(), dest.ip()) {
        (IpAddr::V4(s), IpAddr::V4(d)) => Some(build_v4_frame(
            s.octets(),
            source.port(),
            d.octets(),
            dest.port(),
            payload,
        )),
        (IpAddr::V6(s), IpAddr::V6(d)) => Some(build_v6_frame(
            s.octets(),
            source.port(),
            d.octets(),
            dest.port(),
            payload,
        )),
        _ => None,
    }
}

/// Ethernet header framing an IP payload of the given ethertype.
fn eth_header(ethertype: u16) -> Vec<u8> {
    let mut hdr = Vec::with_capacity(ETH_HDR_LEN);
    hdr.extend_from_slice(&DUMMY_MAC);
    hdr.extend_from_slice(&DUMMY_MAC);
    hdr.extend_from_slice(&ethertype.to_be_bytes());
    hdr
}

/// Assemble Ethernet + IPv4 + TCP + payload.
fn build_v4_frame(
    src_ip: [u8; 4],
    src_port: u16,
    dst_ip: [u8; 4],
    dst_port: u16,
    payload: &[u8],
) -> Vec<u8> {
    let tcp = tcp_segment(src_port, dst_port, payload);
    let total_len = IPV4_HDR_LEN + tcp.len();

    let mut ip = Vec::with_capacity(IPV4_HDR_LEN);
    ip.push(0x45); // version 4, IHL 5 words
    ip.push(0x00); // DSCP / ECN
    ip.extend_from_slice(&(total_len.min(u16::MAX as usize) as u16).to_be_bytes());
    ip.extend_from_slice(&0u16.to_be_bytes()); // identification
    ip.extend_from_slice(&0u16.to_be_bytes()); // flags + fragment offset
    ip.push(64); // TTL
    ip.push(IP_PROTO_TCP);
    ip.extend_from_slice(&0u16.to_be_bytes()); // checksum placeholder
    ip.extend_from_slice(&src_ip);
    ip.extend_from_slice(&dst_ip);
    let checksum = ones_complement_checksum(&ip);
    ip[10..12].copy_from_slice(&checksum.to_be_bytes());

    let mut frame = eth_header(ETHERTYPE_IPV4);
    frame.extend_from_slice(&ip);
    frame.extend_from_slice(&tcp);
    frame
}

/// Assemble Ethernet + IPv6 + TCP + payload.
fn build_v6_frame(
    src_ip: [u8; 16],
    src_port: u16,
    dst_ip: [u8; 16],
    dst_port: u16,
    payload: &[u8],
) -> Vec<u8> {
    let tcp = tcp_segment(src_port, dst_port, payload);

    let mut ip = Vec::with_capacity(IPV6_HDR_LEN);
    ip.extend_from_slice(&0x6000_0000u32.to_be_bytes()); // version 6, TC/flow 0
    ip.extend_from_slice(&(tcp.len().min(u16::MAX as usize) as u16).to_be_bytes()); // payload length
    ip.push(IP_PROTO_TCP); // next header
    ip.push(64); // hop limit
    ip.extend_from_slice(&src_ip);
    ip.extend_from_slice(&dst_ip);

    let mut frame = eth_header(ETHERTYPE_IPV6);
    frame.extend_from_slice(&ip);
    frame.extend_from_slice(&tcp);
    frame
}

/// Build a minimal TCP segment (20-byte header, PSH|ACK, checksum 0) + payload.
///
/// The checksum is left zero: our own extractor never validates it, and a real
/// analyser treats a zero TCP checksum as "not computed". Keeping it zero
/// avoids the IPv4/IPv6 pseudo-header dance while staying round-trip-correct.
fn tcp_segment(src_port: u16, dst_port: u16, payload: &[u8]) -> Vec<u8> {
    let mut tcp = Vec::with_capacity(TCP_HDR_LEN + payload.len());
    tcp.extend_from_slice(&src_port.to_be_bytes());
    tcp.extend_from_slice(&dst_port.to_be_bytes());
    tcp.extend_from_slice(&0u32.to_be_bytes()); // sequence number
    tcp.extend_from_slice(&0u32.to_be_bytes()); // acknowledgement number
    tcp.push(0x50); // data offset 5 words, reserved 0
    tcp.push(0x18); // flags: PSH | ACK
    tcp.extend_from_slice(&0xFFFFu16.to_be_bytes()); // window size
    tcp.extend_from_slice(&0u16.to_be_bytes()); // checksum (not computed)
    tcp.extend_from_slice(&0u16.to_be_bytes()); // urgent pointer
    tcp.extend_from_slice(payload);
    tcp
}

/// Standard internet 16-bit ones-complement checksum over `bytes`.
fn ones_complement_checksum(bytes: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut chunks = bytes.chunks_exact(2);
    for pair in &mut chunks {
        sum += u16::from_be_bytes([pair[0], pair[1]]) as u32;
    }
    if let [last] = chunks.remainder() {
        sum += (*last as u32) << 8;
    }
    while (sum >> 16) != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

// ─── Per-container live-capture registry ────────────────────────────────────────
//
// Live-capture ownership and DB desired-state reconciliation are split out so
// this format/parser module stays focused and below the repository file-size cap.

mod capture;

pub(crate) use capture::destroy_container_after_capture_fence;
pub use capture::{
    fence_unowned_capture_owner, start_capture_reconciler, start_container_capture,
    stop_container_capture,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

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
        // Two ordered (src,dst) pairs: a->b and b->a.
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
    fn parse_rejects_short_and_non_ip() {
        assert!(parse_frame(&[]).is_none());
        assert!(parse_frame(&[0u8; 10]).is_none());
        // Valid-length Ethernet frame but ARP ethertype (0x0806) -> None.
        let mut arp = vec![0u8; ETH_HDR_LEN + 4];
        arp[12] = 0x08;
        arp[13] = 0x06;
        assert!(parse_frame(&arp).is_none());
    }
}
