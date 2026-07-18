use super::formula::{
    score_epoch_service, validate_service_weight, EpochServiceEvidence, EpochServiceScore,
    ScoringError,
};

/// One team's score for a complete epoch after normalized service weighting.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TeamEpochScore {
    pub points: f64,
    pub service_count: usize,
    pub total_weight: f64,
}

/// Weight service-local scores into one fixed-ceiling team epoch.
///
/// An empty service set produces a zero-point epoch. Each service score is
/// weighted by its precommitted `service_weight`, then divided by the total
/// weight, so changing the number or mix of services cannot expand the
/// 100-point epoch budget.
pub fn aggregate_team_epoch(
    service_scores: &[EpochServiceScore],
) -> Result<TeamEpochScore, ScoringError> {
    let mut weighted_points = 0.0;
    let mut total_weight = 0.0;
    for score in service_scores {
        validate_service_weight(score.service_weight)?;
        if !score.local_points.is_finite() {
            return Err(ScoringError::NonFinite {
                field: "local_points",
            });
        }
        weighted_points += score.service_weight * score.local_points.clamp(0.0, 100.0);
        total_weight += score.service_weight;
    }

    let points = if total_weight == 0.0 {
        0.0
    } else {
        (weighted_points / total_weight).clamp(0.0, 100.0)
    };
    Ok(TeamEpochScore {
        points,
        service_count: service_scores.len(),
        total_weight,
    })
}

/// Score and aggregate all services for one team epoch.
pub fn score_team_epoch(evidence: &[EpochServiceEvidence]) -> Result<TeamEpochScore, ScoringError> {
    let service_scores = evidence
        .iter()
        .map(score_epoch_service)
        .collect::<Result<Vec<_>, _>>()?;
    aggregate_team_epoch(&service_scores)
}

/// Equal-weight average of finalized team-epoch point totals.
///
/// An empty epoch list returns zero. Inputs outside `[0, 100]` are saturated so
/// an upstream arithmetic error cannot expand the candidate's final ceiling;
/// non-finite inputs are rejected.
pub fn average_equal_epochs(epoch_points: &[f64]) -> Result<f64, ScoringError> {
    if epoch_points.is_empty() {
        return Ok(0.0);
    }

    let mut total = 0.0;
    for &points in epoch_points {
        if !points.is_finite() {
            return Err(ScoringError::NonFinite {
                field: "epoch_points",
            });
        }
        total += points.clamp(0.0, 100.0);
    }
    Ok((total / epoch_points.len() as f64).clamp(0.0, 100.0))
}

/// Weighted epoch average used only for a declared partial tail epoch. Complete
/// epochs use weight `1`; a tail with `r` of `n` ticks uses `r/n`, preventing a
/// one-tick event tail from carrying a full epoch's influence.
pub fn average_weighted_epochs(epoch_points: &[(f64, f64)]) -> Result<f64, ScoringError> {
    let mut weighted_total = 0.0;
    let mut total_weight = 0.0;
    for &(points, weight) in epoch_points {
        if !points.is_finite() {
            return Err(ScoringError::NonFinite {
                field: "epoch_points",
            });
        }
        if !weight.is_finite() {
            return Err(ScoringError::NonFinite {
                field: "epoch_weight",
            });
        }
        if weight < 0.0 {
            return Err(ScoringError::Negative {
                field: "epoch_weight",
            });
        }
        weighted_total += points.clamp(0.0, 100.0) * weight;
        total_weight += weight;
    }
    if total_weight == 0.0 {
        Ok(0.0)
    } else {
        Ok((weighted_total / total_weight).clamp(0.0, 100.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::ad::scoring::{
        score_epoch_service, ScoringError, MAX_SERVICE_WEIGHT, MIN_SERVICE_WEIGHT,
    };

    const EPSILON: f64 = 1e-12;

    fn close(left: f64, right: f64) {
        assert!(
            (left - right).abs() <= EPSILON,
            "expected {left} to equal {right}"
        );
    }

    fn evidence() -> EpochServiceEvidence {
        EpochServiceEvidence {
            opportunity_count: 10,
            capture_count: 6,
            rarity_sum: 0.0,
            defense_opportunity_count: 10,
            protected_opportunity_count: 6,
            sla_credit_sum: 10.0,
            sla_tick_count: 10,
            service_weight: 1.0,
        }
    }

    #[test]
    fn balanced_sixty_sixty_scores_sixty() {
        let score = score_epoch_service(&evidence()).unwrap();
        close(score.offense_rate, 0.6);
        close(score.defense_rate, 0.6);
        close(score.sla_rate, 1.0);
        close(score.core_rate, 0.6);
        close(score.local_points, 60.0);
    }

    #[test]
    fn attack_only_keeps_the_bounded_direct_share() {
        let score = score_epoch_service(&EpochServiceEvidence {
            capture_count: 10,
            defense_opportunity_count: 0,
            protected_opportunity_count: 0,
            ..evidence()
        })
        .unwrap();
        close(score.offense_rate, 1.0);
        close(score.defense_rate, 0.0);
        close(score.core_rate, 0.4);
        close(score.local_points, 40.0);
    }

    #[test]
    fn no_own_flags_create_no_defense_score() {
        let score = score_epoch_service(&EpochServiceEvidence {
            capture_count: 0,
            defense_opportunity_count: 0,
            protected_opportunity_count: 0,
            ..evidence()
        })
        .unwrap();
        close(score.defense_rate, 0.0);
        close(score.local_points, 0.0);
    }

    #[test]
    fn sla_scales_the_whole_local_score() {
        let full = score_epoch_service(&evidence()).unwrap();
        let half = score_epoch_service(&EpochServiceEvidence {
            sla_credit_sum: 5.0,
            ..evidence()
        })
        .unwrap();
        close(half.sla_rate, 0.5);
        close(half.local_points, full.local_points * 0.5);
    }

    #[test]
    fn rarity_is_normalized_by_the_frozen_opportunity_budget() {
        let score = score_epoch_service(&EpochServiceEvidence {
            capture_count: 4,
            rarity_sum: 8.0,
            defense_opportunity_count: 0,
            protected_opportunity_count: 0,
            ..evidence()
        })
        .unwrap();
        close(score.capture_rate, 0.4);
        close(score.rarity_rate, 0.8);
        close(score.offense_rate, 0.6);
        close(score.local_points, 24.0);
    }

    #[test]
    fn service_weights_normalize_to_one_epoch_budget() {
        let low = score_epoch_service(&EpochServiceEvidence {
            service_weight: MIN_SERVICE_WEIGHT,
            ..evidence()
        })
        .unwrap();
        let high = score_epoch_service(&EpochServiceEvidence {
            capture_count: 10,
            protected_opportunity_count: 10,
            service_weight: MAX_SERVICE_WEIGHT,
            ..evidence()
        })
        .unwrap();
        let epoch = aggregate_team_epoch(&[low, high]).unwrap();
        close(epoch.total_weight, 2.0);
        close(epoch.points, 84.0);

        let same = score_team_epoch(&[
            EpochServiceEvidence {
                service_weight: MIN_SERVICE_WEIGHT,
                ..evidence()
            },
            EpochServiceEvidence {
                service_weight: MAX_SERVICE_WEIGHT,
                ..evidence()
            },
        ])
        .unwrap();
        close(same.points, 60.0);
    }

    #[test]
    fn final_score_is_an_equal_epoch_average() {
        close(average_equal_epochs(&[20.0, 60.0, 100.0]).unwrap(), 60.0);
        close(average_equal_epochs(&[]).unwrap(), 0.0);
    }

    #[test]
    fn partial_tail_uses_fractional_epoch_weight() {
        close(
            average_weighted_epochs(&[(80.0, 1.0), (0.0, 0.25)]).unwrap(),
            64.0,
        );
        close(average_weighted_epochs(&[]).unwrap(), 0.0);
    }

    #[test]
    fn normalized_rates_and_aggregates_are_clamped() {
        let saturated = score_epoch_service(&EpochServiceEvidence {
            opportunity_count: 1,
            capture_count: 2,
            rarity_sum: 10.0,
            defense_opportunity_count: 1,
            protected_opportunity_count: 2,
            sla_credit_sum: 2.0,
            sla_tick_count: 1,
            service_weight: 1.0,
        })
        .unwrap();
        close(saturated.offense_rate, 1.0);
        close(saturated.defense_rate, 1.0);
        close(saturated.sla_rate, 1.0);
        close(saturated.core_rate, 1.0);
        close(saturated.local_points, 100.0);
        close(average_equal_epochs(&[-20.0, 120.0]).unwrap(), 50.0);
    }

    #[test]
    fn malformed_continuous_evidence_and_weights_are_rejected() {
        assert_eq!(
            score_epoch_service(&EpochServiceEvidence {
                rarity_sum: f64::NAN,
                ..evidence()
            }),
            Err(ScoringError::NonFinite {
                field: "rarity_sum"
            })
        );
        assert_eq!(
            score_epoch_service(&EpochServiceEvidence {
                sla_credit_sum: -1.0,
                ..evidence()
            }),
            Err(ScoringError::Negative {
                field: "sla_credit_sum"
            })
        );
        assert!(matches!(
            score_epoch_service(&EpochServiceEvidence {
                service_weight: 1.21,
                ..evidence()
            }),
            Err(ScoringError::ServiceWeightOutOfRange { .. })
        ));
        assert!(matches!(
            average_equal_epochs(&[f64::INFINITY]),
            Err(ScoringError::NonFinite {
                field: "epoch_points"
            })
        ));
    }
}
