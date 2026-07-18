use thiserror::Error;

pub const OFFENSE_WEIGHT: f64 = 0.4;
pub const DEFENSE_WEIGHT: f64 = 0.4;
pub const BALANCE_WEIGHT: f64 = 0.2;
/// Relative rarity coefficient. Since `H <= C`, `0.25 * H` is at most 25% of
/// base capture coverage (and at most 0.20 absolute lift), not a 25-point pool.
pub const RARITY_COEFFICIENT: f64 = 0.25;
pub const MIN_SERVICE_WEIGHT: f64 = 0.8;
pub const MAX_SERVICE_WEIGHT: f64 = 1.2;

/// Settled evidence for one team, service, and epoch.
///
/// Counts are flag opportunities frozen by the caller. `rarity_sum` is the sum
/// of bounded rarity fractions attached to accepted captures. Defense is the
/// observable share of opponent-flag pairs without an accepted capture; it does
/// not claim that the opponent attempted an exploit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EpochServiceEvidence {
    pub opportunity_count: u64,
    pub capture_count: u64,
    pub rarity_sum: f64,
    pub defense_opportunity_count: u64,
    pub protected_opportunity_count: u64,
    pub sla_credit_sum: f64,
    pub sla_tick_count: u64,
    pub service_weight: f64,
}

/// Normalized rates and points produced from one [`EpochServiceEvidence`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EpochServiceScore {
    pub capture_rate: f64,
    pub rarity_rate: f64,
    pub offense_rate: f64,
    pub defense_rate: f64,
    pub sla_rate: f64,
    pub core_rate: f64,
    /// The service-local score before cross-service weighting, in `[0, 100]`.
    pub local_points: f64,
    pub service_weight: f64,
}

#[derive(Clone, Copy, Debug, Error, PartialEq)]
pub enum ScoringError {
    #[error("{field} must be finite")]
    NonFinite { field: &'static str },
    #[error("{field} must be nonnegative")]
    Negative { field: &'static str },
    #[error("service_weight must be in [{MIN_SERVICE_WEIGHT}, {MAX_SERVICE_WEIGHT}], got {value}")]
    ServiceWeightOutOfRange { value: f64 },
}

fn validate_nonnegative(value: f64, field: &'static str) -> Result<(), ScoringError> {
    if !value.is_finite() {
        return Err(ScoringError::NonFinite { field });
    }
    if value < 0.0 {
        return Err(ScoringError::Negative { field });
    }
    Ok(())
}

pub(super) fn validate_service_weight(value: f64) -> Result<(), ScoringError> {
    if !value.is_finite() {
        return Err(ScoringError::NonFinite {
            field: "service_weight",
        });
    }
    if !(MIN_SERVICE_WEIGHT..=MAX_SERVICE_WEIGHT).contains(&value) {
        return Err(ScoringError::ServiceWeightOutOfRange { value });
    }
    Ok(())
}

fn count_rate(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn value_rate(numerator: f64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator / denominator as f64
    }
}

/// Score one settled team-service epoch with the official balanced formula.
///
/// ```text
/// O = min(1, captures/opportunities + 0.25*rarity_sum/opportunities)
/// D = protected_opponent_flag_pairs/eligible_opponent_flag_pairs
/// R = clamp(sla_credit_sum/sla_tick_count, 0, 1)
/// Core  = 0.4*O + 0.4*D + 0.2*sqrt(O*D)
/// Local = 100*R*Core
/// ```
///
/// Count inconsistencies saturate the affected normalized rate rather than
/// allowing a score above its fixed ceiling. Non-finite or negative continuous
/// evidence is rejected instead of silently creating points.
pub fn score_epoch_service(
    evidence: &EpochServiceEvidence,
) -> Result<EpochServiceScore, ScoringError> {
    validate_nonnegative(evidence.rarity_sum, "rarity_sum")?;
    validate_nonnegative(evidence.sla_credit_sum, "sla_credit_sum")?;
    validate_service_weight(evidence.service_weight)?;

    let capture_rate = count_rate(evidence.capture_count, evidence.opportunity_count);
    let rarity_rate = value_rate(evidence.rarity_sum, evidence.opportunity_count);
    let offense_rate = (capture_rate + RARITY_COEFFICIENT * rarity_rate).clamp(0.0, 1.0);
    let defense_rate = count_rate(
        evidence.protected_opportunity_count,
        evidence.defense_opportunity_count,
    )
    .clamp(0.0, 1.0);
    let sla_rate = value_rate(evidence.sla_credit_sum, evidence.sla_tick_count).clamp(0.0, 1.0);
    let core_rate = (OFFENSE_WEIGHT * offense_rate
        + DEFENSE_WEIGHT * defense_rate
        + BALANCE_WEIGHT * (offense_rate * defense_rate).sqrt())
    .clamp(0.0, 1.0);
    let local_points = (100.0 * sla_rate * core_rate).clamp(0.0, 100.0);

    Ok(EpochServiceScore {
        capture_rate,
        rarity_rate,
        offense_rate,
        defense_rate,
        sla_rate,
        core_rate,
        local_points,
        service_weight: evidence.service_weight,
    })
}
