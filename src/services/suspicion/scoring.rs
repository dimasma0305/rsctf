//! Suspicion scoring: risk bands + per-participation breakdown.
use super::*;

// ─────────────────────────────────────────────────────────────────────────────
// Pure tiered aggregation (SuspicionScoringService.cs)
// ─────────────────────────────────────────────────────────────────────────────

/// Risk band — the headline classification an admin triages by. Derived from
/// WHICH evidence tier fired, not from a raw numeric threshold. Mirrors RSCTF
/// `RiskBand`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RiskBand {
    /// No signals at all.
    Clean = 0,
    /// Only network/identity context fired — environmental, not suspicion.
    Context = 1,
    /// Behavioral heuristics only — low confidence.
    Watch = 2,
    /// Strong automation/scanner evidence — worth investigating.
    Investigate = 3,
    /// Hard cross-team evidence — confirmed-grade.
    Evidenced = 4,
}

impl RiskBand {
    /// Localisation key, matching RSCTF `SuspicionBreakdown.BandKey`.
    pub fn band_key(self) -> &'static str {
        match self {
            RiskBand::Evidenced => "evidenced",
            RiskBand::Investigate => "investigate",
            RiskBand::Watch => "watch",
            RiskBand::Context => "context",
            RiskBand::Clean => "clean",
        }
    }
}

/// One raw suspicion event row, as it would be read from the (future)
/// SuspicionEvent table. Mirrors the C# `(Type, Details, Time, ScoreDelta)` tuple.
#[derive(Clone, Debug)]
pub struct SuspicionEventRow {
    pub rule_code: String,
    /// Internal incident identity. It is used for idempotent scoring but is not
    /// added to the established public cheat-report JSON.
    pub evidence_key: String,
    pub details: String,
    pub time: chrono::DateTime<chrono::Utc>,
    /// Persisted incident weight. Legacy rows may not have one and fall back to
    /// the supplied rule-weight resolver.
    pub score_delta: Option<i32>,
}

/// One event annotated with its tier and whether it contributed to the score.
/// Mirrors `ScoredSuspicionEvent`.
#[derive(Clone, Debug)]
pub struct ScoredSuspicionEvent {
    pub rule_code: String,
    pub details: String,
    pub time: chrono::DateTime<chrono::Utc>,
    pub score_delta: i32,
    pub tier: SuspicionTier,
    pub counted: bool,
}

/// The fair-scoring breakdown for one participation. Mirrors `SuspicionBreakdown`.
#[derive(Clone, Debug)]
pub struct SuspicionBreakdown {
    pub hard: i32,
    pub strong: i32,
    pub behavioral: i32,
    pub corroboration: i32,
    pub total: i32,
    pub band: RiskBand,
    pub events: Vec<ScoredSuspicionEvent>,
}

/// Pure, read-time aggregation of suspicion event rows into a tiered risk
/// breakdown. Faithful port of `SuspicionScoring.Compute`.
///
/// `weight` resolves the *current* weight for a rule code (admin override →
/// default); pass e.g. `default_weight` to use the compiled-in table.
///
/// Invariant: a team with zero Hard evidence can never rank above a team with
/// any Hard evidence. Context signals contribute exactly 0 on their own and only
/// corroborate existing hard evidence, capped at Hard/2.
pub fn compute_breakdown(
    events: &[SuspicionEventRow],
    weight: impl Fn(&str) -> i32,
) -> SuspicionBreakdown {
    use std::collections::{HashMap, HashSet};

    let mut annotated: Vec<ScoredSuspicionEvent> = Vec::new();
    let mut tier_subtotal: HashMap<SuspicionTier, i32> = HashMap::new();
    let mut tier_scored: HashMap<SuspicionTier, i32> = HashMap::new();
    let mut corroboration_units = 0;
    let mut context_seen: HashSet<String> = HashSet::new();

    // Group events by rule code, preserving first-seen order for determinism.
    let mut groups: Vec<String> = Vec::new();
    let mut by_type: HashMap<String, Vec<&SuspicionEventRow>> = HashMap::new();
    for e in events {
        by_type
            .entry(e.rule_code.clone())
            .or_insert_with(|| {
                groups.push(e.rule_code.clone());
                Vec::new()
            })
            .push(e);
    }

    for rule_code in &groups {
        let group = &by_type[rule_code];
        let ty = SuspicionType::from_code(rule_code);
        let tier = ty.map(|t| t.tier()).unwrap_or(SuspicionTier::Behavioral);
        let cap = ty.map(|t| t.max_incidents()).unwrap_or(3);
        // Count the MOST RECENT distinct incidents first. New rows use their
        // durable evidence key. Legacy rows did not persist incident identity or
        // score delta, so preserve their historical one-counted-row-per-rule
        // behavior even when m0052 retained several raw race-collision rows.
        let mut ordered: Vec<&&SuspicionEventRow> = group.iter().collect();
        ordered.sort_by_key(|event| std::cmp::Reverse(event.time));

        let mut seen_incident: HashSet<String> = HashSet::new();
        let mut legacy_incident_seen = false;
        let mut counted_incidents = 0;

        for e in ordered {
            let is_new_incident = if e.score_delta.is_some() {
                seen_incident.insert(e.evidence_key.clone())
            } else if legacy_incident_seen {
                false
            } else {
                legacy_incident_seen = true;
                true
            };
            let score_delta = e.score_delta.unwrap_or_else(|| weight(rule_code));
            let mut counted = false;
            if tier > SuspicionTier::Context && is_new_incident && counted_incidents < cap {
                counted_incidents += 1;
                let scored = *tier_scored.get(&tier).unwrap_or(&0);
                if scored < tier_ceiling(tier) {
                    counted = true;
                    tier_scored.insert(tier, scored + score_delta);
                    *tier_subtotal.entry(tier).or_insert(0) += score_delta;
                }
            }

            annotated.push(ScoredSuspicionEvent {
                rule_code: e.rule_code.clone(),
                details: e.details.clone(),
                time: e.time,
                score_delta,
                tier,
                counted,
            });
        }

        if tier == SuspicionTier::Context && context_seen.insert(rule_code.clone()) {
            corroboration_units += ty.map(|t| t.corroboration_unit()).unwrap_or(5);
        }
    }

    let hard = *tier_subtotal.get(&SuspicionTier::Hard).unwrap_or(&0);
    let strong = tier_ceiling(SuspicionTier::Strong)
        .min(*tier_subtotal.get(&SuspicionTier::Strong).unwrap_or(&0));
    let behavioral = tier_ceiling(SuspicionTier::Behavioral)
        .min(*tier_subtotal.get(&SuspicionTier::Behavioral).unwrap_or(&0));
    // Context only corroborates EXISTING hard evidence, never more than Hard/2.
    let corroboration = if hard > 0 {
        (hard / 2).min(corroboration_units)
    } else {
        0
    };

    let total = hard + corroboration + strong + behavioral;

    let band = if hard > 0 {
        RiskBand::Evidenced
    } else if strong > 0 {
        RiskBand::Investigate
    } else if behavioral > 0 {
        RiskBand::Watch
    } else if !context_seen.is_empty() {
        RiskBand::Context
    } else {
        RiskBand::Clean
    };

    SuspicionBreakdown {
        hard,
        strong,
        behavioral,
        corroboration,
        total,
        band,
        events: annotated,
    }
}

#[cfg(test)]
mod tests {
    use super::{compute_breakdown, RiskBand, SuspicionEventRow};
    use chrono::{Duration, Utc};

    #[test]
    fn distinct_evidence_keys_keep_incidents_and_historical_weights() {
        let now = Utc::now();
        let events = vec![
            SuspicionEventRow {
                rule_code: "StolenFlag".to_string(),
                evidence_key: "submission:500".to_string(),
                details: "Flag stolen from another team".to_string(),
                time: now - Duration::seconds(1),
                score_delta: Some(80),
            },
            SuspicionEventRow {
                rule_code: "StolenFlag".to_string(),
                evidence_key: "submission:501".to_string(),
                details: "Flag stolen from another team".to_string(),
                time: now,
                score_delta: Some(120),
            },
        ];

        let breakdown = compute_breakdown(&events, |_| 999);

        assert_eq!(breakdown.band, RiskBand::Evidenced);
        assert_eq!(breakdown.hard, 200);
        assert_eq!(breakdown.total, 200);
        assert_eq!(
            breakdown
                .events
                .iter()
                .filter(|event| event.counted)
                .count(),
            2
        );
        let mut deltas = breakdown
            .events
            .iter()
            .map(|event| event.score_delta)
            .collect::<Vec<_>>();
        deltas.sort_unstable();
        assert_eq!(deltas, vec![80, 120]);
    }

    #[test]
    fn legacy_event_without_delta_uses_rule_weight_fallback() {
        let events = vec![SuspicionEventRow {
            rule_code: "StolenFlag".to_string(),
            evidence_key: "legacy:7".to_string(),
            details: "Flag stolen from another team".to_string(),
            time: Utc::now(),
            score_delta: None,
        }];

        let breakdown = compute_breakdown(&events, |_| 75);

        assert_eq!(breakdown.hard, 75);
        assert_eq!(breakdown.events[0].score_delta, 75);
    }

    #[test]
    fn legacy_collision_rows_remain_visible_but_score_once() {
        let now = Utc::now();
        let events = vec![
            SuspicionEventRow {
                rule_code: "StolenFlag".to_string(),
                evidence_key: "legacy:7".to_string(),
                details: "Flag stolen from another team".to_string(),
                time: now - Duration::seconds(1),
                score_delta: None,
            },
            SuspicionEventRow {
                rule_code: "StolenFlag".to_string(),
                evidence_key: "legacy:8".to_string(),
                details: "Flag stolen from another team".to_string(),
                time: now,
                score_delta: None,
            },
        ];

        let breakdown = compute_breakdown(&events, |_| 75);

        assert_eq!(breakdown.hard, 75);
        assert_eq!(breakdown.total, 75);
        assert_eq!(breakdown.events.len(), 2);
        assert_eq!(
            breakdown
                .events
                .iter()
                .filter(|event| event.counted)
                .count(),
            1
        );
        assert!(breakdown.events[0].counted, "newest legacy row counts");
    }
}
