use super::{
    authoritative_round_window, classify_round_target, complete_ad_scoring_roster,
    complete_koth_scoring_roster, earliest_complete_ad_roster_round, koth_scoring_lifecycle_ready,
    minimum_round_duration_seconds, network_scope_matches, playable_round_window,
    prepared_checker_exists, RoundTargetDisposition,
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
fn ad_scoring_start_does_not_wait_for_service_enrollment() {
    let challenges = [10, 11];
    assert!(complete_ad_scoring_roster(
        &[1, 2],
        &challenges,
        true,
        false
    ));
    assert!(!complete_ad_scoring_roster(&[1], &challenges, true, false));
    assert!(!complete_ad_scoring_roster(&[1, 2], &[], true, false));
    assert!(!complete_ad_scoring_roster(
        &[1, 2],
        &challenges,
        false,
        false,
    ));
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn delayed_scoring_recovers_the_first_round_with_the_complete_roster() {
    use sqlx::{Connection, PgConnection};

    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let mut connection = PgConnection::connect(&database_url).await.unwrap();
    sqlx::query(
        r#"CREATE TEMP TABLE "AdRounds" (
               id INTEGER PRIMARY KEY,
               game_id INTEGER NOT NULL,
               number INTEGER NOT NULL
           )"#,
    )
    .execute(&mut connection)
    .await
    .unwrap();
    sqlx::query(
        r#"CREATE TEMP TABLE "AdFlags" (
               round_id INTEGER NOT NULL,
               team_service_id INTEGER NOT NULL
           )"#,
    )
    .execute(&mut connection)
    .await
    .unwrap();
    sqlx::raw_sql(
        r#"INSERT INTO "AdRounds" (id, game_id, number)
           VALUES (1, 7, 1), (2, 7, 2), (3, 7, 3);
           INSERT INTO "AdFlags" (round_id, team_service_id)
           VALUES (1, 10), (2, 10), (2, 11), (3, 10), (3, 11)"#,
    )
    .execute(&mut connection)
    .await
    .unwrap();

    assert_eq!(
        earliest_complete_ad_roster_round(&mut connection, 7, 3, &[10, 11])
            .await
            .unwrap(),
        Some(2)
    );
    assert_eq!(
        earliest_complete_ad_roster_round(&mut connection, 7, 3, &[10, 11, 12])
            .await
            .unwrap(),
        None
    );
}

#[test]
fn practice_scoring_can_start_with_one_team() {
    assert!(complete_koth_scoring_roster(
        &[1],
        true,
        true,
        true,
        true,
        true,
    ));
    assert!(!complete_koth_scoring_roster(
        &[1],
        true,
        true,
        true,
        true,
        false,
    ));

    assert!(complete_ad_scoring_roster(&[1], &[58], true, true));
}

#[test]
fn koth_scoring_requires_ready_target_checker_and_crown_lifecycle() {
    assert!(complete_koth_scoring_roster(
        &[1, 2],
        true,
        true,
        true,
        true,
        false,
    ));
    assert!(!complete_koth_scoring_roster(
        &[1, 2],
        true,
        false,
        true,
        true,
        false,
    ));
    assert!(!complete_koth_scoring_roster(
        &[1, 2],
        true,
        true,
        false,
        true,
        false,
    ));
}

#[test]
fn managed_vpn_is_required_only_when_a_marker_cooldown_can_select_a_champion() {
    assert!(koth_scoring_lifecycle_ready(true, false, 1, 2, false));
    assert!(koth_scoring_lifecycle_ready(true, true, 0, 2, false));
    assert!(koth_scoring_lifecycle_ready(true, true, 1, 1, false));
    assert!(!koth_scoring_lifecycle_ready(true, true, 1, 2, false));
    assert!(koth_scoring_lifecycle_ready(true, true, 1, 2, true));
    assert!(!koth_scoring_lifecycle_ready(false, false, 0, 1, true));
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
    let prepared_at = prior_end + Duration::seconds(7);
    let event_end = prepared_at + Duration::minutes(5);
    let (start, end, reanchored) =
        playable_round_window(nominal, event_end, 30, prepared_at, 15).unwrap();
    assert!(reanchored);
    assert_eq!(start, prepared_at);
    assert_eq!(end - start, Duration::seconds(30));
    assert!(start >= prior_end, "successor rounds must never overlap");
}

#[test]
fn ordinary_scheduler_jitter_preserves_the_configured_cadence() {
    let prior_end = Utc::now();
    let nominal = (prior_end, prior_end + Duration::seconds(30));
    let prepared_at = prior_end + Duration::milliseconds(5_250);
    let event_end = prepared_at + Duration::minutes(5);
    let (start, end, reanchored) =
        playable_round_window(nominal, event_end, 30, prepared_at, 15).unwrap();
    assert!(!reanchored);
    assert_eq!(start, prior_end);
    assert_eq!(end, prior_end + Duration::seconds(30));
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

#[test]
fn leaderboard_terminal_round_requires_a_nonempty_settlement_window() {
    let now = Utc::now();
    let nominal = (now, now + Duration::seconds(30));
    let minimum = super::super::koth_api::API_WAVE_SETTLEMENT_LAG_SECONDS.saturating_add(1);
    assert_eq!(minimum, 21);
    assert!(
        playable_round_window(nominal, now + Duration::seconds(15), 30, now, minimum).is_none()
    );
    assert!(
        playable_round_window(nominal, now + Duration::seconds(20), 30, now, minimum).is_none()
    );
    let (start, end, reanchored) =
        playable_round_window(nominal, now + Duration::seconds(21), 30, now, minimum).unwrap();
    assert_eq!(start, now);
    assert_eq!(end, now + Duration::seconds(21));
    assert!(!reanchored);
    assert_eq!(end - Duration::seconds(20), now + Duration::seconds(1));
}

#[test]
fn api_hill_wiring_raises_the_production_minimum_to_twenty_one_seconds() {
    assert_eq!(minimum_round_duration_seconds(3, false), 15);
    assert_eq!(minimum_round_duration_seconds(3, true), 21);
}

#[test]
fn leaderboard_round_absorbs_every_too_short_terminal_tail() {
    let start = Utc::now();
    let nominal_end = start + Duration::seconds(30);
    let minimum = minimum_round_duration_seconds(3, true);

    for tail_seconds in 1..minimum {
        let event_end = nominal_end + Duration::seconds(tail_seconds);
        let (_, end, reanchored) =
            playable_round_window((start, nominal_end), event_end, 30, start, minimum).unwrap();
        assert!(!reanchored);
        assert_eq!(end, event_end, "tail={tail_seconds}s");
        assert!(
            end - Duration::seconds(super::super::koth_api::API_WAVE_SETTLEMENT_LAG_SECONDS)
                > nominal_end
                    - Duration::seconds(super::super::koth_api::API_WAVE_SETTLEMENT_LAG_SECONDS),
            "absorbing tail={tail_seconds}s must make the later advertised cutoff reachable"
        );
    }
}

#[test]
fn leaderboard_round_leaves_a_playable_terminal_tail_for_its_own_round() {
    let start = Utc::now();
    let nominal_end = start + Duration::seconds(30);
    let minimum = minimum_round_duration_seconds(3, true);
    let event_end = nominal_end + Duration::seconds(minimum);

    let (_, end, _) =
        playable_round_window((start, nominal_end), event_end, 30, start, minimum).unwrap();
    assert_eq!(end, nominal_end);
}
