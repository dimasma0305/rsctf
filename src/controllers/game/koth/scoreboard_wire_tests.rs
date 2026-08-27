use axum::body::HttpBody;
use axum::http::{header, HeaderMap, StatusCode};
use bytes::Bytes;

use super::*;

const LOAD_PROFILE_TEAMS: i32 = 500;
const LOAD_PROFILE_HILLS: i32 = 20;

fn maximum_load_profile_board() -> KothScoreboardModel {
    let hills = (1..=LOAD_PROFILE_HILLS)
        .map(|challenge_id| KothScoreboardHill {
            challenge_id,
            title: format!("Hill {challenge_id:02}"),
            category: ChallengeCategory::Pwn,
            claim_source: "Marker".to_owned(),
            current_holder_team_name: Some(format!("Team {challenge_id:03}")),
            current_holder_participation_id: Some(challenge_id),
            provisional_claimant_team_name: None,
            provisional_claimant_participation_id: None,
            provisional_confirmation_ticks: 0,
            cycle_number: 7,
            cycle_tick: 3,
            reset_phase: "Active".to_owned(),
            is_scorable: true,
            next_reset_ticks: Some(7),
            cooldown_participants: Vec::new(),
            last_check_status: Some("Ok".to_owned()),
        })
        .collect::<Vec<_>>();
    let teams = (1..=LOAD_PROFILE_TEAMS)
        .map(|team_id| {
            let team_band = team_id % 97;
            KothTeamScoreRow {
                rank: team_id,
                participation_id: team_id,
                team_id,
                team_name: format!("Maximum roster team {team_id:03}"),
                division: Some(if team_id % 2 == 0 { "Open" } else { "Student" }.to_owned()),
                settled_total: 40.0 + f64::from(team_band) / 2.0,
                projected_total: 42.0 + f64::from(team_band) / 2.0,
                settled_epoch_points: 120.0 + f64::from(team_band) * 2.0,
                settled_epoch_weight: 3.0,
                projected_epoch_points: 150.0 + f64::from(team_band) * 2.0,
                projected_epoch_weight: 4.0,
                acquisition_rate: f64::from(40 + team_band % 51) / 100.0,
                control_rate: f64::from(35 + team_band % 56) / 100.0,
                reliability_rate: f64::from(50 + team_band % 50) / 100.0,
                hills: (1..=LOAD_PROFILE_HILLS)
                    .map(|challenge_id| {
                        let mix = (team_id * 31 + challenge_id * 17) % 101;
                        KothHillScore {
                            challenge_id,
                            settled_points: f64::from(200 + mix * 7) / 10.0,
                            projected_points: f64::from(220 + mix * 7) / 10.0,
                            acquisition_rate: f64::from(25 + mix % 71) / 100.0,
                            control_rate: f64::from(20 + mix % 76) / 100.0,
                            reliability_rate: f64::from(45 + mix % 55) / 100.0,
                            acquisition_windows: i64::from(1 + (team_id + challenge_id) % 13),
                            controlled_ticks: i64::from(10 + (team_id * challenge_id) % 41),
                            responsible_ticks: 50,
                            healthy_responsible_ticks: i64::from(
                                20 + (team_id + challenge_id) % 31,
                            ),
                            is_current_holder: team_id == challenge_id,
                        }
                    })
                    .collect(),
                epochs: (1..=KOTH_DETAIL_EPOCH_LIMIT)
                    .map(|epoch| KothEpochScore {
                        epoch: epoch as i32,
                        points: 20.0 + f64::from(team_band) / 2.0 + epoch as f64,
                        epoch_weight: epoch as f64,
                        finalized: true,
                    })
                    .collect(),
            }
        })
        .collect();
    KothScoreboardModel {
        epoch_ticks: 8,
        cycle_ticks: 10,
        champion_cooldown_ticks: 2,
        claim_confirmation_ticks: 2,
        start_round: Some(1),
        started: true,
        fully_settled: false,
        current_epoch: 4,
        detail_epoch_limit: KOTH_DETAIL_EPOCH_LIMIT,
        latest_round: 31,
        current_round_ends_at: None,
        tick_seconds: 60,
        generated_at: Utc::now(),
        is_frozen_view: false,
        freeze: None,
        hills,
        teams,
    }
}

#[tokio::test]
async fn maximum_roster_and_hill_profile_is_cacheable_and_conditionally_compact() {
    let raw = Bytes::from(serde_json::to_vec(&maximum_load_profile_board()).unwrap());
    let built = super::super::scoreboard_encoding::build_stable_bundle(
        raw.clone(),
        "_KothScoreBoardWireV2Frozen_load-profile".to_owned(),
        b"\"generatedAt\":",
    )
    .await
    .unwrap();
    assert!(
        built.cacheable,
        "{LOAD_PROFILE_TEAMS}-team × {LOAD_PROFILE_HILLS}-hill bundle must fit the cache cap"
    );
    let bundle_bytes = built.bytes.len();

    let mut headers = HeaderMap::new();
    headers.insert(header::ACCEPT_ENCODING, "br, gzip;q=0.8".parse().unwrap());
    let response =
        super::super::scoreboard_encoding::response(built.bytes.clone(), &headers).unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_ENCODING], "br");
    let etag = response.headers()[header::ETAG].clone();
    let encoded_bytes = response.body().size_hint().exact().unwrap() as usize;
    assert!(
        encoded_bytes < raw.len() / 5,
        "encoded={encoded_bytes}, raw={}",
        raw.len()
    );
    eprintln!(
        "koth-scoreboard-profile teams={LOAD_PROFILE_TEAMS} hills={LOAD_PROFILE_HILLS} rawBytes={} brotliBytes={encoded_bytes} cacheBundleBytes={bundle_bytes}",
        raw.len()
    );

    headers.insert(header::IF_NONE_MATCH, etag);
    let unchanged = super::super::scoreboard_encoding::response(built.bytes, &headers).unwrap();
    assert_eq!(unchanged.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(unchanged.body().size_hint().exact(), Some(0));
}
