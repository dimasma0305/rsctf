use std::collections::HashSet;

use super::{
    authoritative_round_window, classify_round_target, complete_engine_scoring_roster,
    network_scope_matches, playable_round_window, prepared_checker_exists, valid_service_endpoint,
    RoundTargetDisposition,
};
use chrono::{Duration, Utc};

#[test]
fn round_scope_revalidation_rejects_ownership_changes() {
    assert!(network_scope_matches(None, false));
    assert!(network_scope_matches(None, true));
    assert!(network_scope_matches(Some(false), false));
    assert!(!network_scope_matches(Some(false), true));
    assert!(!network_scope_matches(Some(true), false));
    assert!(network_scope_matches(Some(true), true));
}

#[test]
fn checker_readiness_requires_prepared_files() {
    assert!(!prepared_checker_exists(None));
    assert!(!prepared_checker_exists(Some("")));
    assert!(!prepared_checker_exists(Some(
        "/definitely/missing/rsctf-checker"
    )));
}

#[test]
fn service_readiness_rejects_provisioning_placeholders() {
    assert!(!valid_service_endpoint("", 0));
    assert!(!valid_service_endpoint("  ", 31337));
    assert!(!valid_service_endpoint("10.13.37.2", 0));
    assert!(valid_service_endpoint("10.13.37.2", 31337));
}

#[test]
fn scoring_roster_requires_two_complete_teams() {
    let challenges = [10, 11];
    let complete = HashSet::from([(1, 10), (1, 11), (2, 10), (2, 11)]);
    assert!(complete_engine_scoring_roster(
        &[1, 2],
        &challenges,
        false,
        false,
        &complete,
        true,
        true,
    ));
    assert!(!complete_engine_scoring_roster(
        &[1],
        &challenges,
        false,
        false,
        &complete,
        true,
        true,
    ));
    let partial = HashSet::from([(1, 10), (1, 11), (2, 10)]);
    assert!(!complete_engine_scoring_roster(
        &[1, 2],
        &challenges,
        false,
        false,
        &partial,
        true,
        true,
    ));
    assert!(!complete_engine_scoring_roster(
        &[1, 2],
        &[],
        false,
        false,
        &complete,
        true,
        true,
    ));
    assert!(!complete_engine_scoring_roster(
        &[1, 2],
        &challenges,
        false,
        false,
        &complete,
        false,
        true,
    ));
}

#[test]
fn koth_scoring_requires_ready_crown_lifecycle() {
    let empty = HashSet::new();
    assert!(complete_engine_scoring_roster(
        &[1, 2],
        &[],
        true,
        true,
        &empty,
        true,
        true,
    ));
    assert!(!complete_engine_scoring_roster(
        &[1, 2],
        &[],
        true,
        false,
        &empty,
        true,
        true,
    ));
    assert!(!complete_engine_scoring_roster(
        &[1, 2],
        &[],
        true,
        true,
        &empty,
        true,
        false,
    ));
}

#[test]
fn concurrent_call_repairs_the_same_successor() {
    assert_eq!(
        classify_round_target(Some((12, 8)), Some((11, 7))),
        RoundTargetDisposition::Repair
    );
    assert_eq!(
        classify_round_target(Some((1, 1)), None),
        RoundTargetDisposition::Repair
    );
}

#[test]
fn current_snapshot_advances_but_stale_snapshot_does_not_skip() {
    assert_eq!(
        classify_round_target(Some((11, 7)), Some((11, 7))),
        RoundTargetDisposition::Advance
    );
    assert_eq!(
        classify_round_target(Some((13, 9)), Some((11, 7))),
        RoundTargetDisposition::Stale
    );
}

#[test]
fn authoritative_windows_do_not_inherit_scheduler_delay() {
    let game_start = Utc::now();
    let game_end = game_start + Duration::seconds(125);
    let first = authoritative_round_window(game_start, game_end, 5, 30, None).unwrap();
    assert_eq!(first.0, game_start + Duration::seconds(5));
    assert_eq!(first.1 - first.0, Duration::seconds(30));

    // A caller arriving late still derives the successor from the prior stored
    // boundary, not from its own wall clock.
    let second = authoritative_round_window(game_start, game_end, 5, 30, Some(first.1)).unwrap();
    assert_eq!(second.0, first.1);
    assert_eq!(second.1 - second.0, Duration::seconds(30));

    let final_partial = authoritative_round_window(
        game_start,
        game_end,
        5,
        30,
        Some(game_start + Duration::seconds(115)),
    )
    .unwrap();
    assert_eq!(final_partial.1, game_end);
}

#[test]
fn elapsed_window_reanchors_without_replaying_live_flags() {
    let nominal_start = Utc::now();
    let nominal_end = nominal_start + Duration::seconds(30);
    let recovered_at = nominal_end + Duration::seconds(75);
    let event_end = recovered_at + Duration::minutes(5);
    let (start, end, reanchored) = playable_round_window(
        (nominal_start, nominal_end),
        event_end,
        30,
        recovered_at,
        15,
    )
    .unwrap();
    assert!(reanchored);
    assert_eq!(start, recovered_at);
    assert_eq!(end - start, Duration::seconds(30));
}

#[test]
fn late_poll_reanchors_a_full_tick_without_overlap() {
    let prior_end = Utc::now();
    let nominal = (prior_end, prior_end + Duration::seconds(30));
    let prepared_at = prior_end + Duration::seconds(5);
    let event_end = prepared_at + Duration::minutes(5);
    let (start, end, reanchored) =
        playable_round_window(nominal, event_end, 30, prepared_at, 15).unwrap();
    assert!(reanchored);
    assert_eq!(start, prepared_at);
    assert_eq!(end - start, Duration::seconds(30));
    assert!(start >= prior_end, "successor rounds must never overlap");
}

#[test]
fn terminal_round_is_capped_only_when_minimum_runway_remains() {
    let now = Utc::now();
    let nominal = (now, now + Duration::seconds(30));
    let playable_end = now + Duration::seconds(15);
    let (start, end, _) = playable_round_window(nominal, playable_end, 30, now, 15).unwrap();
    assert_eq!(start, now);
    assert_eq!(end, playable_end);

    assert!(playable_round_window(nominal, now + Duration::seconds(14), 30, now, 15,).is_none());
}
