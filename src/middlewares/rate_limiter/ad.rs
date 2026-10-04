//! Canonical A&D submission work admission.

use std::sync::LazyLock;

use axum::response::Response;

use super::{check_weighted_async, too_many_requests, Policy};

/// Maximum distinct plausible flags one participation may enqueue immediately.
/// Four maximum-size batches leave room for ordinary exploit retries without
/// turning the fixed-rate test allowance into a five-minute production burst.
const MIN_SUBMIT_BURST_FLAGS: u32 = 100;
pub(super) const DEFAULT_SUBMIT_BURST_FLAGS: u32 = 400;
const MAX_SUBMIT_BURST_FLAGS: u32 = 3_200;

pub(super) fn parse_submit_burst_flags(value: Option<&str>) -> Result<u32, String> {
    let Some(value) = value else {
        return Ok(DEFAULT_SUBMIT_BURST_FLAGS);
    };
    let parsed = value.parse::<u32>().map_err(|_| burst_error())?;
    if !(MIN_SUBMIT_BURST_FLAGS..=MAX_SUBMIT_BURST_FLAGS).contains(&parsed) {
        return Err(burst_error());
    }
    Ok(parsed)
}

fn burst_error() -> String {
    format!(
        "RSCTF_AD_SUBMIT_BURST_FLAGS must be an integer from \
         {MIN_SUBMIT_BURST_FLAGS} through {MAX_SUBMIT_BURST_FLAGS}"
    )
}

static SUBMIT_BURST_FLAGS: LazyLock<Result<u32, String>> = LazyLock::new(|| {
    parse_submit_burst_flags(std::env::var("RSCTF_AD_SUBMIT_BURST_FLAGS").ok().as_deref())
});

pub(super) fn validate_configuration() -> Result<(), String> {
    SUBMIT_BURST_FLAGS
        .as_ref()
        .map(|_| ())
        .map_err(|message| message.clone())
}

pub(super) fn submit_burst_flags() -> u32 {
    SUBMIT_BURST_FLAGS
        .as_ref()
        .copied()
        .unwrap_or_else(|message| panic!("{message}"))
}

/// Enforce the team-scoped A&D work budget after authentication has resolved a
/// canonical participation. Returning the normal 429 response preserves the
/// public error envelope and `Retry-After` header.
pub(crate) async fn admit_submit(
    game_id: i32,
    participation_id: i32,
    distinct_plausible_flags: usize,
) -> Option<Response> {
    let cost = u32::try_from(distinct_plausible_flags.max(1)).unwrap_or(u32::MAX);
    let key = format!("game:{game_id}:participation:{participation_id}");
    check_weighted_async(Policy::AdSubmit, key, cost)
        .await
        .err()
        .map(too_many_requests)
}
