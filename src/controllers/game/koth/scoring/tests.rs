use super::*;

#[test]
fn recent_rollup_epochs_keep_the_true_timeline_prefix() {
    let header = rollup::RollupHeaderRow {
        epoch: 1,
        cumulative_scorable_ticks: 8,
        cumulative_eligible_windows: 2,
    };
    let prefix = rollup::TeamRollupRow {
        participation_id: 7,
        cumulative_points_numerator: 20.0,
        cumulative_epoch_weight: 1.0,
        cumulative_acquisition_numerator: 0.5,
        cumulative_control_numerator: 0.5,
        cumulative_sla_numerator: 1.0,
        cumulative_rate_weight: 1.0,
        cumulative_acquisition_windows: 1,
        cumulative_controlled_ticks: 4,
        cumulative_responsible_ticks: 4,
        cumulative_healthy_responsible_ticks: 4,
    };
    let tail = KothTeamAggregate {
        settled_total: 0.0,
        projected_total: 80.0,
        acquisition_rate: 0.0,
        control_rate: 0.0,
        reliability_rate: 0.0,
        cells: HashMap::new(),
        epochs: vec![KothEpochAggregate {
            epoch: 2,
            points: 80.0,
            epoch_weight: 0.5,
            finalized: false,
            cumulative_points_numerator: 40.0,
            cumulative_epoch_weight: 0.5,
        }],
    };
    let raw = KothScoringSnapshot {
        teams: HashMap::from([(7, tail)]),
        hills: BTreeMap::new(),
        fully_settled: false,
    };
    let recent = rollup::RecentTeamEpochRow {
        participation_id: 7,
        epoch: 1,
        points: 20.0,
        epoch_weight: 1.0,
        cumulative_points_numerator: 20.0,
        cumulative_epoch_weight: 1.0,
    };

    let merged = merge_rollup_prefix(
        &[7],
        Some(&header),
        vec![prefix],
        Vec::new(),
        vec![recent],
        raw,
        false,
        2,
    );
    let team = &merged.teams[&7];
    // This fixture carries no hill cells, so it exercises only the timeline
    // prefix; the normalized event score is covered by the field-best tests.
    assert_eq!(team.epochs.len(), 2);
    assert_eq!(team.epochs[0].cumulative_points_numerator, 20.0);
    assert_eq!(team.epochs[0].cumulative_epoch_weight, 1.0);
    assert_eq!(team.epochs[1].cumulative_points_numerator, 60.0);
    assert_eq!(team.epochs[1].cumulative_epoch_weight, 1.5);
}

#[test]
fn scoring_uses_each_teams_personal_cooldown_denominator() {
    let meta = vec![HillEpochMetaRow {
        challenge_id: 5,
        claim_source: "Marker".to_string(),
        epoch: 1,
        start_round: 1,
        end_round: 3,
        service_weight: 1.0,
        round_count: 3,
        result_count: 3,
        scorable_ticks: 3,
        eligible_windows: 1,
        all_finalized: true,
        max_checked_at: Some(Utc::now()),
    }];
    let evidence = vec![
        TeamEvidenceRow {
            participation_id: 7,
            challenge_id: 5,
            epoch: 1,
            acquisition_windows: 1,
            controlled_ticks: 2,
            responsible_ticks: 2,
            healthy_responsible_ticks: 2,
            personal_scorable_ticks: 2,
            personal_eligible_windows: 1,
            api_activity_rate: None,
            api_objective_rate: None,
            api_performance_rate: None,
            api_lead_rate: None,
        },
        TeamEvidenceRow {
            participation_id: 9,
            challenge_id: 5,
            epoch: 1,
            acquisition_windows: 1,
            controlled_ticks: 2,
            responsible_ticks: 2,
            healthy_responsible_ticks: 2,
            personal_scorable_ticks: 3,
            personal_eligible_windows: 1,
            api_activity_rate: None,
            api_objective_rate: None,
            api_performance_rate: None,
            api_lead_rate: None,
        },
    ];

    let scored = score_evidence_rows(&meta, &evidence, &[7, 9], 3, false).unwrap();

    assert!((scored.teams[&7].control_rate - 1.0).abs() < 1e-12);
    assert!((scored.teams[&9].control_rate - (2.0 / 3.0)).abs() < 1e-12);
    assert!(scored.teams[&7].projected_total > scored.teams[&9].projected_total);
}

#[test]
fn complete_epochs_keep_equal_weight_but_partial_tail_uses_played_ticks() {
    let mut row = HillEpochMetaRow {
        challenge_id: 5,
        claim_source: "Marker".to_string(),
        epoch: 1,
        start_round: 1,
        end_round: 12,
        service_weight: 1.0,
        round_count: 12,
        result_count: 12,
        scorable_ticks: 8,
        eligible_windows: 4,
        all_finalized: true,
        max_checked_at: Some(Utc::now()),
    };
    assert_eq!(epoch_weight_fraction(&[row.clone()], 12), 1.0);

    row.end_round = 3;
    row.round_count = 3;
    row.result_count = 3;
    row.scorable_ticks = 2;
    let mut second_hill = row.clone();
    second_hill.challenge_id = 6;
    second_hill.service_weight = 1.2;
    second_hill.scorable_ticks = 3;
    let expected_weight = (2.0 + 1.2 * 3.0) / ((1.0 + 1.2) * 12.0);
    assert!((epoch_weight_fraction([&row, &second_hill], 12) - expected_weight).abs() < 1e-12);
}

#[test]
fn wholly_void_hill_does_not_dilute_an_available_hill() {
    let meta = vec![
        HillEpochMetaRow {
            challenge_id: 5,
            claim_source: "Marker".to_string(),
            epoch: 1,
            start_round: 1,
            end_round: 3,
            service_weight: 1.0,
            round_count: 3,
            result_count: 3,
            // This complete epoch contains platform voids, but its one
            // attributable tick keeps the hill at full normalization weight.
            scorable_ticks: 1,
            eligible_windows: 1,
            all_finalized: true,
            max_checked_at: Some(Utc::now()),
        },
        HillEpochMetaRow {
            challenge_id: 6,
            claim_source: "Marker".to_string(),
            epoch: 1,
            start_round: 1,
            end_round: 3,
            service_weight: 1.0,
            round_count: 3,
            result_count: 3,
            scorable_ticks: 0,
            eligible_windows: 0,
            all_finalized: true,
            max_checked_at: Some(Utc::now()),
        },
    ];
    let evidence = vec![TeamEvidenceRow {
        participation_id: 7,
        challenge_id: 5,
        epoch: 1,
        acquisition_windows: 1,
        controlled_ticks: 1,
        responsible_ticks: 1,
        healthy_responsible_ticks: 1,
        personal_scorable_ticks: 1,
        personal_eligible_windows: 1,
        api_activity_rate: None,
        api_objective_rate: None,
        api_performance_rate: None,
        api_lead_rate: None,
    }];

    let scored = score_evidence_rows(&meta, &evidence, &[7], 3, false).unwrap();
    let team = &scored.teams[&7];

    assert!((team.projected_total - 100.0).abs() < 1e-12);
    assert!((team.settled_total - 100.0).abs() < 1e-12);
    assert!((team.epochs[0].epoch_weight - 1.0).abs() < 1e-12);
    assert!((team.cells[&5].projected_weight - 1.0).abs() < 1e-12);
    assert!((team.cells[&6].projected_weight - 0.0).abs() < 1e-12);
}

#[test]
fn leaderboard_uses_relative_performance_and_crown_share_for_every_team() {
    let meta = vec![HillEpochMetaRow {
        challenge_id: 8,
        claim_source: "Api".to_string(),
        epoch: 1,
        start_round: 1,
        end_round: 3,
        service_weight: 1.0,
        round_count: 3,
        result_count: 3,
        scorable_ticks: 3,
        eligible_windows: 3,
        all_finalized: true,
        max_checked_at: Some(Utc::now()),
    }];
    let evidence = vec![
        TeamEvidenceRow {
            participation_id: 7,
            challenge_id: 8,
            epoch: 1,
            acquisition_windows: 3,
            controlled_ticks: 3,
            responsible_ticks: 3,
            healthy_responsible_ticks: 3,
            personal_scorable_ticks: 3,
            personal_eligible_windows: 3,
            api_activity_rate: Some(1.0),
            api_objective_rate: Some(0.8),
            api_performance_rate: Some(1.0),
            api_lead_rate: Some(1.0),
        },
        TeamEvidenceRow {
            participation_id: 9,
            challenge_id: 8,
            epoch: 1,
            acquisition_windows: 2,
            controlled_ticks: 2,
            responsible_ticks: 3,
            healthy_responsible_ticks: 0,
            personal_scorable_ticks: 3,
            personal_eligible_windows: 3,
            api_activity_rate: Some(0.5),
            api_objective_rate: Some(0.4),
            api_performance_rate: Some(0.4_f64.powf(0.75)),
            api_lead_rate: Some(0.0),
        },
    ];
    let scored = score_evidence_rows(&meta, &evidence, &[7, 9], 3, false).unwrap();
    assert!(scored.teams[&7].projected_total > scored.teams[&9].projected_total);
    assert_eq!(scored.teams[&7].acquisition_rate, 1.0);
    assert_eq!(scored.teams[&7].control_rate, 0.8);
    assert_eq!(scored.teams[&7].reliability_rate, 1.0);
}

fn close(left: f64, right: f64) {
    assert!(
        (left - right).abs() < 1e-9,
        "expected {left} to equal {right}"
    );
}

fn hill_rollup(
    participation_id: i32,
    challenge_id: i32,
    points: f64,
    weight: f64,
) -> rollup::HillRollupRow {
    rollup::HillRollupRow {
        participation_id,
        challenge_id,
        service_weight: weight,
        cumulative_points_numerator: points,
        cumulative_score_weight: 1.0,
        cumulative_acquisition_numerator: 0.0,
        cumulative_control_numerator: 0.0,
        cumulative_sla_numerator: 0.0,
        cumulative_rate_weight: 1.0,
        cumulative_acquisition_windows: 0,
        cumulative_controlled_ticks: 0,
        cumulative_responsible_ticks: 0,
        cumulative_healthy_responsible_ticks: 0,
    }
}

fn team_rollup(participation_id: i32, points: f64) -> rollup::TeamRollupRow {
    rollup::TeamRollupRow {
        participation_id,
        cumulative_points_numerator: points,
        cumulative_epoch_weight: 1.0,
        cumulative_acquisition_numerator: 0.0,
        cumulative_control_numerator: 0.0,
        cumulative_sla_numerator: 0.0,
        cumulative_rate_weight: 1.0,
        cumulative_acquisition_windows: 0,
        cumulative_controlled_ticks: 0,
        cumulative_responsible_ticks: 0,
        cumulative_healthy_responsible_ticks: 0,
    }
}

#[test]
fn event_score_normalizes_each_hill_to_its_capped_field_best() {
    let header = rollup::RollupHeaderRow {
        epoch: 1,
        cumulative_scorable_ticks: 8,
        cumulative_eligible_windows: 2,
    };
    // Hill 5: the best event average is 80, so the hill scales by 1.25.
    // Hill 6: the best event average is 10, so the cap holds the factor at 4.
    let merged = merge_rollup_prefix(
        &[1, 2],
        Some(&header),
        vec![team_rollup(1, 45.0), team_rollup(2, 25.0)],
        vec![
            hill_rollup(1, 5, 80.0, 1.0),
            hill_rollup(1, 6, 10.0, 1.0),
            hill_rollup(2, 5, 40.0, 1.0),
            hill_rollup(2, 6, 10.0, 1.0),
        ],
        Vec::new(),
        KothScoringSnapshot::default(),
        true,
        1,
    );

    let five = merged.hills[&5];
    let six = merged.hills[&6];
    close(five.settled_field_best, 80.0);
    close(five.settled_multiplier, 1.25);
    close(six.settled_field_best, 10.0);
    close(six.settled_multiplier, 4.0);
    close(five.settled_share, 0.5);
    close(six.settled_share, 0.5);
    close(five.projected_share, 0.5);

    let one = &merged.teams[&1];
    close(one.cells[&5].settled_points, 80.0);
    close(one.cells[&5].settled_normalized_points, 100.0);
    close(one.cells[&6].settled_normalized_points, 40.0);
    close(one.settled_total, 70.0);
    close(one.projected_total, 70.0);

    let two = &merged.teams[&2];
    close(two.cells[&5].settled_normalized_points, 50.0);
    close(two.cells[&6].settled_normalized_points, 40.0);
    close(two.settled_total, 45.0);
    assert!(merged.fully_settled);
}

#[test]
fn hill_weights_and_void_hills_shape_the_normalized_shares() {
    let header = rollup::RollupHeaderRow {
        epoch: 1,
        cumulative_scorable_ticks: 8,
        cumulative_eligible_windows: 2,
    };
    let mut void_hill = hill_rollup(1, 9, 0.0, 1.0);
    void_hill.cumulative_score_weight = 0.0;
    let mut void_hill_two = hill_rollup(2, 9, 0.0, 1.0);
    void_hill_two.cumulative_score_weight = 0.0;
    let merged = merge_rollup_prefix(
        &[1, 2],
        Some(&header),
        vec![team_rollup(1, 60.0), team_rollup(2, 30.0)],
        vec![
            hill_rollup(1, 5, 60.0, 1.2),
            hill_rollup(1, 6, 0.0, 0.8),
            void_hill,
            hill_rollup(2, 5, 30.0, 1.2),
            hill_rollup(2, 6, 50.0, 0.8),
            void_hill_two,
        ],
        Vec::new(),
        KothScoringSnapshot::default(),
        false,
        1,
    );

    // A wholly void hill contributes neither numerator nor denominator.
    close(merged.hills[&9].settled_share, 0.0);
    close(merged.hills[&9].settled_multiplier, 1.0);
    close(merged.hills[&5].settled_share, 0.6);
    close(merged.hills[&6].settled_share, 0.4);
    // Hill 5 best 60 -> x(100/60); hill 6 best 50 -> x2.
    let one = &merged.teams[&1];
    close(one.cells[&5].settled_normalized_points, 100.0);
    close(one.cells[&6].settled_normalized_points, 0.0);
    close(one.settled_total, 60.0);
    let two = &merged.teams[&2];
    close(two.cells[&5].settled_normalized_points, 50.0);
    close(two.cells[&6].settled_normalized_points, 100.0);
    close(two.settled_total, 0.6 * 50.0 + 0.4 * 100.0);
    assert!(!merged.fully_settled);
}
