use std::error::Error;
use std::fmt::{Display, Formatter};

pub const ACQUISITION_WEIGHT: f64 = 0.25;
pub const CONTROL_WEIGHT: f64 = 0.55;
pub const BALANCE_WEIGHT: f64 = 0.20;
pub const MIN_SERVICE_WEIGHT: f64 = 0.8;
pub const MAX_SERVICE_WEIGHT: f64 = 1.2;

#[derive(Clone, Copy, Debug, PartialEq)]
struct KothFormulaWeights {
    acquisition: f64,
    control: f64,
    balance: f64,
}

const fn formula_weights() -> KothFormulaWeights {
    KothFormulaWeights {
        acquisition: ACQUISITION_WEIGHT,
        control: CONTROL_WEIGHT,
        balance: BALANCE_WEIGHT,
    }
}

/// Frozen evidence for one team, hill, and epoch.
///
/// The caller must omit platform-attributed samples from all denominators.
/// `controlled_ticks` requires an exact current-window token match, while
/// `responsible_ticks` counts non-void samples for which that team controlled
/// the hill and was therefore responsible for its service state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KothEpochHillEvidence {
    pub scorable_ticks: i64,
    pub acquisition_windows: i64,
    pub eligible_windows: i64,
    pub controlled_ticks: i64,
    pub responsible_ticks: i64,
    pub healthy_responsible_ticks: i64,
    pub service_weight: f64,
}

/// Normalized rates and bounded points for one team, hill, and epoch.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KothEpochHillScore {
    pub acquisition_rate: f64,
    pub control_rate: f64,
    pub reliability_rate: f64,
    pub core_rate: f64,
    pub local_points: f64,
    pub service_weight: f64,
}

/// One scored hill and its field-wide evidence eligibility.
///
/// `evidence_fraction` is zero only when the hill has no scorable evidence for
/// the field. Any positive evidence makes it one: individual void samples are
/// already removed from the personal scoring denominators, while a shortened
/// final epoch is weighted separately.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WeightedHillScore {
    pub score: KothEpochHillScore,
    pub evidence_fraction: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KothTeamEpochScore {
    pub points: f64,
    pub hill_count: usize,
    pub total_weight: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum KothScoringError {
    NonFinite { field: &'static str },
    Negative { field: &'static str },
    FractionOutOfRange { field: &'static str, value: f64 },
    ServiceWeightOutOfRange { value: f64 },
    InvalidWeightTotal { value: f64 },
}

impl Display for KothScoringError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NonFinite { field } => write!(formatter, "{field} must be finite"),
            Self::Negative { field } => write!(formatter, "{field} must be nonnegative"),
            Self::FractionOutOfRange { field, value } => {
                write!(formatter, "{field} must be in [0, 1], got {value}")
            }
            Self::ServiceWeightOutOfRange { value } => write!(
                formatter,
                "service_weight must be in [{MIN_SERVICE_WEIGHT}, {MAX_SERVICE_WEIGHT}], got {value}"
            ),
            Self::InvalidWeightTotal { value } => {
                write!(formatter, "formula weights must sum to 1, got {value}")
            }
        }
    }
}

impl Error for KothScoringError {}

fn validate_nonnegative(value: f64, field: &'static str) -> Result<(), KothScoringError> {
    if !value.is_finite() {
        return Err(KothScoringError::NonFinite { field });
    }
    if value < 0.0 {
        return Err(KothScoringError::Negative { field });
    }
    Ok(())
}

fn validate_fraction(value: f64, field: &'static str) -> Result<(), KothScoringError> {
    validate_nonnegative(value, field)?;
    if value > 1.0 {
        return Err(KothScoringError::FractionOutOfRange { field, value });
    }
    Ok(())
}

fn validate_service_weight(value: f64) -> Result<(), KothScoringError> {
    if !value.is_finite() {
        return Err(KothScoringError::NonFinite {
            field: "service_weight",
        });
    }
    if value < 0.0 {
        return Err(KothScoringError::Negative {
            field: "service_weight",
        });
    }
    if !(MIN_SERVICE_WEIGHT..=MAX_SERVICE_WEIGHT).contains(&value) {
        return Err(KothScoringError::ServiceWeightOutOfRange { value });
    }
    Ok(())
}

fn validate_count(value: i64, field: &'static str) -> Result<(), KothScoringError> {
    if value < 0 {
        return Err(KothScoringError::Negative { field });
    }
    Ok(())
}

fn validate_evidence(evidence: &KothEpochHillEvidence) -> Result<(), KothScoringError> {
    validate_count(evidence.scorable_ticks, "scorable_ticks")?;
    validate_count(evidence.acquisition_windows, "acquisition_windows")?;
    validate_count(evidence.eligible_windows, "eligible_windows")?;
    validate_count(evidence.controlled_ticks, "controlled_ticks")?;
    validate_count(evidence.responsible_ticks, "responsible_ticks")?;
    validate_count(
        evidence.healthy_responsible_ticks,
        "healthy_responsible_ticks",
    )?;
    validate_service_weight(evidence.service_weight)
}

fn validate_formula_weights(weights: KothFormulaWeights) -> Result<(), KothScoringError> {
    validate_nonnegative(weights.acquisition, "acquisition_weight")?;
    validate_nonnegative(weights.control, "control_weight")?;
    validate_nonnegative(weights.balance, "balance_weight")?;
    let total = weights.acquisition + weights.control + weights.balance;
    if !total.is_finite() {
        return Err(KothScoringError::NonFinite {
            field: "formula_weight_total",
        });
    }
    if (total - 1.0).abs() > 1e-12 {
        return Err(KothScoringError::InvalidWeightTotal { value: total });
    }
    Ok(())
}

fn count_rate(numerator: i64, denominator: i64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        (numerator as f64 / denominator as f64).clamp(0.0, 1.0)
    }
}

/// Return the bounded observed share of an expected evidence budget.
pub fn evidence_fraction(observed: u64, expected: u64) -> f64 {
    if expected == 0 {
        0.0
    } else {
        (observed as f64 / expected as f64).clamp(0.0, 1.0)
    }
}

/// Score one team-hill epoch with the crown-cycle KotH formula.
///
/// ```text
/// A = acquisition_windows / eligible_windows
/// C = controlled_ticks / scorable_ticks
/// R = healthy_responsible_ticks / responsible_ticks
/// Core  = 0.25*A + 0.55*C + 0.20*sqrt(A*C)
/// Local = 100*R*Core
/// ```
///
/// Rates saturate at one when upstream counts are inconsistent. The output is
/// always in `[0, 100]`; an empty denominator contributes a zero rate.
pub fn score_epoch_hill(
    evidence: &KothEpochHillEvidence,
) -> Result<KothEpochHillScore, KothScoringError> {
    score_epoch_hill_with_weights(formula_weights(), evidence)
}

/// Internal seam used to exhaustively test validation of the fixed formula.
fn score_epoch_hill_with_weights(
    weights: KothFormulaWeights,
    evidence: &KothEpochHillEvidence,
) -> Result<KothEpochHillScore, KothScoringError> {
    validate_evidence(evidence)?;
    validate_formula_weights(weights)?;

    let acquisition_rate = count_rate(evidence.acquisition_windows, evidence.eligible_windows);
    let control_rate = count_rate(evidence.controlled_ticks, evidence.scorable_ticks);
    let reliability_rate = count_rate(
        evidence.healthy_responsible_ticks,
        evidence.responsible_ticks,
    );
    let core_rate = (weights.acquisition * acquisition_rate
        + weights.control * control_rate
        + weights.balance * (acquisition_rate * control_rate).sqrt())
    .clamp(0.0, 1.0);
    let local_points = (100.0 * reliability_rate * core_rate).clamp(0.0, 100.0);

    Ok(KothEpochHillScore {
        acquisition_rate,
        control_rate,
        reliability_rate,
        core_rate,
        local_points,
        service_weight: evidence.service_weight,
    })
}

/// Normalize weighted hill scores into one fixed-ceiling team epoch.
pub fn aggregate_epoch_hills(
    hills: &[WeightedHillScore],
) -> Result<KothTeamEpochScore, KothScoringError> {
    let mut weighted_points = 0.0;
    let mut total_weight = 0.0;

    for hill in hills {
        validate_service_weight(hill.score.service_weight)?;
        validate_fraction(hill.evidence_fraction, "evidence_fraction")?;
        validate_fraction(hill.score.acquisition_rate, "acquisition_rate")?;
        validate_fraction(hill.score.control_rate, "control_rate")?;
        validate_fraction(hill.score.reliability_rate, "reliability_rate")?;
        validate_fraction(hill.score.core_rate, "core_rate")?;
        validate_nonnegative(hill.score.local_points, "local_points")?;

        let weight = hill.score.service_weight * hill.evidence_fraction;
        weighted_points += hill.score.local_points.clamp(0.0, 100.0) * weight;
        total_weight += weight;
    }

    let points = if total_weight == 0.0 {
        0.0
    } else {
        (weighted_points / total_weight).clamp(0.0, 100.0)
    };

    Ok(KothTeamEpochScore {
        points,
        hill_count: hills.len(),
        total_weight,
    })
}

/// Equal-weight average of complete epoch scores.
#[cfg(test)]
pub fn average_equal_epochs(epoch_points: &[f64]) -> Result<f64, KothScoringError> {
    if epoch_points.is_empty() {
        return Ok(0.0);
    }

    let mut total = 0.0;
    for &points in epoch_points {
        validate_nonnegative(points, "epoch_points")?;
        total += points.clamp(0.0, 100.0);
    }

    Ok((total / epoch_points.len() as f64).clamp(0.0, 100.0))
}

/// Average complete epochs and a declared partial tail epoch.
///
/// Complete epochs use weight `1`; a tail containing `r` of `n` expected ticks
/// uses `r/n`. This prevents a short event tail from carrying a full epoch's
/// influence while keeping every complete epoch equally important.
pub fn average_weighted_epochs(epoch_points: &[(f64, f64)]) -> Result<f64, KothScoringError> {
    let mut weighted_total = 0.0;
    let mut total_weight = 0.0;

    for &(points, weight) in epoch_points {
        validate_nonnegative(points, "epoch_points")?;
        validate_fraction(weight, "epoch_weight")?;
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

    const EPSILON: f64 = 1e-12;

    fn close(left: f64, right: f64) {
        assert!(
            (left - right).abs() <= EPSILON,
            "expected {left} to equal {right}"
        );
    }

    fn evidence() -> KothEpochHillEvidence {
        KothEpochHillEvidence {
            scorable_ticks: 10,
            acquisition_windows: 6,
            eligible_windows: 10,
            controlled_ticks: 6,
            responsible_ticks: 6,
            healthy_responsible_ticks: 6,
            service_weight: 1.0,
        }
    }

    #[test]
    fn balanced_sixty_sixty_scores_sixty() {
        let score = score_epoch_hill(&evidence()).unwrap();
        close(score.acquisition_rate, 0.6);
        close(score.control_rate, 0.6);
        close(score.reliability_rate, 1.0);
        close(score.core_rate, 0.6);
        close(score.local_points, 60.0);
    }

    #[test]
    fn reliability_scales_the_whole_score() {
        let full = score_epoch_hill(&evidence()).unwrap();
        let half = score_epoch_hill(&KothEpochHillEvidence {
            healthy_responsible_ticks: 3,
            ..evidence()
        })
        .unwrap();

        close(half.reliability_rate, 0.5);
        close(half.local_points, full.local_points * 0.5);
    }

    #[test]
    fn formula_matches_the_declared_competitive_weights() {
        let score = score_epoch_hill(&KothEpochHillEvidence {
            scorable_ticks: 10,
            acquisition_windows: 2,
            eligible_windows: 5,
            controlled_ticks: 8,
            responsible_ticks: 8,
            healthy_responsible_ticks: 6,
            service_weight: 1.0,
        })
        .unwrap();
        let expected_core = 0.25 * 0.4 + 0.55 * 0.8 + 0.20 * f64::sqrt(0.4 * 0.8);

        close(score.acquisition_rate, 0.4);
        close(score.control_rate, 0.8);
        close(score.reliability_rate, 0.75);
        close(score.core_rate, expected_core);
        close(score.local_points, 100.0 * 0.75 * expected_core);
    }

    #[test]
    fn formula_values_sustained_control_over_capture_speed() {
        let acquisition_only = score_epoch_hill(&KothEpochHillEvidence {
            acquisition_windows: 10,
            controlled_ticks: 0,
            responsible_ticks: 1,
            healthy_responsible_ticks: 1,
            ..evidence()
        })
        .unwrap();
        let control_only = score_epoch_hill(&KothEpochHillEvidence {
            acquisition_windows: 0,
            controlled_ticks: 10,
            ..evidence()
        })
        .unwrap();

        close(acquisition_only.local_points, 25.0);
        close(control_only.local_points, 55.0);
        assert!(control_only.local_points > acquisition_only.local_points);
    }

    #[test]
    fn formula_clamps_inconsistent_counts_and_requires_responsibility() {
        let bounded = score_epoch_hill(&KothEpochHillEvidence {
            scorable_ticks: 1,
            acquisition_windows: 2,
            eligible_windows: 1,
            controlled_ticks: 2,
            responsible_ticks: 1,
            healthy_responsible_ticks: 2,
            service_weight: 1.0,
        })
        .unwrap();
        close(bounded.acquisition_rate, 1.0);
        close(bounded.control_rate, 1.0);
        close(bounded.reliability_rate, 1.0);
        close(bounded.local_points, 100.0);

        let no_responsibility = score_epoch_hill(&KothEpochHillEvidence {
            responsible_ticks: 0,
            healthy_responsible_ticks: 0,
            ..evidence()
        })
        .unwrap();
        close(no_responsibility.reliability_rate, 0.0);
        close(no_responsibility.local_points, 0.0);
    }

    #[test]
    fn acquisition_or_control_alone_uses_the_declared_direct_share() {
        let acquisition_only = score_epoch_hill(&KothEpochHillEvidence {
            acquisition_windows: 10,
            controlled_ticks: 0,
            responsible_ticks: 1,
            healthy_responsible_ticks: 1,
            ..evidence()
        })
        .unwrap();
        let control_only = score_epoch_hill(&KothEpochHillEvidence {
            acquisition_windows: 0,
            controlled_ticks: 10,
            ..evidence()
        })
        .unwrap();

        close(acquisition_only.local_points, 25.0);
        close(control_only.local_points, 55.0);
    }

    #[test]
    fn no_responsible_sample_produces_no_points() {
        let score = score_epoch_hill(&KothEpochHillEvidence {
            responsible_ticks: 0,
            healthy_responsible_ticks: 0,
            ..evidence()
        })
        .unwrap();

        close(score.reliability_rate, 0.0);
        close(score.local_points, 0.0);
    }

    #[test]
    fn rates_and_points_stay_bounded_on_inconsistent_counts() {
        let score = score_epoch_hill(&KothEpochHillEvidence {
            scorable_ticks: 1,
            acquisition_windows: 2,
            eligible_windows: 1,
            controlled_ticks: 2,
            responsible_ticks: 1,
            healthy_responsible_ticks: 2,
            service_weight: 1.0,
        })
        .unwrap();

        close(score.acquisition_rate, 1.0);
        close(score.control_rate, 1.0);
        close(score.reliability_rate, 1.0);
        close(score.local_points, 100.0);
        close(evidence_fraction(20, 10), 1.0);
        close(evidence_fraction(1, 0), 0.0);
    }

    #[test]
    fn service_and_evidence_weights_preserve_one_epoch_budget() {
        let low = score_epoch_hill(&KothEpochHillEvidence {
            service_weight: MIN_SERVICE_WEIGHT,
            ..evidence()
        })
        .unwrap();
        let high = score_epoch_hill(&KothEpochHillEvidence {
            acquisition_windows: 10,
            controlled_ticks: 10,
            service_weight: MAX_SERVICE_WEIGHT,
            ..evidence()
        })
        .unwrap();
        let epoch = aggregate_epoch_hills(&[
            WeightedHillScore {
                score: low,
                evidence_fraction: 1.0,
            },
            WeightedHillScore {
                score: high,
                evidence_fraction: 1.0,
            },
        ])
        .unwrap();

        close(epoch.points, 84.0);
        close(epoch.total_weight, 2.0);

        let shortened = aggregate_epoch_hills(&[
            WeightedHillScore {
                score: KothEpochHillScore {
                    local_points: 100.0,
                    service_weight: 1.0,
                    ..low
                },
                evidence_fraction: 1.0,
            },
            WeightedHillScore {
                score: KothEpochHillScore {
                    local_points: 0.0,
                    service_weight: 1.0,
                    ..low
                },
                evidence_fraction: 0.25,
            },
        ])
        .unwrap();
        close(shortened.points, 80.0);
    }

    #[test]
    fn wholly_void_hill_is_excluded_from_epoch_normalization() {
        let perfect = score_epoch_hill(&KothEpochHillEvidence {
            acquisition_windows: 10,
            controlled_ticks: 10,
            ..evidence()
        })
        .unwrap();
        let void = score_epoch_hill(&KothEpochHillEvidence {
            scorable_ticks: 0,
            acquisition_windows: 0,
            eligible_windows: 0,
            controlled_ticks: 0,
            responsible_ticks: 0,
            healthy_responsible_ticks: 0,
            service_weight: 1.0,
        })
        .unwrap();

        let epoch = aggregate_epoch_hills(&[
            WeightedHillScore {
                score: perfect,
                evidence_fraction: 1.0,
            },
            WeightedHillScore {
                score: void,
                evidence_fraction: 0.0,
            },
        ])
        .unwrap();

        close(epoch.points, 100.0);
        close(epoch.total_weight, 1.0);
    }

    #[test]
    fn complete_epochs_are_equal_and_tail_epoch_is_fractional() {
        close(average_equal_epochs(&[20.0, 60.0, 100.0]).unwrap(), 60.0);
        close(average_equal_epochs(&[]).unwrap(), 0.0);
        close(
            average_weighted_epochs(&[(80.0, 1.0), (0.0, 0.25)]).unwrap(),
            64.0,
        );
        close(average_weighted_epochs(&[]).unwrap(), 0.0);
    }

    #[test]
    fn malformed_floats_and_weights_are_rejected() {
        assert_eq!(
            score_epoch_hill(&KothEpochHillEvidence {
                service_weight: f64::NAN,
                ..evidence()
            }),
            Err(KothScoringError::NonFinite {
                field: "service_weight"
            })
        );
        assert!(matches!(
            score_epoch_hill(&KothEpochHillEvidence {
                service_weight: 1.21,
                ..evidence()
            }),
            Err(KothScoringError::ServiceWeightOutOfRange { .. })
        ));
        assert_eq!(
            average_equal_epochs(&[-1.0]),
            Err(KothScoringError::Negative {
                field: "epoch_points"
            })
        );
        assert!(matches!(
            average_weighted_epochs(&[(10.0, 1.1)]),
            Err(KothScoringError::FractionOutOfRange {
                field: "epoch_weight",
                ..
            })
        ));
        assert!(matches!(
            aggregate_epoch_hills(&[WeightedHillScore {
                score: score_epoch_hill(&evidence()).unwrap(),
                evidence_fraction: f64::INFINITY,
            }]),
            Err(KothScoringError::NonFinite {
                field: "evidence_fraction"
            })
        ));
    }

    #[test]
    fn negative_evidence_counts_are_rejected_instead_of_sanitized() {
        let cases = [
            (
                KothEpochHillEvidence {
                    scorable_ticks: -1,
                    ..evidence()
                },
                "scorable_ticks",
            ),
            (
                KothEpochHillEvidence {
                    acquisition_windows: -1,
                    ..evidence()
                },
                "acquisition_windows",
            ),
            (
                KothEpochHillEvidence {
                    eligible_windows: -1,
                    ..evidence()
                },
                "eligible_windows",
            ),
            (
                KothEpochHillEvidence {
                    controlled_ticks: -1,
                    ..evidence()
                },
                "controlled_ticks",
            ),
            (
                KothEpochHillEvidence {
                    responsible_ticks: -1,
                    ..evidence()
                },
                "responsible_ticks",
            ),
            (
                KothEpochHillEvidence {
                    healthy_responsible_ticks: -1,
                    ..evidence()
                },
                "healthy_responsible_ticks",
            ),
        ];

        for (bad, field) in cases {
            assert_eq!(
                score_epoch_hill(&bad),
                Err(KothScoringError::Negative { field })
            );
        }
    }

    #[test]
    fn malformed_formula_weights_are_rejected() {
        assert!(matches!(
            score_epoch_hill_with_weights(
                KothFormulaWeights {
                    acquisition: f64::NAN,
                    control: 0.55,
                    balance: 0.20,
                },
                &evidence(),
            ),
            Err(KothScoringError::NonFinite {
                field: "acquisition_weight"
            })
        ));
        assert!(matches!(
            score_epoch_hill_with_weights(
                KothFormulaWeights {
                    acquisition: -0.01,
                    control: 0.81,
                    balance: 0.20,
                },
                &evidence(),
            ),
            Err(KothScoringError::Negative {
                field: "acquisition_weight"
            })
        ));
        assert!(matches!(
            score_epoch_hill_with_weights(
                KothFormulaWeights {
                    acquisition: 0.25,
                    control: 0.50,
                    balance: 0.20,
                },
                &evidence(),
            ),
            Err(KothScoringError::InvalidWeightTotal { .. })
        ));
    }

    #[test]
    fn partial_final_epoch_weight_is_bounded_played_over_expected() {
        let weight = evidence_fraction(3, 12);
        close(weight, 0.25);
        close(
            average_weighted_epochs(&[(80.0, 1.0), (40.0, weight)]).unwrap(),
            72.0,
        );
        close(evidence_fraction(13, 12), 1.0);
        close(evidence_fraction(0, 0), 0.0);
    }
}
