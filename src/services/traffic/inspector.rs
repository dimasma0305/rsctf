//! Bounded, versioned PCAP inspection for the monitor flow viewer.
//!
//! A capture is parsed once per immutable filesystem identity and the resulting
//! index is shared by list and detail reads. Filter changes therefore scan a
//! bounded in-memory index instead of repeatedly scheduling 256 MiB parses.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{File, Metadata};
use std::io::Read;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::{Duration, Instant};

use pcap_file::pcap::PcapReader;
use sha2::{Digest, Sha256};

use super::parse_frame;
use crate::utils::error::AppError;
use crate::utils::single_flight::SingleFlight;

pub(crate) const MAX_INSPECT_CAPTURE_BYTES: u64 = 256 * 1024 * 1024;
pub(crate) const MAX_CAPTURE_FLOWS: usize = 20_000;
pub(crate) const DEFAULT_FLOW_PAGE_SIZE: u16 = 50;
pub(crate) const MAX_FLOW_PAGE_SIZE: u16 = 200;
const MAX_REGEX_PATTERN_BYTES: usize = 256;

const MAX_FLOW_PAYLOAD_BYTES: usize = 256 * 1024;
const MAX_SNAPSHOT_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
const MAX_FLOW_CHUNKS: usize = 1_024;
const MAX_SNAPSHOT_CHUNKS: usize = 50_000;
const MAX_FLAG_OFFSETS_PER_CHUNK: usize = 256;
const MAX_FLAG_HITS_PER_FLOW: u32 = 65_535;
const MAX_SNAPSHOT_WEIGHT_BYTES: usize = 16 * 1024 * 1024;
const MAX_CACHE_WEIGHT_BYTES: usize = 64 * 1024 * 1024;
const MAX_CACHE_ENTRIES: usize = 8;
const CACHE_TTL: Duration = Duration::from_secs(120);
const PARSE_WORK_BUDGET: Duration = Duration::from_secs(60);
const PARSE_FLIGHT_TIMEOUT: Duration = Duration::from_secs(65);
const INSPECTION_SLOTS: usize = 2;
const INSPECTION_WEIGHT_UNITS: usize = 256;
const BYTES_PER_WEIGHT_UNIT: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FlowDirection {
    ContainerToTeam,
    TeamToContainer,
}

#[derive(Clone, Debug)]
pub(crate) struct IndexedPayloadChunk {
    pub(crate) direction: FlowDirection,
    pub(crate) timestamp_utc: i64,
    pub(crate) payload: Vec<u8>,
    pub(crate) flag_offsets: Vec<u32>,
}

#[derive(Clone, Debug)]
pub(crate) struct IndexedFlow {
    pub(crate) flow_id: String,
    pub(crate) connection_port: u16,
    pub(crate) first_seen_utc: i64,
    pub(crate) last_seen_utc: i64,
    pub(crate) peer_ip: String,
    pub(crate) packets_in: u64,
    pub(crate) packets_out: u64,
    pub(crate) bytes_in: u64,
    pub(crate) bytes_out: u64,
    pub(crate) flag_hits: u32,
    pub(crate) payload_truncated: bool,
    pub(crate) chunks: Vec<IndexedPayloadChunk>,
}

#[derive(Debug)]
pub(crate) struct FlowSnapshot {
    identity: FileIdentity,
    version: String,
    flows: Vec<IndexedFlow>,
    indexed_payload_bytes: usize,
    payload_truncated: bool,
    cache_weight: usize,
}

impl FlowSnapshot {
    pub(crate) fn version(&self) -> &str {
        &self.version
    }

    pub(crate) fn flows(&self) -> &[IndexedFlow] {
        &self.flows
    }

    pub(crate) fn indexed_payload_bytes(&self) -> usize {
        self.indexed_payload_bytes
    }

    pub(crate) fn payload_truncated(&self) -> bool {
        self.payload_truncated
    }

    pub(crate) fn flow(
        &self,
        connection_port: u16,
        flow_id: Option<&str>,
    ) -> Result<Option<&IndexedFlow>, InspectionError> {
        if let Some(flow_id) = flow_id {
            return Ok(self
                .flows
                .iter()
                .find(|flow| flow.connection_port == connection_port && flow.flow_id == flow_id));
        }
        let mut matches = self
            .flows
            .iter()
            .filter(|flow| flow.connection_port == connection_port);
        let first = matches.next();
        if first.is_some() && matches.next().is_some() {
            return Err(InspectionError::AmbiguousFlow);
        }
        Ok(first)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InspectionError {
    NotFound,
    Invalid(String),
    TooLarge,
    TooManyFlows,
    SnapshotTooLarge,
    Busy,
    Changed,
    StaleSnapshot,
    AmbiguousFlow,
    AmbiguousEndpoints,
    Internal(String),
}

impl From<InspectionError> for AppError {
    fn from(error: InspectionError) -> Self {
        match error {
            InspectionError::NotFound => AppError::not_found("Capture not found"),
            InspectionError::Invalid(message) => AppError::bad_request(message),
            InspectionError::TooLarge => AppError::payload_too_large(
                "Capture is larger than the 256 MiB inspection limit; download it instead",
            ),
            InspectionError::TooManyFlows => {
                AppError::bad_request("Capture contains more than 20000 distinct flows")
            }
            InspectionError::SnapshotTooLarge => AppError::payload_too_large(
                "Capture flow index exceeds the bounded inspection memory budget",
            ),
            InspectionError::Busy => {
                AppError::overloaded("Capture inspection capacity is busy; retry shortly", 1)
            }
            InspectionError::Changed => AppError::overloaded(
                "Capture changed while it was being inspected; retry the current version",
                1,
            ),
            InspectionError::StaleSnapshot => AppError::conflict(
                "The requested capture snapshot is no longer available; refresh the flow list",
            ),
            InspectionError::AmbiguousFlow => AppError::conflict(
                "Multiple flows share this connectionPort; select one with its flowId",
            ),
            InspectionError::AmbiguousEndpoints => AppError::bad_request(
                "Capture flow does not contain exactly one configured challenge service endpoint",
            ),
            InspectionError::Internal(message) => AppError::internal(message),
        }
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct FileIdentity {
    path: PathBuf,
    len: u64,
    modified_nanos: i128,
    device: u64,
    inode: u64,
    container_port: u16,
}

impl FileIdentity {
    fn from_metadata(
        path: PathBuf,
        metadata: &Metadata,
        container_port: u16,
    ) -> Result<Self, InspectionError> {
        if !metadata.is_file() {
            return Err(InspectionError::NotFound);
        }
        if metadata.len() > MAX_INSPECT_CAPTURE_BYTES {
            return Err(InspectionError::TooLarge);
        }
        let (modified_nanos, device, inode) = metadata_identity(metadata)?;
        Ok(Self {
            path,
            len: metadata.len(),
            modified_nanos,
            device,
            inode,
            container_port,
        })
    }

    fn version(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(self.path.to_string_lossy().as_bytes());
        digest.update(self.len.to_le_bytes());
        digest.update(self.modified_nanos.to_le_bytes());
        digest.update(self.device.to_le_bytes());
        digest.update(self.inode.to_le_bytes());
        digest.update(self.container_port.to_le_bytes());
        hex::encode(&digest.finalize()[..16])
    }
}

#[cfg(unix)]
fn metadata_identity(metadata: &Metadata) -> Result<(i128, u64, u64), InspectionError> {
    use std::os::unix::fs::MetadataExt;

    let seconds = i128::from(metadata.mtime());
    let nanos = i128::from(metadata.mtime_nsec());
    Ok((
        seconds.saturating_mul(1_000_000_000).saturating_add(nanos),
        metadata.dev(),
        metadata.ino(),
    ))
}

#[cfg(not(unix))]
fn metadata_identity(metadata: &Metadata) -> Result<(i128, u64, u64), InspectionError> {
    let modified = metadata
        .modified()
        .map_err(|error| InspectionError::Internal(format!("capture mtime: {error}")))?;
    let nanos = modified
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| InspectionError::Internal("capture mtime predates Unix epoch".into()))?
        .as_nanos();
    let nanos = i128::try_from(nanos)
        .map_err(|_| InspectionError::Internal("capture mtime is out of range".into()))?;
    Ok((nanos, 0, 0))
}

struct CacheEntry {
    snapshot: Arc<FlowSnapshot>,
    inserted_at: Instant,
}

#[derive(Default)]
struct SnapshotCache {
    entries: HashMap<FileIdentity, CacheEntry>,
    total_weight: usize,
}

static SNAPSHOT_CACHE: LazyLock<RwLock<SnapshotCache>> = LazyLock::new(Default::default);
static SNAPSHOT_FLIGHT: LazyLock<SingleFlight<BuildOutcome>> = LazyLock::new(SingleFlight::new);
static ACTIVE_PARSES: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);
static PARSE_SLOTS: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(INSPECTION_SLOTS)));
static PARSE_WEIGHT: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(INSPECTION_WEIGHT_UNITS)));

#[cfg(test)]
static PARSE_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[derive(Clone, Debug)]
enum BuildOutcome {
    Ready(Arc<FlowSnapshot>),
    Failed(InspectionError),
}

impl Default for BuildOutcome {
    fn default() -> Self {
        Self::Failed(InspectionError::Internal(
            "capture inspection did not complete before its deadline".into(),
        ))
    }
}

struct ParsePermit {
    _slot: tokio::sync::OwnedSemaphorePermit,
    _weight: tokio::sync::OwnedSemaphorePermit,
}

struct ActiveParse {
    version: String,
}

impl ActiveParse {
    fn try_begin(version: &str) -> Option<Self> {
        let mut active = ACTIVE_PARSES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        active.insert(version.to_owned()).then(|| Self {
            version: version.to_owned(),
        })
    }
}

impl Drop for ActiveParse {
    fn drop(&mut self) {
        ACTIVE_PARSES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.version);
    }
}

fn try_parse_permit(file_bytes: u64) -> Result<ParsePermit, InspectionError> {
    let units = file_bytes.max(1).div_ceil(BYTES_PER_WEIGHT_UNIT);
    let units = u32::try_from(units).map_err(|_| InspectionError::TooLarge)?;
    let slot = Arc::clone(&PARSE_SLOTS)
        .try_acquire_owned()
        .map_err(|_| InspectionError::Busy)?;
    let weight = Arc::clone(&PARSE_WEIGHT)
        .try_acquire_many_owned(units)
        .map_err(|_| InspectionError::Busy)?;
    Ok(ParsePermit {
        _slot: slot,
        _weight: weight,
    })
}

async fn current_identity(
    path: &Path,
    container_port: u16,
) -> Result<FileIdentity, InspectionError> {
    let metadata = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => InspectionError::NotFound,
            _ => InspectionError::Internal(format!("capture metadata: {error}")),
        })?;
    if metadata.file_type().is_symlink() {
        return Err(InspectionError::NotFound);
    }
    FileIdentity::from_metadata(path.to_path_buf(), &metadata, container_port)
}

fn current_identity_sync(
    path: &Path,
    container_port: u16,
) -> Result<FileIdentity, InspectionError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => InspectionError::NotFound,
        _ => InspectionError::Internal(format!("capture metadata: {error}")),
    })?;
    if metadata.file_type().is_symlink() {
        return Err(InspectionError::NotFound);
    }
    FileIdentity::from_metadata(path.to_path_buf(), &metadata, container_port)
}

fn cached_identity(identity: &FileIdentity) -> Option<Arc<FlowSnapshot>> {
    let cache = SNAPSHOT_CACHE
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.entries.get(identity).and_then(|entry| {
        (entry.inserted_at.elapsed() <= CACHE_TTL).then(|| Arc::clone(&entry.snapshot))
    })
}

fn cached_version(path: &Path, version: &str) -> Option<Arc<FlowSnapshot>> {
    let cache = SNAPSHOT_CACHE
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.entries.values().find_map(|entry| {
        (entry.inserted_at.elapsed() <= CACHE_TTL
            && entry.snapshot.identity.path == path
            && entry.snapshot.version == version)
            .then(|| Arc::clone(&entry.snapshot))
    })
}

fn insert_snapshot(snapshot: Arc<FlowSnapshot>) {
    let mut cache = SNAPSHOT_CACHE
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let expired = cache
        .entries
        .iter()
        .filter_map(|(identity, entry)| {
            (entry.inserted_at.elapsed() > CACHE_TTL).then(|| identity.clone())
        })
        .collect::<Vec<_>>();
    for identity in expired {
        if let Some(entry) = cache.entries.remove(&identity) {
            cache.total_weight = cache
                .total_weight
                .saturating_sub(entry.snapshot.cache_weight);
        }
    }
    if let Some(previous) = cache.entries.remove(&snapshot.identity) {
        cache.total_weight = cache
            .total_weight
            .saturating_sub(previous.snapshot.cache_weight);
    }
    while cache.entries.len() >= MAX_CACHE_ENTRIES
        || cache.total_weight.saturating_add(snapshot.cache_weight) > MAX_CACHE_WEIGHT_BYTES
    {
        let Some(oldest) = cache
            .entries
            .iter()
            .min_by_key(|(_, entry)| entry.inserted_at)
            .map(|(identity, _)| identity.clone())
        else {
            break;
        };
        if let Some(entry) = cache.entries.remove(&oldest) {
            cache.total_weight = cache
                .total_weight
                .saturating_sub(entry.snapshot.cache_weight);
        }
    }
    cache.total_weight = cache.total_weight.saturating_add(snapshot.cache_weight);
    cache.entries.insert(
        snapshot.identity.clone(),
        CacheEntry {
            snapshot,
            inserted_at: Instant::now(),
        },
    );
}

pub(crate) fn invalidate_inspection_path(path: &Path) {
    invalidate_matching_paths(|candidate| candidate == path);
}

pub(crate) fn invalidate_inspection_directory(path: &Path) {
    invalidate_matching_paths(|candidate| candidate.starts_with(path));
}

fn invalidate_matching_paths(mut matches: impl FnMut(&Path) -> bool) {
    let mut cache = SNAPSHOT_CACHE
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let keys = cache
        .entries
        .keys()
        .filter(|identity| matches(&identity.path))
        .cloned()
        .collect::<Vec<_>>();
    for key in keys {
        if let Some(entry) = cache.entries.remove(&key) {
            cache.total_weight = cache
                .total_weight
                .saturating_sub(entry.snapshot.cache_weight);
        }
    }
}

pub(crate) async fn load_flow_snapshot(
    path: &Path,
    container_port: u16,
    requested_version: Option<&str>,
) -> Result<Arc<FlowSnapshot>, InspectionError> {
    if let Some(version) = requested_version {
        validate_snapshot_version(version)?;
        if let Some(snapshot) = cached_version(path, version) {
            return Ok(snapshot);
        }
    }

    let identity = current_identity(path, container_port).await?;
    let version = identity.version();
    if requested_version.is_some_and(|requested| requested != version) {
        return Err(InspectionError::StaleSnapshot);
    }
    if let Some(snapshot) = cached_identity(&identity) {
        return Ok(snapshot);
    }

    let flight_key = format!("traffic-flow:{version}");
    let active_key = flight_key.clone();
    let identity_for_build = identity.clone();
    let outcome = SNAPSHOT_FLIGHT
        .run_with_timeout(&flight_key, PARSE_FLIGHT_TIMEOUT, move || async move {
            if let Some(snapshot) = cached_identity(&identity_for_build) {
                return BuildOutcome::Ready(snapshot);
            }
            let Some(active_parse) = ActiveParse::try_begin(&active_key) else {
                return BuildOutcome::Failed(InspectionError::Busy);
            };
            let permit = match try_parse_permit(identity_for_build.len) {
                Ok(permit) => permit,
                Err(error) => return BuildOutcome::Failed(error),
            };
            let parse_deadline = Instant::now() + PARSE_WORK_BUDGET;
            let parsed = tokio::task::spawn_blocking(move || {
                let _active_parse = active_parse;
                let _permit = permit;
                match build_snapshot(identity_for_build, parse_deadline) {
                    Ok(snapshot) => {
                        let snapshot = Arc::new(snapshot);
                        insert_snapshot(Arc::clone(&snapshot));
                        BuildOutcome::Ready(snapshot)
                    }
                    Err(error) => BuildOutcome::Failed(error),
                }
            })
            .await;
            match parsed {
                Ok(outcome) => outcome,
                Err(error) => BuildOutcome::Failed(InspectionError::Internal(format!(
                    "capture inspection task failed: {error}"
                ))),
            }
        })
        .await;
    match outcome {
        BuildOutcome::Ready(snapshot) => Ok(snapshot),
        BuildOutcome::Failed(error) => Err(error),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct FlowKey(SocketAddr, SocketAddr);

impl FlowKey {
    fn new(source: SocketAddr, destination: SocketAddr) -> Self {
        if source <= destination {
            Self(source, destination)
        } else {
            Self(destination, source)
        }
    }

    fn id(&self) -> String {
        fn append_socket(bytes: &mut Vec<u8>, socket: SocketAddr) {
            match socket.ip() {
                std::net::IpAddr::V4(ip) => {
                    bytes.push(4);
                    bytes.extend_from_slice(&ip.octets());
                }
                std::net::IpAddr::V6(ip) => {
                    bytes.push(6);
                    bytes.extend_from_slice(&ip.octets());
                }
            }
            bytes.extend_from_slice(&socket.port().to_be_bytes());
        }

        let mut bytes = Vec::with_capacity(38);
        append_socket(&mut bytes, self.0);
        append_socket(&mut bytes, self.1);
        hex::encode(bytes)
    }
}

struct FlowBuilder {
    flow_id: String,
    peer: SocketAddr,
    container: SocketAddr,
    first_seen_utc: i64,
    last_seen_utc: i64,
    packets_in: u64,
    packets_out: u64,
    bytes_in: u64,
    bytes_out: u64,
    flag_hits: u32,
    payload_bytes: usize,
    payload_truncated: bool,
    chunks: Vec<IndexedPayloadChunk>,
}

impl FlowBuilder {
    fn new(
        source: SocketAddr,
        destination: SocketAddr,
        timestamp_utc: i64,
        container_port: u16,
    ) -> Result<Self, InspectionError> {
        // The challenge's configured service port is the only authoritative
        // orientation available in a raw PCAP. Port magnitude and first-packet
        // direction are both unsafe for high service ports and response-first
        // captures.
        let (peer, container) = match (
            source.port() == container_port,
            destination.port() == container_port,
        ) {
            (true, false) => (destination, source),
            (false, true) => (source, destination),
            _ => return Err(InspectionError::AmbiguousEndpoints),
        };
        Ok(Self {
            flow_id: FlowKey::new(source, destination).id(),
            peer,
            container,
            first_seen_utc: timestamp_utc,
            last_seen_utc: timestamp_utc,
            packets_in: 0,
            packets_out: 0,
            bytes_in: 0,
            bytes_out: 0,
            flag_hits: 0,
            payload_bytes: 0,
            payload_truncated: false,
            chunks: Vec::new(),
        })
    }

    fn observe(
        &mut self,
        source: SocketAddr,
        destination: SocketAddr,
        timestamp_utc: i64,
        payload: &[u8],
        snapshot_payload_bytes: &mut usize,
        snapshot_chunks: &mut usize,
    ) {
        self.first_seen_utc = self.first_seen_utc.min(timestamp_utc);
        self.last_seen_utc = self.last_seen_utc.max(timestamp_utc);
        let direction = if source == self.peer && destination == self.container {
            self.packets_out = self.packets_out.saturating_add(1);
            self.bytes_out = self
                .bytes_out
                .saturating_add(u64::try_from(payload.len()).unwrap_or(u64::MAX));
            FlowDirection::TeamToContainer
        } else {
            self.packets_in = self.packets_in.saturating_add(1);
            self.bytes_in = self
                .bytes_in
                .saturating_add(u64::try_from(payload.len()).unwrap_or(u64::MAX));
            FlowDirection::ContainerToTeam
        };

        let (flag_hits, flag_offsets) = find_flag_markers(payload);
        self.flag_hits = self
            .flag_hits
            .saturating_add(flag_hits)
            .min(MAX_FLAG_HITS_PER_FLOW);
        if payload.is_empty() {
            return;
        }

        let flow_remaining = MAX_FLOW_PAYLOAD_BYTES.saturating_sub(self.payload_bytes);
        let snapshot_remaining = MAX_SNAPSHOT_PAYLOAD_BYTES.saturating_sub(*snapshot_payload_bytes);
        let retain = payload.len().min(flow_remaining).min(snapshot_remaining);
        if retain < payload.len()
            || self.chunks.len() >= MAX_FLOW_CHUNKS
            || *snapshot_chunks >= MAX_SNAPSHOT_CHUNKS
        {
            self.payload_truncated = true;
        }
        if retain == 0
            || self.chunks.len() >= MAX_FLOW_CHUNKS
            || *snapshot_chunks >= MAX_SNAPSHOT_CHUNKS
        {
            return;
        }

        let retained_offsets = flag_offsets
            .into_iter()
            .filter(|offset| usize::try_from(*offset).is_ok_and(|offset| offset < retain))
            .take(MAX_FLAG_OFFSETS_PER_CHUNK)
            .collect();
        self.chunks.push(IndexedPayloadChunk {
            direction,
            timestamp_utc,
            payload: payload[..retain].to_vec(),
            flag_offsets: retained_offsets,
        });
        self.payload_bytes = self.payload_bytes.saturating_add(retain);
        *snapshot_payload_bytes = snapshot_payload_bytes.saturating_add(retain);
        *snapshot_chunks = snapshot_chunks.saturating_add(1);
    }

    fn finish(self) -> IndexedFlow {
        IndexedFlow {
            flow_id: self.flow_id,
            connection_port: self.peer.port(),
            first_seen_utc: self.first_seen_utc,
            last_seen_utc: self.last_seen_utc,
            peer_ip: self.peer.ip().to_string(),
            packets_in: self.packets_in,
            packets_out: self.packets_out,
            bytes_in: self.bytes_in,
            bytes_out: self.bytes_out,
            flag_hits: self.flag_hits,
            payload_truncated: self.payload_truncated,
            chunks: self.chunks,
        }
    }
}

fn packet_timestamp_millis(timestamp: Duration) -> i64 {
    i64::try_from(timestamp.as_millis()).unwrap_or(i64::MAX)
}

fn find_flag_markers(payload: &[u8]) -> (u32, Vec<u32>) {
    const PREFIX: &[u8] = b"flag{";
    const MAX_FLAG_BYTES: usize = 256;

    let mut cursor = 0usize;
    let mut closing_brace = 0usize;
    let mut hits = 0u32;
    let mut offsets = Vec::new();
    while cursor.saturating_add(PREFIX.len()) <= payload.len() && hits < MAX_FLAG_HITS_PER_FLOW {
        let Some(relative) = payload[cursor..]
            .windows(PREFIX.len())
            .position(|candidate| candidate == PREFIX)
        else {
            break;
        };
        let start = cursor.saturating_add(relative);
        let body_start = start.saturating_add(PREFIX.len());
        let end = body_start.saturating_add(MAX_FLAG_BYTES).min(payload.len());
        // Keep the closing-brace cursor monotonic. Searching the next 256-byte
        // window from scratch for every `flag{` prefix makes a hostile payload
        // perform up to 256 scans per byte even though the capture-size bound
        // is otherwise linear.
        closing_brace = closing_brace.max(body_start);
        while closing_brace < end && payload[closing_brace] != b'}' {
            closing_brace += 1;
        }
        if closing_brace < end {
            hits = hits.saturating_add(1);
            if offsets.len() < MAX_FLAG_OFFSETS_PER_CHUNK {
                offsets.push(u32::try_from(start).unwrap_or(u32::MAX));
            }
        }
        cursor = body_start;
    }
    (hits, offsets)
}

fn build_snapshot(
    expected: FileIdentity,
    deadline: Instant,
) -> Result<FlowSnapshot, InspectionError> {
    #[cfg(test)]
    PARSE_COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

    if current_identity_sync(&expected.path, expected.container_port)? != expected {
        return Err(InspectionError::Changed);
    }
    let file = File::open(&expected.path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => InspectionError::NotFound,
        _ => InspectionError::Internal(format!("capture open: {error}")),
    })?;
    let opened_identity = FileIdentity::from_metadata(
        expected.path.clone(),
        &file
            .metadata()
            .map_err(|error| InspectionError::Internal(format!("capture metadata: {error}")))?,
        expected.container_port,
    )?;
    if opened_identity != expected {
        return Err(InspectionError::Changed);
    }

    let file = file.take(expected.len.saturating_add(1));
    let mut reader = PcapReader::new(file)
        .map_err(|_| InspectionError::Invalid("Capture is not a valid pcap file".into()))?;
    let mut flows = BTreeMap::<FlowKey, FlowBuilder>::new();
    let mut snapshot_payload_bytes = 0usize;
    let mut snapshot_chunks = 0usize;

    while let Some(next) = reader.next_packet() {
        if Instant::now() >= deadline {
            return Err(InspectionError::Busy);
        }
        let packet = match next {
            Ok(packet) => packet,
            Err(error) => {
                tracing::debug!(%error, "stopping flow inspection at malformed pcap packet");
                break;
            }
        };
        let Some(parsed) = parse_frame(&packet.data) else {
            continue;
        };
        let key = FlowKey::new(parsed.source, parsed.dest);
        if !flows.contains_key(&key) && flows.len() >= MAX_CAPTURE_FLOWS {
            return Err(InspectionError::TooManyFlows);
        }
        let timestamp_utc = packet_timestamp_millis(packet.timestamp);
        if !flows.contains_key(&key) {
            flows.insert(
                key.clone(),
                FlowBuilder::new(
                    parsed.source,
                    parsed.dest,
                    timestamp_utc,
                    expected.container_port,
                )?,
            );
        }
        flows
            .get_mut(&key)
            .expect("flow was inserted above")
            .observe(
                parsed.source,
                parsed.dest,
                timestamp_utc,
                parsed.payload,
                &mut snapshot_payload_bytes,
                &mut snapshot_chunks,
            );
    }

    let take = reader.into_reader();
    let file = take.into_inner();
    let after_open = FileIdentity::from_metadata(
        expected.path.clone(),
        &file
            .metadata()
            .map_err(|error| InspectionError::Internal(format!("capture metadata: {error}")))?,
        expected.container_port,
    )?;
    let after_path = current_identity_sync(&expected.path, expected.container_port)?;
    if after_open != expected || after_path != expected {
        return Err(InspectionError::Changed);
    }

    let mut flows = flows
        .into_values()
        .map(FlowBuilder::finish)
        .collect::<Vec<_>>();
    flows.sort_by_key(|flow| (flow.first_seen_utc, flow.connection_port));
    let payload_truncated = flows.iter().any(|flow| flow.payload_truncated);
    // Charge allocated capacities, not logical lengths, so Vec/String growth
    // slack cannot let the nominal cache bound undercount retained heap.
    let cache_weight = flows.iter().fold(
        std::mem::size_of::<FlowSnapshot>().saturating_add(
            flows
                .capacity()
                .saturating_mul(std::mem::size_of::<IndexedFlow>()),
        ),
        |total, flow| {
            flow.chunks.iter().fold(
                total
                    .saturating_add(flow.flow_id.capacity())
                    .saturating_add(flow.peer_ip.capacity())
                    .saturating_add(
                        flow.chunks
                            .capacity()
                            .saturating_mul(std::mem::size_of::<IndexedPayloadChunk>()),
                    ),
                |total, chunk| {
                    total
                        .saturating_add(chunk.payload.capacity())
                        .saturating_add(
                            chunk
                                .flag_offsets
                                .capacity()
                                .saturating_mul(std::mem::size_of::<u32>()),
                        )
                },
            )
        },
    );
    if cache_weight > MAX_SNAPSHOT_WEIGHT_BYTES {
        return Err(InspectionError::SnapshotTooLarge);
    }
    Ok(FlowSnapshot {
        version: expected.version(),
        identity: expected,
        flows,
        indexed_payload_bytes: snapshot_payload_bytes,
        payload_truncated,
        cache_weight,
    })
}

mod filter;
pub(crate) use filter::{
    filter_flow_page, validate_flow_id, validate_flow_page_bounds, validate_snapshot_version,
    FilteredFlowPage, ValidatedFlowFilter,
};

#[cfg(test)]
mod tests;
