use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::services::suspicion::{self, RiskBand, SuspicionEventRow};
use crate::utils::error::{AppError, AppResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[repr(i16)]
pub enum EvidenceFamily {
    IdentityCorrelation = 0,
    NetworkSession = 1,
    TimingCadence = 2,
    TrajectorySimilarity = 3,
    CrossTeamPossession = 4,
    TrustedProvenance = 5,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[repr(i16)]
pub enum EvidenceTier {
    Context = 0,
    Behavioral = 1,
    Strong = 2,
    Hard = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[repr(i16)]
pub enum EvidenceRelationshipKind {
    Supports = 0,
    Corroborates = 1,
    DerivedFrom = 2,
    Contradicts = 3,
    ExplainedBy = 4,
    SameSubject = 5,
    CrossTeamTransfer = 6,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[repr(i16)]
pub enum FindingReviewStatus {
    Explained = 0,
    Suspicious = 1,
    Confirmed = 2,
    Dismissed = 3,
    NeedsMoreEvidence = 4,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct FindingRow {
    pub id: i64,
    pub game_id: i32,
    pub participation_id: i32,
    pub user_id: Option<Uuid>,
    pub challenge_id: Option<i32>,
    pub detector_code: String,
    pub detector_version: i32,
    pub evidence_family: i16,
    pub evidence_tier: i16,
    pub score_delta: i32,
    pub evidence_key: String,
    pub occurred_at_utc: DateTime<Utc>,
    pub details: serde_json::Value,
    pub shadow: bool,
    pub created_at_utc: DateTime<Utc>,
    pub latest_review_status: Option<i16>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceRelationshipRow {
    pub finding_id: i64,
    pub related_finding_id: Option<i64>,
    pub relation_kind: i16,
    pub related_source_type: Option<String>,
    pub related_source_key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewFinding<'a> {
    pub game_id: i32,
    pub participation_id: i32,
    pub user_id: Option<Uuid>,
    pub challenge_id: Option<i32>,
    pub detector_code: &'a str,
    pub detector_version: i32,
    pub evidence_family: EvidenceFamily,
    pub evidence_tier: EvidenceTier,
    pub score_delta: i32,
    pub evidence_key: &'a str,
    pub occurred_at_utc: DateTime<Utc>,
    pub details: serde_json::Value,
    pub shadow: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FamilyContribution {
    pub family: EvidenceFamily,
    pub behavioral: i64,
    pub strong: i64,
    pub hard: i64,
    pub context_count: usize,
    pub existing_incidents: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FusedEvidenceBreakdown {
    pub participation_id: i32,
    pub total: i64,
    pub band: String,
    pub band_label: String,
    pub reviewer_confirmed: bool,
    pub independent_actionable_families: usize,
    pub existing_score: i64,
    pub finding_score: i64,
    pub families: Vec<FamilyContribution>,
    pub findings: Vec<FindingRow>,
    pub relationships: Vec<EvidenceRelationshipRow>,
}

impl TryFrom<i16> for EvidenceFamily {
    type Error = AppError;

    fn try_from(value: i16) -> Result<Self, Self::Error> {
        Ok(match value {
            0 => Self::IdentityCorrelation,
            1 => Self::NetworkSession,
            2 => Self::TimingCadence,
            3 => Self::TrajectorySimilarity,
            4 => Self::CrossTeamPossession,
            5 => Self::TrustedProvenance,
            _ => return Err(AppError::internal("invalid evidence family")),
        })
    }
}

impl TryFrom<i16> for EvidenceTier {
    type Error = AppError;

    fn try_from(value: i16) -> Result<Self, Self::Error> {
        Ok(match value {
            0 => Self::Context,
            1 => Self::Behavioral,
            2 => Self::Strong,
            3 => Self::Hard,
            _ => return Err(AppError::internal("invalid evidence tier")),
        })
    }
}

fn validate_finding(finding: &NewFinding<'_>) -> AppResult<()> {
    if !(1..=64).contains(&finding.detector_code.len())
        || finding.detector_version < 1
        || !(1..=160).contains(&finding.evidence_key.len())
        || !(0..=10_000).contains(&finding.score_delta)
        || finding.evidence_tier == EvidenceTier::Context && finding.score_delta != 0
        || !finding.details.is_object()
    {
        return Err(AppError::bad_request("Invalid anti-cheat finding"));
    }
    Ok(())
}

pub async fn record_finding(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    finding: &NewFinding<'_>,
) -> AppResult<Option<i64>> {
    validate_finding(finding)?;
    sqlx::query_scalar(
        r#"INSERT INTO "AntiCheatFindings"
             (game_id, participation_id, user_id, challenge_id, detector_code,
              detector_version, evidence_family, evidence_tier, score_delta,
              evidence_key, occurred_at_utc, details, shadow)
           SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13
            WHERE EXISTS (
                SELECT 1 FROM "Games" game
                 JOIN "Participations" participation
                   ON participation.game_id = game.id AND participation.id = $2
                WHERE game.id = $1
                  AND $11 >= game.start_time_utc AND $11 < game.end_time_utc
                  AND participation.competitive_admitted_at_utc IS NOT NULL
                  AND participation.competitive_admitted_at_utc < game.end_time_utc
            )
           ON CONFLICT DO NOTHING RETURNING id"#,
    )
    .bind(finding.game_id)
    .bind(finding.participation_id)
    .bind(finding.user_id)
    .bind(finding.challenge_id)
    .bind(finding.detector_code)
    .bind(finding.detector_version)
    .bind(finding.evidence_family as i16)
    .bind(finding.evidence_tier as i16)
    .bind(finding.score_delta)
    .bind(finding.evidence_key)
    .bind(finding.occurred_at_utc)
    .bind(sqlx::types::Json(&finding.details))
    .bind(finding.shadow)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

pub async fn relate_findings(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    game_id: i32,
    finding_id: i64,
    related_finding_id: Option<i64>,
    relation: EvidenceRelationshipKind,
    source: Option<(&str, &str)>,
) -> AppResult<()> {
    let (source_type, source_key) = source.unzip();
    if related_finding_id.is_some() == source_type.is_some()
        || source_type.is_some_and(|value| !(1..=48).contains(&value.len()))
        || source_key.is_some_and(|value| !(1..=160).contains(&value.len()))
    {
        return Err(AppError::bad_request("Invalid evidence relationship"));
    }
    sqlx::query(
        r#"INSERT INTO "AntiCheatEvidenceRelationships"
             (game_id, finding_id, related_finding_id, relation_kind,
              related_source_type, related_source_key)
           VALUES ($1, $2, $3, $4, $5, $6)
           ON CONFLICT DO NOTHING"#,
    )
    .bind(game_id)
    .bind(finding_id)
    .bind(related_finding_id)
    .bind(relation as i16)
    .bind(source_type)
    .bind(source_key)
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

pub async fn review_finding(
    st: &SharedState,
    game_id: i32,
    finding_id: i64,
    reviewer: Uuid,
    status: FindingReviewStatus,
    note: Option<&str>,
) -> AppResult<()> {
    if note.is_some_and(|value| value.len() > 4_000) {
        return Err(AppError::bad_request("Review note is too long"));
    }
    let inserted = sqlx::query(
        r#"INSERT INTO "AntiCheatFindingReviews"
             (finding_id, game_id, status, reviewed_by_user_id, note)
           SELECT finding.id, finding.game_id, $3, $4, $5
             FROM "AntiCheatFindings" finding
            WHERE finding.id = $1 AND finding.game_id = $2"#,
    )
    .bind(finding_id)
    .bind(game_id)
    .bind(status as i16)
    .bind(reviewer)
    .bind(note)
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if inserted.rows_affected() != 1 {
        return Err(AppError::not_found("Finding not found"));
    }
    Ok(())
}

fn score_findings(findings: &[FindingRow]) -> AppResult<(i64, Vec<FamilyContribution>, usize)> {
    let mut families = BTreeMap::<EvidenceFamily, FamilyContribution>::new();
    for finding in findings {
        let family = EvidenceFamily::try_from(finding.evidence_family)?;
        let tier = EvidenceTier::try_from(finding.evidence_tier)?;
        let contribution = families.entry(family).or_insert(FamilyContribution {
            family,
            behavioral: 0,
            strong: 0,
            hard: 0,
            context_count: 0,
            existing_incidents: 0,
        });
        if tier == EvidenceTier::Context {
            contribution.context_count += 1;
            continue;
        }
        if finding.shadow
            || matches!(
                finding.latest_review_status,
                Some(value)
                    if value == FindingReviewStatus::Explained as i16
                        || value == FindingReviewStatus::Dismissed as i16
            )
        {
            continue;
        }
        match tier {
            EvidenceTier::Context => unreachable!("context findings returned above"),
            EvidenceTier::Behavioral => {
                contribution.behavioral =
                    (contribution.behavioral + i64::from(finding.score_delta)).min(15)
            }
            EvidenceTier::Strong => {
                contribution.strong = (contribution.strong + i64::from(finding.score_delta)).min(30)
            }
            EvidenceTier::Hard => {
                contribution.hard += i64::from(finding.score_delta);
            }
        }
    }
    let actionable_families = families
        .values()
        .filter(|family| family.behavioral > 0 || family.strong > 0 || family.hard > 0)
        .count();
    let behavioral = families
        .values()
        .map(|family| family.behavioral)
        .sum::<i64>()
        .min(25);
    let strong = families
        .values()
        .map(|family| family.strong)
        .sum::<i64>()
        .min(60);
    let hard = families.values().map(|family| family.hard).sum::<i64>();
    Ok((
        hard + strong + behavioral,
        families.into_values().collect(),
        actionable_families,
    ))
}

fn existing_family(rule: suspicion::SuspicionType) -> EvidenceFamily {
    use suspicion::SuspicionType::*;
    match rule {
        SharedIp
        | SharedFingerprint
        | FingerprintChurn
        | IpChurn
        | UnknownIp
        | CrossTeamIp
        | ClusteredRegistration
        | SubnetOverlap => EvidenceFamily::IdentityCorrelation,
        SessionConcurrency
        | NoDownload
        | NoContainer
        | SubmitterNeverAccessedContainer
        | AccessIpMismatchAtSubmission => EvidenceFamily::NetworkSession,
        Burst
        | FastSolveOpen
        | FastSolveDownload
        | FastSolveContainer
        | HighWrongRate
        | AutomatedPattern
        | DelayedSolveSubmission
        | InstantSubmitAfterAccess => EvidenceFamily::TimingCadence,
        SequenceSimilarity | CollusionGroup | ZeroWrongAttempts | SolutionRelay
        | AdaptiveFastSolve | DirectedSolving | FirstBloodAnomaly | Hoarding => {
            EvidenceFamily::TrajectorySimilarity
        }
        StolenFlag | WrongFlagLeakage | FlagEgress | CrossTeamContainerAccess => {
            EvidenceFamily::CrossTeamPossession
        }
        TokenAbuse | HoneypotCanaryFlag | HoneypotHit | HoneypotProtocolHit | HoneypotChain => {
            EvidenceFamily::TrustedProvenance
        }
    }
}

pub async fn fused_breakdown(
    st: &SharedState,
    game_id: i32,
    participation_id: i32,
) -> AppResult<FusedEvidenceBreakdown> {
    let old_raw = sqlx::query_as::<_, (i16, String, DateTime<Utc>, Option<i32>)>(
        r#"SELECT event.kind, event.evidence_key,
                  event.created_at, event.score_delta
             FROM "SuspicionEvents" event
            WHERE event.game_id = $1 AND event.participation_id = $2
            ORDER BY event.created_at, event.id"#,
    )
    .bind(game_id)
    .bind(participation_id)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let old_rows = old_raw
        .iter()
        .map(|(kind, evidence_key, created_at, score_delta)| {
            let rule = suspicion::SuspicionType::from_kind(*kind);
            SuspicionEventRow {
                rule_code: rule
                    .map(|rule| rule.code().to_string())
                    .unwrap_or_else(|| format!("UnknownKind:{kind}")),
                evidence_key: evidence_key.clone(),
                details: rule
                    .map(|rule| rule.default_entry().1.to_string())
                    .unwrap_or_else(|| "Unknown suspicion incident".to_string()),
                time: *created_at,
                score_delta: *score_delta,
            }
        })
        .collect::<Vec<_>>();
    let existing = suspicion::compute_breakdown(&old_rows, suspicion::default_weight);
    let findings = sqlx::query_as::<_, FindingRow>(
        r#"SELECT id, game_id, participation_id, user_id, challenge_id,
                  detector_code, detector_version, evidence_family, evidence_tier,
                  score_delta, evidence_key, occurred_at_utc, details, shadow,
                  created_at_utc, latest.status AS latest_review_status
             FROM "AntiCheatFindings" finding
             LEFT JOIN LATERAL (
                 SELECT review.status FROM "AntiCheatFindingReviews" review
                  WHERE review.finding_id = finding.id
                  ORDER BY review.created_at_utc DESC, review.id DESC LIMIT 1
             ) latest ON TRUE
            WHERE finding.game_id = $1 AND finding.participation_id = $2
            ORDER BY finding.occurred_at_utc, finding.id"#,
    )
    .bind(game_id)
    .bind(participation_id)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let (finding_score, mut families, new_actionable_families) = score_findings(&findings)?;
    let reviewer_confirmed = findings
        .iter()
        .any(|finding| finding.latest_review_status == Some(FindingReviewStatus::Confirmed as i16));
    let relationships = sqlx::query_as::<_, EvidenceRelationshipRow>(
        r#"SELECT relation.finding_id, relation.related_finding_id,
                  relation.relation_kind, relation.related_source_type,
                  relation.related_source_key
             FROM "AntiCheatEvidenceRelationships" relation
             JOIN "AntiCheatFindings" finding ON finding.id = relation.finding_id
            WHERE finding.game_id = $1 AND finding.participation_id = $2
            ORDER BY relation.finding_id, relation.id"#,
    )
    .bind(game_id)
    .bind(participation_id)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let mut existing_actionable = std::collections::BTreeSet::new();
    for row in &old_raw {
        let Some(rule) = suspicion::SuspicionType::from_kind(row.0) else {
            continue;
        };
        let family = existing_family(rule);
        let contribution = if let Some(found) = families.iter_mut().find(|row| row.family == family)
        {
            found
        } else {
            families.push(FamilyContribution {
                family,
                behavioral: 0,
                strong: 0,
                hard: 0,
                context_count: 0,
                existing_incidents: 0,
            });
            families.last_mut().expect("family was inserted")
        };
        contribution.existing_incidents += 1;
        if rule.tier() != suspicion::SuspicionTier::Context
            && row.3.unwrap_or_else(|| rule.default_entry().0) > 0
        {
            existing_actionable.insert(family);
        }
    }
    families.sort_by_key(|family| family.family);
    let actionable_families = existing_actionable
        .into_iter()
        .chain(
            families
                .iter()
                .filter(|family| family.behavioral > 0 || family.strong > 0 || family.hard > 0)
                .map(|family| family.family),
        )
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        .max(new_actionable_families);
    let has_context = findings.iter().any(|finding| finding.evidence_tier == 0);
    let has_new_hard = families.iter().any(|family| family.hard > 0);
    let has_new_strong = families.iter().any(|family| family.strong > 0);
    let band = if existing.band == RiskBand::Evidenced || has_new_hard {
        RiskBand::Evidenced
    } else if existing.band == RiskBand::Investigate || has_new_strong || actionable_families >= 2 {
        RiskBand::Investigate
    } else if existing.band == RiskBand::Watch || finding_score > 0 {
        RiskBand::Watch
    } else if existing.band == RiskBand::Context || has_context {
        RiskBand::Context
    } else {
        RiskBand::Clean
    };
    Ok(FusedEvidenceBreakdown {
        participation_id,
        total: existing.total + finding_score,
        band: band.band_key().to_string(),
        band_label: match band {
            RiskBand::Clean => "No evidence observed",
            RiskBand::Context => "Context only",
            RiskBand::Watch => "Watch",
            RiskBand::Investigate => "Investigate",
            RiskBand::Evidenced => "Verified evidence",
        }
        .to_string(),
        reviewer_confirmed,
        independent_actionable_families: actionable_families,
        existing_score: existing.total,
        finding_score,
        families,
        findings,
        relationships,
    })
}

pub async fn derive_context_findings(st: &SharedState, game_id: i32) -> AppResult<usize> {
    let mut transaction = st
        .pg()
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let inserted: i64 = sqlx::query_scalar(
        r#"WITH candidates AS (
               SELECT dns.game_id, dns.participation_id, dns.user_id,
                      'AiProviderDns'::text AS detector_code,
                      1::integer AS detector_version,
                      1::smallint AS evidence_family,
                      0::smallint AS evidence_tier,
                      0::integer AS score_delta,
                      'dns:' || dns.id::text AS evidence_key,
                      dns.first_seen_at_utc AS occurred_at_utc,
                      jsonb_build_object(
                          'providerCategory', dns.provider_category,
                          'queryCount', dns.query_count,
                          'meaning', 'network context only; not proof of AI use'
                      ) AS details
                 FROM "VpnDnsProviderBuckets" dns WHERE dns.game_id = $1
               UNION ALL
               SELECT network.game_id, network.participation_id, network.user_id,
                      'HostingNetworkSource', 1, 1, 0, 0,
                      'network:' || network.id::text, network.first_seen_at_utc,
                      jsonb_build_object(
                          'networkClass', network.network_class,
                          'sourceAsn', network.source_asn,
                          'meaning', 'network context only; shared/VPS networks are not proof'
                      )
                 FROM "VpnPeerNetworkObservations" network
                WHERE network.game_id = $1 AND network.network_class <> 0
               UNION ALL
               SELECT flag.game_id, flag.receiving_participation_id,
                      flag.receiving_user_id, 'ForeignFlagTransport', 1, 4, 0, 0,
                      'flag-transport:' || flag.id::text, flag.observed_at_utc,
                      jsonb_build_object(
                          'challengeId', flag.challenge_id,
                          'owningParticipationId', flag.owning_participation_id,
                          'transport', flag.transport,
                          'meaning', 'exact foreign flag bytes crossed the VPN; framing is not proven'
                      )
                 FROM "VpnFlagTransportEvents" flag WHERE flag.game_id = $1
           ), inserted AS (
               INSERT INTO "AntiCheatFindings"
                 (game_id, participation_id, user_id, detector_code, detector_version,
                  evidence_family, evidence_tier, score_delta, evidence_key,
                  occurred_at_utc, details, shadow)
               SELECT candidate.game_id, candidate.participation_id, candidate.user_id,
                      candidate.detector_code, candidate.detector_version,
                      candidate.evidence_family, candidate.evidence_tier,
                      candidate.score_delta, candidate.evidence_key,
                      candidate.occurred_at_utc, candidate.details, TRUE
                 FROM candidates candidate
                 JOIN "Participations" participation
                   ON participation.game_id = candidate.game_id
                  AND participation.id = candidate.participation_id
                WHERE participation.competitive_admitted_at_utc IS NOT NULL
               ON CONFLICT DO NOTHING RETURNING 1
           ) SELECT COUNT(*)::bigint FROM inserted"#,
    )
    .bind(game_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    // One peer observed from several endpoint identities is context only. It is
    // intentionally shadowed and cannot corroborate other evidence.
    let sharing: i64 = sqlx::query_scalar(
        r#"WITH candidates AS (
               SELECT observation.game_id, observation.participation_id,
                      observation.user_id, observation.peer_id,
                      MIN(observation.first_seen_at_utc) AS occurred_at_utc,
                      COUNT(DISTINCT observation.endpoint_hash)::integer AS endpoints
                 FROM "VpnPeerNetworkObservations" observation
                WHERE observation.game_id = $1
                GROUP BY observation.game_id, observation.participation_id,
                         observation.user_id, observation.peer_id
               HAVING COUNT(DISTINCT observation.endpoint_hash) > 1
           ), inserted AS (
               INSERT INTO "AntiCheatFindings"
                 (game_id, participation_id, user_id, detector_code, detector_version,
                  evidence_family, evidence_tier, score_delta, evidence_key,
                  occurred_at_utc, details, shadow)
               SELECT game_id, participation_id, user_id,
                      'VpnPeerDeviceSharing', 1, 1, 0, 0,
                      'peer:' || peer_id::text, occurred_at_utc,
                      jsonb_build_object(
                          'endpointCount', endpoints,
                          'meaning', 'one event VPN profile appeared from multiple endpoints; context only'
                      ), TRUE
                 FROM candidates
               ON CONFLICT DO NOTHING RETURNING 1
           ) SELECT COUNT(*)::bigint FROM inserted"#,
    )
    .bind(game_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    // Preserve the provenance graph independently from the detector output.
    // Relationships never add points; they explain which bounded source row a
    // finding came from and whether it corroborates already-canonical proof.
    sqlx::query(
        r#"INSERT INTO "AntiCheatEvidenceRelationships"
             (game_id, finding_id, relation_kind,
              related_source_type, related_source_key)
           SELECT finding.game_id, finding.id, $2,
                  CASE finding.detector_code
                    WHEN 'AiProviderDns' THEN 'VpnDnsProviderBucket'
                    WHEN 'HostingNetworkSource' THEN 'VpnPeerNetworkObservation'
                    WHEN 'ForeignFlagTransport' THEN 'VpnFlagTransportEvent'
                    WHEN 'VpnPeerDeviceSharing' THEN 'VpnPeerNetworkObservation'
                  END,
                  finding.evidence_key
             FROM "AntiCheatFindings" finding
            WHERE finding.game_id = $1
              AND finding.detector_code IN (
                    'AiProviderDns', 'HostingNetworkSource',
                    'ForeignFlagTransport', 'VpnPeerDeviceSharing'
              )
           ON CONFLICT DO NOTHING"#,
    )
    .bind(game_id)
    .bind(EvidenceRelationshipKind::DerivedFrom as i16)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    // An observed foreign flag crossing the event VPN is contextual on its
    // own. If the same receiver later submits that exact owner's challenge
    // flag, link it to the immutable StolenFlag incident without duplicating
    // or increasing the canonical hard score.
    sqlx::query(
        r#"INSERT INTO "AntiCheatEvidenceRelationships"
             (game_id, finding_id, relation_kind,
              related_source_type, related_source_key)
           SELECT finding.game_id, finding.id, $2,
                  'SuspicionEvent', 'event:' || event.id::text
             FROM "AntiCheatFindings" finding
             JOIN "VpnFlagTransportEvents" transport
               ON finding.detector_code = 'ForeignFlagTransport'
              AND finding.evidence_key = 'flag-transport:' || transport.id::text
             JOIN "CheatInfo" cheat
               ON cheat.game_id = transport.game_id
              AND cheat.challenge_id = transport.challenge_id
              AND cheat.submit_participation_id = transport.receiving_participation_id
              AND cheat.source_participation_id = transport.owning_participation_id
              AND cheat.observed_at_utc >= transport.observed_at_utc
             JOIN "SuspicionEvents" event
               ON event.game_id = cheat.game_id
              AND event.participation_id = cheat.submit_participation_id
              AND event.kind = 0
              AND event.evidence_key = cheat.evidence_key
            WHERE finding.game_id = $1
           ON CONFLICT DO NOTHING"#,
    )
    .bind(game_id)
    .bind(EvidenceRelationshipKind::Supports as i16)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(usize::try_from(inserted + sharing).unwrap_or(usize::MAX))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn row(family: EvidenceFamily, tier: EvidenceTier, score: i32, shadow: bool) -> FindingRow {
        FindingRow {
            id: 1,
            game_id: 1,
            participation_id: 1,
            user_id: None,
            challenge_id: None,
            detector_code: "test".to_string(),
            detector_version: 1,
            evidence_family: family as i16,
            evidence_tier: tier as i16,
            score_delta: score,
            evidence_key: "test".to_string(),
            occurred_at_utc: Utc::now(),
            details: serde_json::json!({}),
            shadow,
            created_at_utc: Utc::now(),
            latest_review_status: None,
        }
    }

    #[test]
    fn same_family_is_capped_and_shadow_or_context_never_scores() {
        let rows = vec![
            row(
                EvidenceFamily::NetworkSession,
                EvidenceTier::Strong,
                25,
                false,
            ),
            row(
                EvidenceFamily::NetworkSession,
                EvidenceTier::Strong,
                25,
                false,
            ),
            row(
                EvidenceFamily::NetworkSession,
                EvidenceTier::Context,
                100,
                false,
            ),
            row(
                EvidenceFamily::TrustedProvenance,
                EvidenceTier::Hard,
                100,
                true,
            ),
        ];
        let (score, families, actionable) = score_findings(&rows).unwrap();
        assert_eq!(score, 30);
        assert_eq!(families[0].strong, 30);
        assert_eq!(families[0].context_count, 1);
        assert_eq!(actionable, 1);
    }

    #[test]
    fn independent_families_corroborate_but_reviews_can_suppress_points() {
        let mut network = row(
            EvidenceFamily::NetworkSession,
            EvidenceTier::Behavioral,
            12,
            false,
        );
        network.latest_review_status = Some(FindingReviewStatus::Explained as i16);
        let rows = vec![
            network,
            row(
                EvidenceFamily::TimingCadence,
                EvidenceTier::Behavioral,
                12,
                false,
            ),
            row(
                EvidenceFamily::TrajectorySimilarity,
                EvidenceTier::Strong,
                20,
                false,
            ),
        ];
        let (score, _, actionable) = score_findings(&rows).unwrap();
        assert_eq!(score, 32);
        assert_eq!(actionable, 2);
    }

    #[test]
    fn context_finding_validation_forbids_points() {
        let finding = NewFinding {
            game_id: 1,
            participation_id: 1,
            user_id: None,
            challenge_id: None,
            detector_code: "AiProviderDns",
            detector_version: 1,
            evidence_family: EvidenceFamily::NetworkSession,
            evidence_tier: EvidenceTier::Context,
            score_delta: 1,
            evidence_key: "dns:1",
            occurred_at_utc: Utc::now(),
            details: serde_json::json!({}),
            shadow: true,
        };
        assert!(validate_finding(&finding).is_err());
    }

    #[test]
    fn evidence_family_names_are_stable_strings() {
        let families = BTreeSet::from([
            EvidenceFamily::IdentityCorrelation,
            EvidenceFamily::NetworkSession,
            EvidenceFamily::TimingCadence,
            EvidenceFamily::TrajectorySimilarity,
            EvidenceFamily::CrossTeamPossession,
            EvidenceFamily::TrustedProvenance,
        ]);
        assert_eq!(families.len(), 6);
        assert_eq!(
            serde_json::to_string(&EvidenceFamily::CrossTeamPossession).unwrap(),
            r#""crossTeamPossession""#
        );
    }
}
