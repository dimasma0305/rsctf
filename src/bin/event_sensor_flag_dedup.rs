use std::collections::HashSet;

use rsctf::services::event_security::{FlagTransportInput, MAX_TRACKED_FLOWS};
use uuid::Uuid;

pub(super) type FlagDedupKey = (i32, Uuid, String, i16, i16);

#[derive(Debug, PartialEq, Eq)]
pub(super) enum TrackFlagResult {
    Queued,
    Duplicate,
    Capacity,
}

pub(super) fn flag_dedup_key(game_id: i32, flag: &FlagTransportInput) -> FlagDedupKey {
    (
        game_id,
        flag.peer_id,
        flag.flag_value_hash.clone(),
        flag.transport,
        flag.direction,
    )
}

pub(super) fn track_flag(
    flags: &mut Vec<(i32, FlagTransportInput)>,
    seen_flags: &mut HashSet<FlagDedupKey>,
    game_id: i32,
    flag: FlagTransportInput,
) -> TrackFlagResult {
    let dedup = flag_dedup_key(game_id, &flag);
    if seen_flags.contains(&dedup) {
        return TrackFlagResult::Duplicate;
    }
    if seen_flags.len() >= MAX_TRACKED_FLOWS {
        return TrackFlagResult::Capacity;
    }
    seen_flags.insert(dedup);
    flags.push((game_id, flag));
    TrackFlagResult::Queued
}

pub(super) fn release_flag_dedup(seen_flags: &mut HashSet<FlagDedupKey>, keys: &[FlagDedupKey]) {
    for key in keys {
        seen_flags.remove(key);
    }
}

pub(super) fn release_acknowledged_flags(
    seen_flags: &mut HashSet<FlagDedupKey>,
    acknowledgements: &std::sync::mpsc::Receiver<Vec<FlagDedupKey>>,
) {
    while let Ok(keys) = acknowledgements.try_recv() {
        release_flag_dedup(seen_flags, &keys);
    }
}
