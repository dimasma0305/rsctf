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
    assert!((team.projected_total - 40.0).abs() < 1e-12);
    assert!((team.settled_total - 20.0).abs() < 1e-12);
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
    }];

    let scored = score_evidence_rows(&meta, &evidence, &[7], 3, false).unwrap();
    let team = &scored.teams[&7];

    assert!((team.projected_total - 100.0).abs() < 1e-12);
    assert!((team.settled_total - 100.0).abs() < 1e-12);
    assert!((team.epochs[0].epoch_weight - 1.0).abs() < 1e-12);
    assert!((team.cells[&5].projected_weight - 1.0).abs() < 1e-12);
    assert!((team.cells[&6].projected_weight - 0.0).abs() < 1e-12);
}
