use super::*;

#[test]
fn event_end_caps_monitor_and_public_evidence() {
    let end = Utc::now();
    let freeze = end - chrono::Duration::minutes(30);
    assert_eq!(
        scoreboard_evidence_cutoff(Some(freeze), end, end, false),
        Some(end)
    );
    assert_eq!(
        scoreboard_evidence_cutoff(Some(freeze), end, end, true),
        Some(end)
    );
    assert_eq!(
        scoreboard_evidence_cutoff(Some(freeze), end, end - chrono::Duration::hours(1), true),
        None
    );
}

#[test]
fn negative_database_counts_saturate_at_zero() {
    assert_eq!(count(-1), 0);
    assert_eq!(count(7), 7);
}

#[test]
fn detail_limit_stays_small_and_nonzero() {
    assert!((1..=3).contains(&TEAM_DETAIL_EPOCH_LIMIT));
}

fn rank_row(
    participation_id: i32,
    settled_total: f64,
    projected_total: f64,
    offense_rate: f64,
    defense_rate: f64,
    sla_rate: f64,
) -> AdTeamScore {
    AdTeamScore {
        rank: 0,
        participation_id,
        team_id: participation_id,
        team_name: format!("team-{participation_id}"),
        division: None,
        settled_total,
        projected_total,
        offense_rate,
        defense_rate,
        sla_rate,
        services: Vec::new(),
        epochs: Vec::new(),
    }
}

#[test]
fn ordinal_rank_uses_settled_projection_rates_then_id() {
    let mut rows = vec![
        rank_row(2, 50.0, 60.0, 0.8, 0.8, 0.8),
        rank_row(4, 50.0, 60.0, 0.8, 0.9, 0.8),
        rank_row(6, 50.0, 61.0, 0.1, 0.1, 0.1),
        rank_row(1, 50.0, 60.0, 0.8, 0.8, 0.8),
        rank_row(7, 51.0, 0.0, 0.0, 0.0, 0.0),
        rank_row(3, 50.0, 60.0, 0.8, 0.8, 0.9),
        rank_row(5, 50.0, 60.0, 0.9, 0.1, 0.1),
    ];

    sort_and_rank_team_rows(&mut rows);

    assert_eq!(
        rows.iter()
            .map(|row| row.participation_id)
            .collect::<Vec<_>>(),
        [7, 6, 5, 4, 3, 1, 2]
    );
    assert_eq!(
        rows.iter().map(|row| row.rank).collect::<Vec<_>>(),
        [1, 2, 3, 4, 5, 6, 7]
    );
}

#[test]
fn projected_rates_fractionally_weight_a_short_tail() {
    let service = |offense_rate| ScoredService {
        challenge_id: 1,
        capture_count: 0,
        offense_rate,
        defense_rate: 0.0,
        sla_rate: 0.0,
        service_weight: 1.0,
        local_points: 0.0,
    };
    let epochs = vec![
        ScoredEpoch {
            summary: AdEpochScore {
                epoch: 1,
                points: 0.0,
                epoch_weight: 1.0,
                finalized: true,
            },
            services: vec![service(0.0)],
        },
        ScoredEpoch {
            summary: AdEpochScore {
                epoch: 2,
                points: 0.0,
                epoch_weight: 0.25,
                finalized: false,
            },
            services: vec![service(1.0)],
        },
    ];
    assert!((projected_rate(&epochs, |row| row.offense_rate) - 0.2).abs() < 1e-9);
}

#[test]
fn challenge_contributions_reconcile_with_unequal_weights_and_partial_tail() {
    let epoch = ScoredEpoch {
        summary: AdEpochScore {
            epoch: 2,
            points: 52.0,
            epoch_weight: 0.25,
            finalized: false,
        },
        services: vec![
            ScoredService {
                challenge_id: 1,
                capture_count: 2,
                offense_rate: 0.4,
                defense_rate: 0.8,
                sla_rate: 1.0,
                service_weight: 0.8,
                local_points: 40.0,
            },
            ScoredService {
                challenge_id: 2,
                capture_count: 3,
                offense_rate: 0.9,
                defense_rate: 0.7,
                sla_rate: 1.0,
                service_weight: 1.2,
                local_points: 60.0,
            },
        ],
    };
    let historical = |challenge_id, contribution| ServiceRollupRow {
        participation_id: 1,
        challenge_id,
        cumulative_points_numerator: contribution,
        cumulative_epoch_weight: 1.0,
        cumulative_offense_numerator: 0.5,
        cumulative_defense_numerator: 0.5,
        cumulative_sla_numerator: 1.0,
        cumulative_capture_count: 4,
    };
    let left = merge_service_detail(1, Some(&historical(1, 20.0)), &[epoch.clone()], None);
    let right = merge_service_detail(2, Some(&historical(2, 30.0)), &[epoch], None);

    assert!((left.settled_points + right.settled_points - 50.0).abs() < 1e-12);
    let expected_projected = (50.0 + 52.0 * 0.25) / 1.25;
    assert!((left.projected_points + right.projected_points - expected_projected).abs() < 1e-12);
    assert_eq!(left.capture_count + right.capture_count, 13);
}

#[test]
fn five_hundred_team_ten_service_payload_stays_bounded() {
    const SERVICE_COUNT: i32 = 10;
    let teams = (1..=500)
        .map(|id| AdTeamScore {
            rank: id,
            participation_id: id,
            team_id: id,
            team_name: format!("Team {id:03}"),
            division: Some(format!("Division {}", id % 4)),
            settled_total: 50.0,
            projected_total: 55.0,
            offense_rate: 0.5,
            defense_rate: 0.6,
            sla_rate: 0.9,
            services: (1..=SERVICE_COUNT)
                .map(|challenge_id| AdServiceScore {
                    challenge_id,
                    settled_points: 50.0 / f64::from(SERVICE_COUNT),
                    projected_points: 55.0 / f64::from(SERVICE_COUNT),
                    offense_rate: 0.5,
                    defense_rate: 0.6,
                    sla_rate: 0.9,
                    capture_count: 10,
                    last_check_status: Some("Ok".to_string()),
                })
                .collect(),
            epochs: (1..=TEAM_DETAIL_EPOCH_LIMIT)
                .map(|epoch| AdEpochScore {
                    epoch: epoch as i32,
                    points: 55.0,
                    epoch_weight: 1.0,
                    finalized: true,
                })
                .collect(),
        })
        .collect();
    let board = AdScoreboard {
        epoch_ticks: 8,
        start_round: Some(1),
        started: true,
        fully_settled: false,
        current_epoch: 3,
        latest_round: 24,
        current_round_ends_at: None,
        tick_seconds: 60,
        is_frozen_view: false,
        freeze: None,
        challenges: (1..=SERVICE_COUNT)
            .map(|challenge_id| AdScoreboardChallenge {
                challenge_id,
                title: format!("Service {challenge_id}"),
                category: ChallengeCategory::Web,
            })
            .collect(),
        detail_epoch_limit: TEAM_DETAIL_EPOCH_LIMIT,
        evidence: AdEvidenceStatus::default(),
        teams,
        generated_at: Utc::now(),
    };
    let bytes = serde_json::to_vec(&board).expect("serialize board");
    assert!(
        bytes.len() < 2 * 1024 * 1024,
        "500-team, {SERVICE_COUNT}-service board is {} bytes",
        bytes.len()
    );
}

#[tokio::test]
#[ignore = "requires a migrated Postgres with an A&D game"]
async fn database_board_is_finite_bounded_and_serializable() {
    let url = std::env::var("RSCTF_TEST_DATABASE_URL").expect("test database URL");
    let game_id: i32 = std::env::var("RSCTF_TEST_GAME_ID")
        .expect("test game id")
        .parse()
        .expect("numeric game id");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .expect("connect test database");
    let board = build_ad_scoreboard(&pool, game_id, false, Utc::now())
        .await
        .expect("build official board");

    assert!(board.teams.iter().all(|team| {
        let settled_services = team
            .services
            .iter()
            .map(|service| service.settled_points)
            .sum::<f64>();
        let projected_services = team
            .services
            .iter()
            .map(|service| service.projected_points)
            .sum::<f64>();
        team.epochs.len() <= TEAM_DETAIL_EPOCH_LIMIT
            && team.settled_total.is_finite()
            && (0.0..=100.0).contains(&team.settled_total)
            && team.projected_total.is_finite()
            && (0.0..=100.0).contains(&team.projected_total)
            && [team.offense_rate, team.defense_rate, team.sla_rate]
                .into_iter()
                .all(|rate| rate.is_finite() && (0.0..=1.0).contains(&rate))
            && team.services.iter().all(|service| {
                service.settled_points.is_finite()
                    && service.projected_points.is_finite()
                    && service.capture_count < u64::MAX
                    && [service.offense_rate, service.defense_rate, service.sla_rate]
                        .into_iter()
                        .all(|rate| rate.is_finite() && (0.0..=1.0).contains(&rate))
            })
            && (settled_services - team.settled_total).abs() <= 1e-8
            && (projected_services - team.projected_total).abs() <= 1e-8
            && team.epochs.iter().all(|epoch| {
                epoch.points.is_finite()
                    && (0.0..=100.0).contains(&epoch.points)
                    && epoch.epoch_weight.is_finite()
                    && (0.0..=1.0).contains(&epoch.epoch_weight)
            })
    }));
    let bytes = serde_json::to_vec(&board).expect("serialize official board");
    let service_count = board
        .teams
        .iter()
        .map(|team| team.services.len())
        .sum::<usize>();
    let payload_budget = 64 * 1024 + board.teams.len() * 512 + service_count * 192;
    assert!(
        bytes.len() <= payload_budget,
        "official board is {} bytes for {} teams and {} services (budget {})",
        bytes.len(),
        board.teams.len(),
        service_count,
        payload_budget
    );
}
