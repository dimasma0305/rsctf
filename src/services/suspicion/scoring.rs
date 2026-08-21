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
    /// Only non-actionable context/telemetry fired — not direct suspicion.
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

/// One persisted suspicion-event row. Mirrors the C#
/// `(Type, Details, Time, ScoreDelta)` tuple.
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
    /// Exact contribution applied after deduplication, per-rule incident caps,
    /// and tier ceilings. Context rules apply only their bounded corroboration;
    /// telemetry-only Context rules remain zero.
    pub applied_delta: i64,
    pub tier: SuspicionTier,
    pub counted: bool,
}

/// The fair-scoring breakdown for one participation. Mirrors `SuspicionBreakdown`.
#[derive(Clone, Debug)]
pub struct SuspicionBreakdown {
    pub hard: i64,
    pub strong: i64,
    pub behavioral: i64,
    pub corroboration: i64,
    pub total: i64,
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
/// corroborate existing hard evidence, capped at Hard/2. Pre-cutover rows marked
/// `legacy-untrusted:` remain auditable at zero but never consume incident caps,
/// select a risk band, or provide corroboration.
pub fn compute_breakdown(
    events: &[SuspicionEventRow],
    weight: impl Fn(&str) -> i32,
) -> SuspicionBreakdown {
    use std::collections::{HashMap, HashSet};

    let mut annotated: Vec<ScoredSuspicionEvent> = Vec::new();
    let mut tier_subtotal: HashMap<SuspicionTier, i64> = HashMap::new();
    let mut context_seen: HashSet<String> = HashSet::new();
    let mut context_candidates: Vec<(chrono::DateTime<chrono::Utc>, String, usize, i64)> =
        Vec::new();

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
        let mut counted_incidents = 0_i32;
        let mut newest_trusted_context_index = None;

        for e in ordered {
            let is_untrusted = e.evidence_key.starts_with("legacy-untrusted:");
            // m0052 assigned `legacy:<id>` evidence identities to pre-ledger
            // rows. m0091 freezes their score deltas, but the prefix must keep
            // the original one-incident-per-rule collision behavior.
            let is_legacy = e.score_delta.is_none() || e.evidence_key.starts_with("legacy:");
            let is_new_incident = if is_untrusted || (is_legacy && legacy_incident_seen) {
                false
            } else if is_legacy {
                legacy_incident_seen = true;
                true
            } else {
                seen_incident.insert(e.evidence_key.clone())
            };
            let score_delta = if is_untrusted {
                0
            } else {
                e.score_delta.unwrap_or_else(|| weight(rule_code))
            };
            let mut counted = false;
            let mut applied_delta = 0_i64;
            if tier > SuspicionTier::Context && is_new_incident && counted_incidents < cap {
                counted_incidents += 1;
                let scored = *tier_subtotal.get(&tier).unwrap_or(&0);
                let contribution =
                    i64::from(score_delta.max(0)).min(tier_ceiling(tier).saturating_sub(scored));
                if contribution > 0 {
                    counted = true;
                    applied_delta = contribution;
                    *tier_subtotal.entry(tier).or_insert(0) += contribution;
                }
            }

            let annotated_index = annotated.len();
            annotated.push(ScoredSuspicionEvent {
                rule_code: e.rule_code.clone(),
                details: e.details.clone(),
                time: e.time,
                score_delta,
                applied_delta,
                tier,
                counted,
            });
            if tier == SuspicionTier::Context
                && !is_untrusted
                && newest_trusted_context_index.is_none()
            {
                newest_trusted_context_index = Some(annotated_index);
            }
        }

        if tier == SuspicionTier::Context {
            match newest_trusted_context_index {
                Some(newest_annotated_index) if context_seen.insert(rule_code.clone()) => {
                    let unit = i64::from(ty.map(|t| t.corroboration_unit()).unwrap_or(5));
                    if unit > 0 {
                        let newest = &annotated[newest_annotated_index];
                        context_candidates.push((
                            newest.time,
                            rule_code.clone(),
                            newest_annotated_index,
                            unit,
                        ));
                    }
                }
                _ => {}
            }
        }
    }

    let hard = *tier_subtotal.get(&SuspicionTier::Hard).unwrap_or(&0);
    let strong = *tier_subtotal.get(&SuspicionTier::Strong).unwrap_or(&0);
    let behavioral = *tier_subtotal.get(&SuspicionTier::Behavioral).unwrap_or(&0);
    // Context only corroborates existing Hard evidence, never more than Hard/2.
    // Allocate newest rule evidence first (rule code breaks timestamp ties) so
    // every point in the total reconciles to one visible event.
    context_candidates
        .sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    let mut remaining_corroboration = hard / 2;
    let mut corroboration = 0_i64;
    for (_, _, event_index, unit) in context_candidates {
        let applied = unit.min(remaining_corroboration);
        if applied == 0 {
            continue;
        }
        annotated[event_index].applied_delta = applied;
        annotated[event_index].counted = true;
        corroboration += applied;
        remaining_corroboration -= applied;
    }

    let total = hard
        .saturating_add(corroboration)
        .saturating_add(strong)
        .saturating_add(behavioral);

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

#[derive(sqlx::FromRow)]
struct StoredSuspicionEvent {
    kind: i16,
    evidence_key: String,
    score_delta: i32,
    created_at: chrono::DateTime<chrono::Utc>,
}

/// Rebuild the cached participation score from its authoritative event ledger.
///
/// The caller must pass the connection behind the transaction that inserted a
/// new event while holding that participation's write lock. Keeping the read,
/// pure aggregation, and projection update in one transaction prevents a raw
/// delta write from bypassing incident caps, tier ceilings, or Context's zero
/// direct contribution.
pub(crate) async fn recompute_participation_suspicion_score(
    connection: &mut sqlx::PgConnection,
    participation_id: i32,
) -> AppResult<i32> {
    let stored = sqlx::query_as::<_, StoredSuspicionEvent>(
        r#"SELECT kind, evidence_key, score_delta, created_at
             FROM "SuspicionEvents"
            WHERE participation_id = $1
            ORDER BY created_at DESC, id DESC"#,
    )
    .bind(participation_id)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let mut events = Vec::with_capacity(stored.len());
    for event in stored {
        let ty = SuspicionType::from_kind(event.kind).ok_or_else(|| {
            AppError::internal(format!(
                "unknown suspicion event kind {} for participation {participation_id}",
                event.kind
            ))
        })?;
        events.push(SuspicionEventRow {
            rule_code: ty.code().to_string(),
            evidence_key: event.evidence_key,
            details: ty.default_entry().1.to_string(),
            time: event.created_at,
            score_delta: Some(event.score_delta),
        });
    }

    // All events are frozen with a delta by m0091 and every current writer, so
    // canonical recomputation never re-reads mutable admin rule weights.
    let total = compute_breakdown(&events, |_| 0)
        .total
        .clamp(0, i64::from(i32::MAX));
    let total = i32::try_from(total)
        .map_err(|_| AppError::internal("canonical suspicion score exceeds i32"))?;

    sqlx::query_scalar::<_, i32>(
        r#"UPDATE "Participations"
              SET suspicion_score = $2
            WHERE id = $1
        RETURNING suspicion_score"#,
    )
    .bind(participation_id)
    .bind(total)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("participation not found"))
}

#[cfg(test)]
mod tests {
    use super::{compute_breakdown, RiskBand, SuspicionEventRow, DEFAULTS};
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
                score_delta: Some(75),
            },
            SuspicionEventRow {
                rule_code: "StolenFlag".to_string(),
                evidence_key: "legacy:8".to_string(),
                details: "Flag stolen from another team".to_string(),
                time: now,
                score_delta: Some(75),
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

    #[test]
    fn context_has_zero_direct_score_but_can_corroborate_hard_evidence() {
        let now = Utc::now();
        let context = SuspicionEventRow {
            rule_code: "SharedFingerprint".to_string(),
            evidence_key: "global".to_string(),
            details: String::new(),
            time: now,
            score_delta: Some(10_000),
        };

        let context_only = compute_breakdown(std::slice::from_ref(&context), |_| 10_000);
        assert_eq!(context_only.band, RiskBand::Context);
        assert_eq!(context_only.total, 0);
        assert!(!context_only.events[0].counted);

        let hard = SuspicionEventRow {
            rule_code: "StolenFlag".to_string(),
            evidence_key: "submission:1".to_string(),
            details: String::new(),
            time: now,
            score_delta: Some(100),
        };
        let corroborated = compute_breakdown(&[hard, context], |_| 10_000);
        assert_eq!(corroborated.hard, 100);
        assert_eq!(corroborated.corroboration, 20);
        assert_eq!(corroborated.total, 120);
        let context_event = corroborated
            .events
            .iter()
            .find(|event| event.rule_code == "SharedFingerprint")
            .expect("context event remains visible");
        assert!(context_event.counted);
        assert_eq!(context_event.applied_delta, 20);
        assert_eq!(
            corroborated
                .events
                .iter()
                .map(|event| event.applied_delta)
                .sum::<i64>(),
            corroborated.total
        );
    }

    #[test]
    fn corroboration_is_partially_assigned_to_the_newest_event_deterministically() {
        let now = Utc::now();
        let events = vec![
            SuspicionEventRow {
                rule_code: "SharedFingerprint".to_string(),
                evidence_key: "fingerprint:old".to_string(),
                details: String::new(),
                time: now - Duration::seconds(2),
                score_delta: Some(60),
            },
            SuspicionEventRow {
                rule_code: "CrossTeamIP".to_string(),
                evidence_key: "ip:newest".to_string(),
                details: String::new(),
                time: now,
                score_delta: Some(20),
            },
            SuspicionEventRow {
                rule_code: "SharedFingerprint".to_string(),
                evidence_key: "fingerprint:new".to_string(),
                details: String::new(),
                time: now - Duration::seconds(1),
                score_delta: Some(60),
            },
            SuspicionEventRow {
                rule_code: "StolenFlag".to_string(),
                evidence_key: "submission:1".to_string(),
                details: String::new(),
                time: now,
                score_delta: Some(25),
            },
        ];

        let breakdown = compute_breakdown(&events, |_| 0);

        assert_eq!(breakdown.hard, 25);
        assert_eq!(breakdown.corroboration, 12);
        assert_eq!(breakdown.total, 37);
        let cross_team_ip = breakdown
            .events
            .iter()
            .find(|event| event.rule_code == "CrossTeamIP")
            .expect("newest context rule remains visible");
        assert_eq!(cross_team_ip.applied_delta, 10);
        assert!(cross_team_ip.counted);
        let fingerprint_events = breakdown
            .events
            .iter()
            .filter(|event| event.rule_code == "SharedFingerprint")
            .collect::<Vec<_>>();
        assert_eq!(fingerprint_events.len(), 2);
        assert_eq!(fingerprint_events[0].applied_delta, 2);
        assert!(fingerprint_events[0].counted);
        assert_eq!(fingerprint_events[1].applied_delta, 0);
        assert!(!fingerprint_events[1].counted);
        assert_eq!(
            breakdown
                .events
                .iter()
                .map(|event| event.applied_delta)
                .sum::<i64>(),
            breakdown.total
        );
    }

    #[test]
    fn non_actionable_telemetry_events_remain_visible_without_score_or_corroboration() {
        let now = Utc::now();
        let events = [
            "NoDownload",
            "NoContainer",
            "FastSolve-Open",
            "FastSolve-Download",
            "FastSolve-Container",
            "DirectedSolving",
            "ClusteredRegistration",
            "HoneypotHit",
            "HoneypotProtocolHit",
            "HoneypotChain",
            "FlagEgress",
            "SubmitterNeverAccessedContainer",
        ]
        .into_iter()
        .enumerate()
        .map(|(incident, rule_code)| SuspicionEventRow {
            rule_code: rule_code.to_string(),
            evidence_key: format!("legacy-telemetry:{incident}"),
            details: String::new(),
            time: now,
            score_delta: Some(80),
        })
        .collect::<Vec<_>>();

        let visible = compute_breakdown(&events, |_| 80);
        assert_eq!(visible.band, RiskBand::Context);
        assert_eq!(visible.total, 0);
        assert_eq!(visible.events.len(), 12);
        assert!(visible
            .events
            .iter()
            .all(|event| !event.counted && event.applied_delta == 0));

        let mut with_hard = events;
        with_hard.push(SuspicionEventRow {
            rule_code: "StolenFlag".to_string(),
            evidence_key: "submission:1".to_string(),
            details: String::new(),
            time: now,
            score_delta: Some(100),
        });
        let no_corroboration = compute_breakdown(&with_hard, |_| 80);
        assert_eq!(no_corroboration.hard, 100);
        assert_eq!(no_corroboration.corroboration, 0);
        assert_eq!(no_corroboration.total, 100);
    }

    #[test]
    fn quarantined_pre_cutover_events_never_score_or_corroborate() {
        let now = Utc::now();
        let events = vec![
            SuspicionEventRow {
                rule_code: "StolenFlag".to_string(),
                evidence_key: "legacy-untrusted:1".to_string(),
                details: String::new(),
                time: now,
                score_delta: Some(100),
            },
            SuspicionEventRow {
                rule_code: "SharedFingerprint".to_string(),
                evidence_key: "legacy-untrusted:2".to_string(),
                details: String::new(),
                time: now,
                score_delta: Some(60),
            },
            SuspicionEventRow {
                rule_code: "StolenFlag".to_string(),
                evidence_key: "submission:3".to_string(),
                details: String::new(),
                time: now,
                score_delta: Some(100),
            },
        ];

        let breakdown = compute_breakdown(&events, |_| 10_000);
        assert_eq!(breakdown.hard, 100);
        assert_eq!(breakdown.corroboration, 0);
        assert_eq!(breakdown.total, 100);
        assert_eq!(breakdown.events.len(), 3);
        let quarantined = breakdown
            .events
            .iter()
            .filter(|event| event.score_delta == 0)
            .collect::<Vec<_>>();
        assert_eq!(quarantined.len(), 2);
        assert!(quarantined
            .iter()
            .all(|event| event.applied_delta == 0 && !event.counted));
    }

    #[test]
    fn quarantined_canonical_collisions_yield_only_post_cutover_score() {
        let now = Utc::now();
        let fixtures = [
            ("Burst", "global", 30),
            ("ZeroWrongAttempts", "challenge:20", 50),
            ("AdaptiveFastSolve", "challenge:20", 60),
            ("HighWrongRate", "challenge:20", 40),
            ("FirstBloodAnomaly", "challenge:20", 20),
        ];
        let mut events = fixtures
            .iter()
            .enumerate()
            .map(|(index, (rule_code, _, raw_delta))| SuspicionEventRow {
                rule_code: (*rule_code).to_string(),
                evidence_key: format!("legacy-untrusted:{}", index + 1),
                details: String::new(),
                time: now - Duration::seconds(1),
                score_delta: Some(*raw_delta),
            })
            .collect::<Vec<_>>();
        events.extend(
            fixtures
                .into_iter()
                .map(|(rule_code, evidence_key, score_delta)| SuspicionEventRow {
                    rule_code: rule_code.to_string(),
                    evidence_key: evidence_key.to_string(),
                    details: String::new(),
                    time: now,
                    score_delta: Some(score_delta),
                }),
        );

        let breakdown = compute_breakdown(&events, |_| 10_000);

        assert_eq!(breakdown.strong, 40);
        assert_eq!(breakdown.behavioral, 25);
        assert_eq!(breakdown.total, 65);
        assert_eq!(
            breakdown
                .events
                .iter()
                .filter(|event| event.score_delta == 0)
                .count(),
            5
        );
        assert!(breakdown
            .events
            .iter()
            .filter(|event| event.score_delta == 0)
            .all(|event| event.applied_delta == 0 && !event.counted));
        assert_eq!(
            breakdown
                .events
                .iter()
                .map(|event| event.applied_delta)
                .sum::<i64>(),
            breakdown.total
        );
    }

    #[test]
    fn incident_caps_and_tier_ceilings_apply_before_the_total() {
        let now = Utc::now();
        let hard = (0..12).map(|incident| SuspicionEventRow {
            rule_code: "StolenFlag".to_string(),
            evidence_key: format!("submission:{incident}"),
            details: String::new(),
            time: now + Duration::seconds(incident),
            score_delta: Some(10_000),
        });
        let strong = ["AutomatedPattern", "HighWrongRate"]
            .into_iter()
            .enumerate()
            .map(|(incident, rule_code)| SuspicionEventRow {
                rule_code: rule_code.to_string(),
                evidence_key: format!("strong:{incident}"),
                details: String::new(),
                time: now,
                score_delta: Some(10_000),
            });
        let behavioral =
            ["Hoarding", "FastSolve-Open"]
                .into_iter()
                .enumerate()
                .map(|(incident, rule_code)| SuspicionEventRow {
                    rule_code: rule_code.to_string(),
                    evidence_key: format!("behavioral:{incident}"),
                    details: String::new(),
                    time: now,
                    score_delta: Some(10_000),
                });
        let events = hard.chain(strong).chain(behavioral).collect::<Vec<_>>();

        let breakdown = compute_breakdown(&events, |_| 10_000);

        assert_eq!(breakdown.hard, 100_000, "StolenFlag caps at ten incidents");
        assert_eq!(breakdown.strong, 60, "Strong tier has a shared ceiling");
        assert_eq!(
            breakdown.behavioral, 25,
            "Behavioral tier has a shared ceiling"
        );
        assert_eq!(breakdown.total, 100_085);
        assert_eq!(
            breakdown
                .events
                .iter()
                .map(|event| event.applied_delta)
                .sum::<i64>(),
            breakdown.total
        );
        assert!(breakdown
            .events
            .iter()
            .any(|event| event.applied_delta == 60));
        assert_eq!(
            breakdown
                .events
                .iter()
                .filter(|event| event.rule_code == "StolenFlag" && event.counted)
                .count(),
            10
        );
    }

    #[test]
    fn bounded_rule_weights_keep_the_canonical_projection_inside_i32() {
        let now = Utc::now();
        let events = DEFAULTS
            .iter()
            .flat_map(|(ty, _, _)| {
                (0..ty.max_incidents()).map(move |incident| SuspicionEventRow {
                    rule_code: ty.code().to_string(),
                    evidence_key: format!("bounded:{}:{incident}", ty.kind()),
                    details: String::new(),
                    time: now,
                    score_delta: Some(10_000),
                })
            })
            .collect::<Vec<_>>();

        let breakdown = compute_breakdown(&events, |_| 10_000);

        assert_eq!(breakdown.hard, 380_000);
        assert_eq!(breakdown.strong, 60);
        assert_eq!(breakdown.behavioral, 25);
        assert_eq!(breakdown.corroboration, 70);
        assert_eq!(breakdown.total, 380_155);
        assert!(i32::try_from(breakdown.total).is_ok());
        assert_eq!(
            breakdown
                .events
                .iter()
                .map(|event| event.applied_delta)
                .sum::<i64>(),
            breakdown.total,
            "every canonical score point is explained by applied event deltas"
        );
    }
}
