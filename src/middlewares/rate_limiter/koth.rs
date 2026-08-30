//! Admission control for managed Leaderboard KotH capability exchange.

use std::sync::LazyLock;

use axum::http::Method;
use axum::response::Response;

use super::{check_async, too_many_requests, Kind, Policy};

/// A managed arena legitimately authenticates every roster member through one
/// challenge-container source address. The default 30,000-token bucket refills
/// at 500/s, comfortably above the fixed 2,000-team lifecycle profile of 100
/// authentications/s while retaining a finite invalid-capability ceiling.
const MIN_SOURCE_ADMISSION_PER_MINUTE: u32 = 3_000;
const DEFAULT_SOURCE_ADMISSION_PER_MINUTE: u32 = 30_000;
const MAX_SOURCE_ADMISSION_PER_MINUTE: u32 = 1_000_000;
const AUTH_PATH: &str = "/api/v1/koth/capability/authenticate";

pub(super) fn parse_source_admission(value: Option<&str>) -> Result<u32, String> {
    let Some(value) = value else {
        return Ok(DEFAULT_SOURCE_ADMISSION_PER_MINUTE);
    };
    let parsed = value.parse::<u32>().map_err(|_| admission_error())?;
    if !(MIN_SOURCE_ADMISSION_PER_MINUTE..=MAX_SOURCE_ADMISSION_PER_MINUTE).contains(&parsed) {
        return Err(admission_error());
    }
    Ok(parsed)
}

fn admission_error() -> String {
    format!(
        "RSCTF_KOTH_CAPABILITY_IP_ADMISSION_PER_MINUTE must be an integer from \
         {MIN_SOURCE_ADMISSION_PER_MINUTE} through {MAX_SOURCE_ADMISSION_PER_MINUTE}"
    )
}

static SOURCE_ADMISSION_PER_MINUTE: LazyLock<Result<u32, String>> = LazyLock::new(|| {
    parse_source_admission(
        std::env::var("RSCTF_KOTH_CAPABILITY_IP_ADMISSION_PER_MINUTE")
            .ok()
            .as_deref(),
    )
});

pub(super) fn validate_configuration() -> Result<(), String> {
    SOURCE_ADMISSION_PER_MINUTE
        .as_ref()
        .map(|_| ())
        .map_err(|message| message.clone())
}

pub(super) fn source_admission_kind() -> Kind {
    let capacity = f64::from(
        SOURCE_ADMISSION_PER_MINUTE
            .as_ref()
            .copied()
            .unwrap_or_else(|message| panic!("{message}")),
    );
    Kind::Bucket {
        capacity,
        refill_per_sec: capacity / 60.0,
    }
}

pub(super) fn is_auth_request(method: &Method, path: &str) -> bool {
    method == Method::POST && path == AUTH_PATH
}

/// Bound body parsing and invalid-token lookup work without charging the
/// anonymous Global partition used by reporter context/observation traffic.
pub(super) async fn admit_source(ip: String) -> Option<Response> {
    check_async(Policy::KothCapabilityAdmission, ip)
        .await
        .err()
        .map(too_many_requests)
}

pub(super) fn partition_key(game_id: i32, challenge_id: i32, participation_id: i32) -> String {
    format!(
        "koth-capability:game:{game_id}:challenge:{challenge_id}:participation:{participation_id}"
    )
}

/// Enforce ordinary per-identity fairness after an opaque capability resolves
/// to its canonical event, hill, and participation.
pub(crate) async fn admit_authenticated(
    game_id: i32,
    challenge_id: i32,
    participation_id: i32,
) -> Option<Response> {
    let key = partition_key(game_id, challenge_id, participation_id);
    check_async(Policy::Global, key)
        .await
        .err()
        .map(too_many_requests)
}
