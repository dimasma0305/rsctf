use regex::bytes::{Regex, RegexBuilder};

use super::{
    FlowDirection, FlowSnapshot, IndexedFlow, InspectionError, MAX_FLOW_PAGE_SIZE,
    MAX_REGEX_PATTERN_BYTES,
};

const MAX_FLOW_PAGE: u32 = 100_000;
const MAX_FLOW_ID_BYTES: usize = 76;
const MAX_REGEX_COMPILED_BYTES: usize = 256 * 1024;
const MAX_PEER_FILTER_BYTES: usize = 64;

#[derive(Debug)]
pub(crate) struct ValidatedFlowFilter {
    payload_regex: Option<Regex>,
    peer_ip_contains: Option<String>,
    start_utc: Option<i64>,
    end_utc: Option<i64>,
    direction: Option<FlowDirection>,
    flags_only: bool,
}

impl ValidatedFlowFilter {
    pub(crate) fn new(
        regex_pattern: Option<&str>,
        peer_ip_contains: Option<&str>,
        start_utc: Option<i64>,
        end_utc: Option<i64>,
        direction: Option<FlowDirection>,
        flags_only: bool,
    ) -> Result<Self, InspectionError> {
        if start_utc.is_some_and(|value| value < 0)
            || end_utc.is_some_and(|value| value < 0)
            || start_utc
                .zip(end_utc)
                .is_some_and(|(start, end)| start > end)
        {
            return Err(InspectionError::Invalid(
                "Flow time bounds must be non-negative Unix milliseconds with startUtc <= endUtc"
                    .into(),
            ));
        }
        let payload_regex = regex_pattern
            .filter(|pattern| !pattern.is_empty())
            .map(|pattern| {
                if pattern.len() > MAX_REGEX_PATTERN_BYTES {
                    return Err(InspectionError::Invalid(format!(
                        "regexPattern is limited to {MAX_REGEX_PATTERN_BYTES} bytes"
                    )));
                }
                RegexBuilder::new(pattern)
                    .unicode(false)
                    .size_limit(MAX_REGEX_COMPILED_BYTES)
                    .dfa_size_limit(MAX_REGEX_COMPILED_BYTES)
                    .build()
                    .map_err(|error| {
                        InspectionError::Invalid(format!("Invalid payload regex: {error}"))
                    })
            })
            .transpose()?;
        let peer_ip_contains = peer_ip_contains
            .filter(|value| !value.is_empty())
            .map(|value| {
                if value.len() > MAX_PEER_FILTER_BYTES
                    || !value
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() || matches!(byte, b'.' | b':'))
                {
                    return Err(InspectionError::Invalid(format!(
                        "peerIpContains must be at most {MAX_PEER_FILTER_BYTES} IP-address characters"
                    )));
                }
                Ok(value.to_ascii_lowercase())
            })
            .transpose()?;
        Ok(Self {
            payload_regex,
            peer_ip_contains,
            start_utc,
            end_utc,
            direction,
            flags_only,
        })
    }

    fn matches(&self, flow: &IndexedFlow) -> bool {
        if self
            .peer_ip_contains
            .as_ref()
            .is_some_and(|needle| !flow.peer_ip.to_ascii_lowercase().contains(needle))
            || self
                .start_utc
                .is_some_and(|start| flow.last_seen_utc < start)
            || self.end_utc.is_some_and(|end| flow.first_seen_utc > end)
            || (self.flags_only && flow.flag_hits == 0)
        {
            return false;
        }
        if let Some(direction) = self.direction {
            let packets = match direction {
                FlowDirection::ContainerToTeam => flow.packets_in,
                FlowDirection::TeamToContainer => flow.packets_out,
            };
            if packets == 0 {
                return false;
            }
        }
        self.payload_regex.as_ref().is_none_or(|regex| {
            flow.chunks.iter().any(|chunk| {
                self.direction
                    .is_none_or(|direction| chunk.direction == direction)
                    && regex.is_match(&chunk.payload)
            })
        })
    }
}

pub(crate) struct FilteredFlowPage {
    pub(crate) indices: Vec<usize>,
    pub(crate) total_items: usize,
}

pub(crate) fn filter_flow_page(
    snapshot: &FlowSnapshot,
    filter: &ValidatedFlowFilter,
    page: u32,
    page_size: u16,
) -> Result<FilteredFlowPage, InspectionError> {
    validate_flow_page_bounds(page, page_size)?;
    let offset = usize::try_from(page.saturating_sub(1))
        .unwrap_or(usize::MAX)
        .saturating_mul(usize::from(page_size));
    let mut total_items = 0usize;
    let mut indices = Vec::with_capacity(usize::from(page_size));
    for (index, flow) in snapshot.flows.iter().enumerate() {
        if !filter.matches(flow) {
            continue;
        }
        if total_items >= offset && indices.len() < usize::from(page_size) {
            indices.push(index);
        }
        total_items = total_items.saturating_add(1);
    }
    Ok(FilteredFlowPage {
        indices,
        total_items,
    })
}

pub(crate) fn validate_flow_page_bounds(page: u32, page_size: u16) -> Result<(), InspectionError> {
    if page == 0 || page > MAX_FLOW_PAGE {
        return Err(InspectionError::Invalid(format!(
            "page must be between 1 and {MAX_FLOW_PAGE}"
        )));
    }
    if page_size == 0 || page_size > MAX_FLOW_PAGE_SIZE {
        return Err(InspectionError::Invalid(format!(
            "pageSize must be between 1 and {MAX_FLOW_PAGE_SIZE}"
        )));
    }
    Ok(())
}

pub(crate) fn validate_snapshot_version(version: &str) -> Result<(), InspectionError> {
    if version.len() != 32 || !version.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(InspectionError::Invalid(
            "snapshotVersion must be a 32-character hexadecimal file version".into(),
        ));
    }
    Ok(())
}

pub(crate) fn validate_flow_id(flow_id: &str) -> Result<(), InspectionError> {
    if flow_id.is_empty()
        || flow_id.len() > MAX_FLOW_ID_BYTES
        || !flow_id.len().is_multiple_of(2)
        || !flow_id.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(InspectionError::Invalid(
            "flowId must be a bounded hexadecimal canonical flow identity".into(),
        ));
    }
    Ok(())
}
