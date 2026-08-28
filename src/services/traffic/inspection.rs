//! Bounded, coalesced PCAP indexing for the monitor traffic inspector.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use pcap_file::pcap::PcapReader;

use super::{parse_frame, InspectedChunk, InspectedDirection, InspectedFlow};
use crate::utils::error::{AppError, AppResult};

const MAX_FLOW_CACHE_ENTRIES: usize = 8;
pub(super) const FLOW_CACHE_TTL: Duration = Duration::from_secs(120);
const FLOW_PARSE_DEADLINE: Duration = Duration::from_secs(20);
pub(super) const MAX_RETAINED_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;
const MAX_RETAINED_PAYLOAD_PER_FLOW: usize = 256 * 1024;
pub(super) const FLOW_PARSE_WORK_UNIT_BYTES: u64 = 16 * 1024 * 1024;
pub(super) const FLOW_PARSE_WORK_UNITS: u32 = 16;
static FLOW_PARSE_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
static FLOW_PARSE_WORK: std::sync::LazyLock<Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| {
        Arc::new(tokio::sync::Semaphore::new(FLOW_PARSE_WORK_UNITS as usize))
    });
static FLOW_INDEX_CACHE: std::sync::LazyLock<dashmap::DashMap<FileIdentity, Arc<FlowCacheEntry>>> =
    std::sync::LazyLock::new(dashmap::DashMap::new);

pub(super) struct FlowCacheEntry {
    pub(super) inserted_at: Instant,
    pub(super) cell: tokio::sync::OnceCell<Arc<Vec<InspectedFlow>>>,
}

impl FlowCacheEntry {
    fn new() -> Self {
        Self {
            inserted_at: Instant::now(),
            cell: tokio::sync::OnceCell::new(),
        }
    }
}

struct ParseCancellation(Arc<AtomicBool>);

impl ParseCancellation {
    fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    fn signal(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.0)
    }
}

impl Drop for ParseCancellation {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

pub(super) fn parse_work_units(file_bytes: u64) -> u32 {
    u32::try_from(file_bytes.div_ceil(FLOW_PARSE_WORK_UNIT_BYTES).max(1))
        .unwrap_or(u32::MAX)
        .min(FLOW_PARSE_WORK_UNITS)
}

pub(super) fn cache_entry_expired(entry: &FlowCacheEntry, now: Instant) -> bool {
    now.saturating_duration_since(entry.inserted_at) >= FLOW_CACHE_TTL
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct FileIdentity {
    path: PathBuf,
    size: u64,
    modified_nanos: u128,
}

async fn file_identity(path: &Path) -> AppResult<FileIdentity> {
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|_| AppError::not_found("Capture not found"))?;
    let modified_nanos = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos());
    Ok(FileIdentity {
        path: path.to_path_buf(),
        size: metadata.len(),
        modified_nanos,
    })
}

/// Return one coalesced, bounded parse for the immutable file version observed
/// at admission. Replacement/growth changes the key and retires the old index.
pub async fn inspect_flows_cached(
    path: PathBuf,
    max_file_bytes: u64,
    max_flows: usize,
) -> AppResult<Arc<Vec<InspectedFlow>>> {
    let identity = file_identity(&path).await?;
    if identity.size > max_file_bytes {
        return Err(AppError::bad_request(
            "Capture is too large to inspect; download it instead",
        ));
    }
    let now = Instant::now();
    FLOW_INDEX_CACHE.retain(|key, entry| {
        !cache_entry_expired(entry, now) && (key.path != identity.path || key == &identity)
    });
    let cell = FLOW_INDEX_CACHE
        .entry(identity.clone())
        .or_insert_with(|| Arc::new(FlowCacheEntry::new()))
        .clone();
    while FLOW_INDEX_CACHE.len() > MAX_FLOW_CACHE_ENTRIES {
        let removable = FLOW_INDEX_CACHE
            .iter()
            .find(|entry| entry.key() != &identity)
            .map(|entry| entry.key().clone());
        let Some(removable) = removable else {
            break;
        };
        FLOW_INDEX_CACHE.remove(&removable);
    }
    let work_units = parse_work_units(identity.size);
    let observed_size = identity.size;
    let indexed = cell
        .cell
        .get_or_try_init(|| async move {
            let _permit = FLOW_PARSE_SLOTS.try_acquire().map_err(|_| {
                AppError::unavailable("Capture inspection capacity is busy; retry shortly")
            })?;
            let _work = Arc::clone(&FLOW_PARSE_WORK)
                .try_acquire_many_owned(work_units)
                .map_err(|_| {
                    AppError::unavailable("Capture inspection byte budget is busy; retry shortly")
                })?;
            let cancellation = ParseCancellation::new();
            let signal = cancellation.signal();
            tokio::time::timeout(
                FLOW_PARSE_DEADLINE,
                tokio::task::spawn_blocking(move || {
                    inspect_flows_bounded_cancellable(
                        &path,
                        observed_size,
                        max_flows,
                        MAX_RETAINED_PAYLOAD_BYTES,
                        Some(signal.as_ref()),
                    )
                    .map(Arc::new)
                }),
            )
            .await
            .map_err(|_| AppError::unavailable("Capture inspection timed out"))?
            .map_err(|error| {
                AppError::internal(format!("capture inspection task failed: {error}"))
            })?
        })
        .await?;
    Ok(Arc::clone(indexed))
}

fn flag_offsets(payload: &[u8]) -> Vec<usize> {
    const PREFIXES: [&[u8]; 3] = [b"flag{", b"tcp1p{", b"rsctf{"];
    let lowercase = payload
        .iter()
        .map(u8::to_ascii_lowercase)
        .collect::<Vec<_>>();
    let mut offsets = Vec::new();
    for prefix in PREFIXES {
        for offset in 0..=lowercase.len().saturating_sub(prefix.len()) {
            if lowercase.get(offset..offset + prefix.len()) == Some(prefix) {
                offsets.push(offset);
            }
        }
    }
    offsets.sort_unstable();
    offsets.dedup();
    offsets
}

#[cfg(test)]
pub(super) fn inspect_flows_bounded(
    path: &Path,
    max_file_bytes: u64,
    max_flows: usize,
    max_retained_payload: usize,
) -> AppResult<Vec<InspectedFlow>> {
    inspect_flows_bounded_cancellable(path, max_file_bytes, max_flows, max_retained_payload, None)
}

pub(super) fn inspect_flows_bounded_cancellable(
    path: &Path,
    max_file_bytes: u64,
    max_flows: usize,
    max_retained_payload: usize,
    cancellation: Option<&AtomicBool>,
) -> AppResult<Vec<InspectedFlow>> {
    let metadata = std::fs::metadata(path).map_err(|_| AppError::not_found("Capture not found"))?;
    if metadata.len() > max_file_bytes {
        return Err(AppError::bad_request(
            "Capture is too large to inspect; download it instead",
        ));
    }
    let file = std::fs::File::open(path).map_err(|_| AppError::not_found("Capture not found"))?;
    let file = file.take(max_file_bytes.saturating_add(1));
    let mut reader = PcapReader::new(file)
        .map_err(|_| AppError::bad_request("Capture is not a valid pcap file"))?;
    let mut flows = BTreeMap::<u16, InspectedFlow>::new();
    let mut retained = 0usize;

    while let Some(next) = reader.next_packet() {
        if cancellation.is_some_and(|signal| signal.load(Ordering::Acquire)) {
            return Err(AppError::unavailable("Capture inspection was superseded"));
        }
        let packet = match next {
            Ok(packet) => packet,
            Err(_) => break,
        };
        let Some(parsed) = parse_frame(&packet.data) else {
            continue;
        };
        let (connection, peer, direction) = if parsed.source.port() >= parsed.dest.port() {
            (
                parsed.source.port(),
                parsed.dest.ip(),
                InspectedDirection::TeamToContainer,
            )
        } else {
            (
                parsed.dest.port(),
                parsed.source.ip(),
                InspectedDirection::ContainerToTeam,
            )
        };
        if !flows.contains_key(&connection) && flows.len() >= max_flows {
            return Err(AppError::bad_request(
                "Capture contains too many distinct flows",
            ));
        }
        let timestamp_millis = packet.timestamp.as_millis().try_into().unwrap_or(i64::MAX);
        let flow = flows
            .entry(connection)
            .or_insert_with(|| InspectedFlow::new(connection, peer, timestamp_millis));
        flow.first_seen_millis = flow.first_seen_millis.min(timestamp_millis);
        flow.last_seen_millis = flow.last_seen_millis.max(timestamp_millis);
        let payload_len = u64::try_from(parsed.payload.len()).unwrap_or(u64::MAX);
        match direction {
            InspectedDirection::TeamToContainer => {
                flow.packets_out = flow.packets_out.saturating_add(1);
                flow.bytes_out = flow.bytes_out.saturating_add(payload_len);
            }
            InspectedDirection::ContainerToTeam => {
                flow.packets_in = flow.packets_in.saturating_add(1);
                flow.bytes_in = flow.bytes_in.saturating_add(payload_len);
            }
        }
        let offsets = flag_offsets(parsed.payload);
        flow.flag_hits = flow
            .flag_hits
            .saturating_add(u64::try_from(offsets.len()).unwrap_or(u64::MAX));
        let flow_retained = flow
            .chunks
            .iter()
            .map(|chunk| chunk.payload.len())
            .sum::<usize>();
        if !parsed.payload.is_empty()
            && retained.saturating_add(parsed.payload.len()) <= max_retained_payload
            && flow_retained.saturating_add(parsed.payload.len()) <= MAX_RETAINED_PAYLOAD_PER_FLOW
        {
            retained += parsed.payload.len();
            flow.chunks.push(InspectedChunk {
                direction,
                timestamp_millis,
                payload: parsed.payload.to_vec(),
                flag_offsets: offsets,
            });
        }
    }
    if reader.into_reader().limit() == 0 {
        return Err(AppError::bad_request(
            "Capture grew beyond the inspection size limit",
        ));
    }
    Ok(flows.into_values().collect())
}
