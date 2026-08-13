use std::time::Duration;

use sea_orm::DatabaseConnection;

use super::super::bounded_diagnostic;
use super::scheduling::{
    api_snapshot_arrival_deadline, api_snapshot_arrival_is_pending, API_SNAPSHOT_POLL_INTERVAL,
};
use super::LiveHill;
use crate::services::ad::engine::{
    koth_api::{
        read_koth_api_snapshot, stable_koth_api_snapshot, KothApiSnapshot, KothApiSnapshotRead,
    },
    koth_marker::{read_koth_marker, stable_koth_marker, KothMarkerRead},
};
use crate::services::container::ContainerManager;

pub(super) enum ClaimInputRead {
    Marker(KothMarkerRead),
    Api(KothApiSnapshotRead),
}

pub(super) async fn read_claim_input(
    db: &DatabaseConnection,
    containers: &dyn ContainerManager,
    hill: &LiveHill,
    round_id: i32,
) -> ClaimInputRead {
    match hill.claim_source.as_str() {
        "Marker" => {
            ClaimInputRead::Marker(read_koth_marker(containers, Some(&hill.container_id)).await)
        }
        "Api" => ClaimInputRead::Api(
            read_koth_api_snapshot(
                db.get_postgres_connection_pool(),
                hill.target_id,
                hill.cycle_id,
                hill.token_window_attempt,
                &hill.container_id,
                round_id,
                hill.round_start,
                hill.round_end,
            )
            .await,
        ),
        source => ClaimInputRead::Marker(KothMarkerRead::Unavailable(format!(
            "unsupported snapshotted KotH claim source {source:?}"
        ))),
    }
}

pub(super) async fn read_initial_claim_input(
    db: &DatabaseConnection,
    containers: &dyn ContainerManager,
    hill: &LiveHill,
    round_id: i32,
    planned_timeout: Duration,
    effective_deadline: tokio::time::Instant,
) -> ClaimInputRead {
    let wait_until = api_snapshot_arrival_deadline(
        effective_deadline,
        planned_timeout,
        tokio::time::Instant::now(),
    );
    loop {
        let input = read_claim_input(db, containers, hill, round_id).await;
        let now = tokio::time::Instant::now();
        // An early empty heartbeat is not a settled Leaderboard result. The
        // referee may still append a wave that ended at the cutoff; accepting
        // the empty snapshot immediately can race that append and silently
        // void an otherwise valid scoring round.
        let has_finalized_wave = matches!(
            &input,
            ClaimInputRead::Api(KothApiSnapshotRead::Observed(snapshot))
                if !snapshot.waves.is_empty()
        );
        if hill.claim_source != "Api"
            || !api_snapshot_arrival_is_pending(has_finalized_wave, now, wait_until)
        {
            return input;
        }
        tokio::time::sleep(std::cmp::min(
            API_SNAPSHOT_POLL_INTERVAL,
            wait_until.saturating_duration_since(now),
        ))
        .await;
    }
}

fn stable_claim_input(
    before: ClaimInputRead,
    after: ClaimInputRead,
) -> (
    Option<String>,
    bool,
    Option<KothApiSnapshot>,
    Option<String>,
) {
    match (before, after) {
        (ClaimInputRead::Marker(before), ClaimInputRead::Marker(after)) => {
            let (marker, observed, error) = stable_koth_marker(before, after);
            (marker, observed, None, error)
        }
        (ClaimInputRead::Api(before), ClaimInputRead::Api(after)) => {
            let (snapshot, error) = stable_koth_api_snapshot(before, after);
            let observed = snapshot.is_some();
            (None, observed, snapshot, error)
        }
        _ => (
            None,
            false,
            None,
            Some("KotH claim source changed during the functional probe".to_string()),
        ),
    }
}

pub(super) fn stable_claim_outcome(
    before: ClaimInputRead,
    after: ClaimInputRead,
    checker_message: Option<String>,
) -> (
    Option<String>,
    bool,
    Option<KothApiSnapshot>,
    Option<String>,
) {
    let (marker, observed, snapshot, evidence_error) = stable_claim_input(before, after);
    let message = match (checker_message, evidence_error) {
        (Some(checker), Some(evidence)) => {
            Some(format!("{checker}; {}", bounded_diagnostic(evidence)))
        }
        (Some(checker), None) => Some(checker),
        (None, Some(evidence)) => Some(bounded_diagnostic(evidence)),
        (None, None) => None,
    };
    (marker, observed, snapshot, message)
}
