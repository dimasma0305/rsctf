//! Lazy, source-backed review for one persisted suspicion event.
//!
//! The main cheat report is polled, so it intentionally carries only compact
//! scoring rows. This endpoint is opened by a monitor for one event and joins
//! the immutable source ledgers needed to independently review that finding.
//! Raw flags, IP addresses, and browser fingerprints are never returned.

use super::*;

use crate::services::suspicion::{SuspicionTier, SuspicionType};

#[path = "cheat_evidence_sources.rs"]
mod sources;

#[derive(Copy, Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum EvidenceAssessment {
    DirectEvidence,
    StrongIndicator,
    BehavioralIndicator,
    ContextOnly,
}

#[derive(Copy, Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum EvidenceSourceStatus {
    Verified,
    Supporting,
    Synthetic,
    Unavailable,
    Quarantined,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceFact {
    pub label: String,
    pub value: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceSourceReview {
    pub source_type: String,
    pub title: String,
    pub source_id: Option<String>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub recorded_at: Option<DateTime<Utc>>,
    pub immutable: bool,
    pub summary: String,
    pub facts: Vec<EvidenceFact>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SuspicionEvidenceReview {
    pub event_id: i32,
    pub detector_code: String,
    pub assessment: EvidenceAssessment,
    pub source_status: EvidenceSourceStatus,
    pub is_direct_proof: bool,
    pub summary: String,
    pub explanation: String,
    pub evidence_key: String,
    #[serde(with = "crate::utils::datetime::millis")]
    pub observed_at: DateTime<Utc>,
    pub score_delta: i32,
    pub team_id: i32,
    pub team_name: String,
    pub participation_id: i32,
    pub challenge_id: Option<i32>,
    pub challenge_title: Option<String>,
    pub sources: Vec<EvidenceSourceReview>,
    pub limitations: Vec<String>,
    pub review_guidance: Vec<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct EventEvidenceRow {
    event_id: i32,
    game_id: i32,
    participation_id: i32,
    challenge_id: Option<i32>,
    kind: i16,
    evidence_key: String,
    score_delta: i32,
    created_at: DateTime<Utc>,
    team_id: i32,
    team_name: String,
    challenge_title: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct StolenFlagSourceRow {
    cheat_id: i32,
    submission_id: i32,
    source_participation_id: i32,
    source_team_name: String,
    submit_participation_id: i32,
    submit_team_name: String,
    submit_user_name: Option<String>,
    challenge_title: String,
    observed_at: DateTime<Utc>,
    evidence_version: i16,
}

#[derive(Debug, sqlx::FromRow)]
struct CrossTeamAccessSourceRow {
    access_id: i32,
    job_id: i64,
    container_id: Uuid,
    accessing_user_name: Option<String>,
    accessing_participation_id: i32,
    accessing_team_name: String,
    owner_participation_id: i32,
    owner_team_name: String,
    challenge_title: String,
    connected_at: DateTime<Utc>,
    remote_ip_hash: Option<Vec<u8>>,
    completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, sqlx::FromRow)]
struct SubmissionSourceRow {
    submission_id: i32,
    challenge_title: String,
    submitter_name: Option<String>,
    status: i16,
    submitted_at: DateTime<Utc>,
    remote_ip_hash: Option<Vec<u8>>,
    container_id: Option<Uuid>,
    container_last_operation: Option<DateTime<Utc>>,
    container_was_loaded: Option<bool>,
    first_open_at: Option<DateTime<Utc>>,
    first_download_at: Option<DateTime<Utc>>,
    first_container_start_at: Option<DateTime<Utc>>,
}

#[derive(Debug, sqlx::FromRow)]
struct SolveSourceRow {
    submission_id: i32,
    participation_id: i32,
    team_name: String,
    challenge_title: String,
    submitted_at: DateTime<Utc>,
    game_start: DateTime<Utc>,
    wrong_before: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct IdentitySourceRow {
    user_id: Uuid,
    user_name: String,
    team_name: String,
    kind: String,
    value_hint: String,
    source: String,
    observed_at: DateTime<Utc>,
}

fn fact(label: impl Into<String>, value: impl Into<String>) -> EvidenceFact {
    EvidenceFact {
        label: label.into(),
        value: value.into(),
    }
}

fn format_time(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn duration_text(milliseconds: i64) -> String {
    let seconds = milliseconds as f64 / 1_000.0;
    if seconds.abs() < 120.0 {
        format!("{seconds:.3} seconds")
    } else {
        format!("{:.2} minutes", seconds / 60.0)
    }
}

fn hash_hint(value: Option<&[u8]>) -> String {
    value
        .filter(|value| !value.is_empty())
        .map(|value| format!("{}…", hex::encode(&value[..value.len().min(6)])))
        .unwrap_or_else(|| "not captured".to_string())
}

fn assessment_for(tier: SuspicionTier) -> EvidenceAssessment {
    match tier {
        SuspicionTier::Hard => EvidenceAssessment::DirectEvidence,
        SuspicionTier::Strong => EvidenceAssessment::StrongIndicator,
        SuspicionTier::Behavioral => EvidenceAssessment::BehavioralIndicator,
        SuspicionTier::Context => EvidenceAssessment::ContextOnly,
    }
}

fn assessment_explanation(assessment: EvidenceAssessment) -> &'static str {
    match assessment {
        EvidenceAssessment::DirectEvidence => {
            "This detector represents a concrete cross-team action. It is direct proof only when the exact immutable source record below is verified."
        }
        EvidenceAssessment::StrongIndicator => {
            "This is a strong automation or relay indicator, not proof of cheating by itself. Review the measurements and surrounding activity before acting."
        }
        EvidenceAssessment::BehavioralIndicator => {
            "This is a statistical or timing anomaly. Legitimate behavior can produce it, so it must be reviewed with other independent evidence."
        }
        EvidenceAssessment::ContextOnly => {
            "This is non-actionable context. It contributes no direct score and cannot establish cheating without separate hard evidence."
        }
    }
}

fn default_limitations(assessment: EvidenceAssessment) -> Vec<String> {
    match assessment {
        EvidenceAssessment::DirectEvidence => vec![
            "Raw flags and raw network addresses are deliberately redacted from this response."
                .to_string(),
            "Confirm the source and destination identities before applying a sanction.".to_string(),
        ],
        EvidenceAssessment::StrongIndicator => vec![
            "Automation-like behavior can have legitimate explanations such as scripts explicitly allowed by event rules or accessibility tooling."
                .to_string(),
            "Do not treat this signal alone as conclusive proof.".to_string(),
        ],
        EvidenceAssessment::BehavioralIndicator => vec![
            "Timing and similarity are probabilistic indicators and can coincide naturally."
                .to_string(),
            "Require independent corroboration before taking punitive action.".to_string(),
        ],
        EvidenceAssessment::ContextOnly => vec![
            "Shared networks, devices, and account movement are common in schools, offices, and public venues."
                .to_string(),
            "This event is retained for investigation context only and is not proof.".to_string(),
        ],
    }
}

fn base_review(event: &EventEvidenceRow, ty: SuspicionType) -> SuspicionEvidenceReview {
    let assessment = assessment_for(ty.tier());
    SuspicionEvidenceReview {
        event_id: event.event_id,
        detector_code: ty.code().to_string(),
        assessment,
        source_status: EvidenceSourceStatus::Unavailable,
        is_direct_proof: false,
        summary: ty.default_entry().1.to_string(),
        explanation: assessment_explanation(assessment).to_string(),
        evidence_key: event.evidence_key.clone(),
        observed_at: event.created_at,
        score_delta: event.score_delta,
        team_id: event.team_id,
        team_name: event.team_name.clone(),
        participation_id: event.participation_id,
        challenge_id: event.challenge_id,
        challenge_title: event.challenge_title.clone(),
        sources: vec![EvidenceSourceReview {
            source_type: "suspicionEvent".to_string(),
            title: "Immutable detector event".to_string(),
            source_id: Some(format!("event:{}", event.event_id)),
            recorded_at: Some(event.created_at),
            immutable: true,
            summary: "The detector decision, evidence identity, weight, and observation time are append-only."
                .to_string(),
            facts: vec![
                fact("Detector", ty.code()),
                fact("Participation", event.participation_id.to_string()),
                fact("Team", &event.team_name),
                fact("Evidence identity", &event.evidence_key),
                fact("Frozen rule weight", event.score_delta.to_string()),
            ],
        }],
        limitations: default_limitations(assessment),
        review_guidance: vec![
            "Compare the source timestamp and identities with the event timeline.".to_string(),
            "Ask the team for an explanation and preserve this evidence export before changing participation status."
                .to_string(),
        ],
    }
}

fn mark_supporting(review: &mut SuspicionEvidenceReview) {
    if review.source_status == EvidenceSourceStatus::Unavailable {
        review.source_status = EvidenceSourceStatus::Supporting;
    }
}

async fn load_event(
    pool: &sqlx::PgPool,
    game_id: i32,
    event_id: i32,
) -> AppResult<EventEvidenceRow> {
    sqlx::query_as::<_, EventEvidenceRow>(
        r#"SELECT event.id AS event_id,
                  event.game_id,
                  event.participation_id,
                  event.challenge_id,
                  event.kind,
                  event.evidence_key,
                  COALESCE(event.score_delta, 0) AS score_delta,
                  event.created_at,
                  participation.team_id,
                  team.name AS team_name,
                  challenge.title AS challenge_title
             FROM "SuspicionEvents" event
             JOIN "Participations" participation
               ON participation.id = event.participation_id
              AND participation.game_id = event.game_id
             JOIN "Teams" team ON team.id = participation.team_id
        LEFT JOIN "GameChallenges" challenge
               ON challenge.id = event.challenge_id
              AND challenge.game_id = event.game_id
            WHERE event.game_id = $1 AND event.id = $2"#,
    )
    .bind(game_id)
    .bind(event_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("suspicion event not found"))
}

async fn add_stolen_flag_source(
    pool: &sqlx::PgPool,
    event: &EventEvidenceRow,
    review: &mut SuspicionEvidenceReview,
) -> AppResult<()> {
    let Some(submission_id) = parse_i32_key(&event.evidence_key, "submission:") else {
        return Ok(());
    };
    let row = sqlx::query_as::<_, StolenFlagSourceRow>(
        r#"SELECT cheat.id AS cheat_id,
                  cheat.submission_id,
                  cheat.source_participation_id,
                  COALESCE(NULLIF(cheat.evidence_payload->>'sourceTeamName', ''), source_team.name)
                      AS source_team_name,
                  cheat.submit_participation_id,
                  COALESCE(NULLIF(cheat.evidence_payload->>'submitTeamName', ''), submit_team.name)
                      AS submit_team_name,
                  NULLIF(cheat.evidence_payload->>'submitUserName', '') AS submit_user_name,
                  COALESCE(NULLIF(cheat.evidence_payload->>'challengeTitle', ''), challenge.title)
                      AS challenge_title,
                  cheat.observed_at_utc AS observed_at,
                  cheat.evidence_version
             FROM "CheatInfo" cheat
             JOIN "Participations" source_participation
               ON source_participation.id = cheat.source_participation_id
              AND source_participation.game_id = cheat.game_id
             JOIN "Teams" source_team ON source_team.id = source_participation.team_id
             JOIN "Participations" submit_participation
               ON submit_participation.id = cheat.submit_participation_id
              AND submit_participation.game_id = cheat.game_id
             JOIN "Teams" submit_team ON submit_team.id = submit_participation.team_id
             JOIN "GameChallenges" challenge
               ON challenge.id = cheat.challenge_id
              AND challenge.game_id = cheat.game_id
            WHERE cheat.game_id = $1
              AND cheat.submission_id = $2
              AND cheat.submit_participation_id = $3
              AND cheat.challenge_id = $4
              AND cheat.evidence_key = $5
              AND cheat.observed_at_utc = $6"#,
    )
    .bind(event.game_id)
    .bind(submission_id)
    .bind(event.participation_id)
    .bind(event.challenge_id)
    .bind(&event.evidence_key)
    .bind(event.created_at)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some(row) = row else {
        return Ok(());
    };

    review.source_status = EvidenceSourceStatus::Verified;
    review.is_direct_proof = true;
    review.sources.push(EvidenceSourceReview {
        source_type: "cheatInfo".to_string(),
        title: "Canonical foreign-flag ownership record".to_string(),
        source_id: Some(format!("cheatInfo:{}", row.cheat_id)),
        recorded_at: Some(row.observed_at),
        immutable: true,
        summary: "The grading transaction matched the submitted dynamic flag to a different participation and froze both identities. The flag value is redacted."
            .to_string(),
        facts: vec![
            fact("Submission", format!("#{}", row.submission_id)),
            fact(
                "Submitting team",
                format!("{} (participation {})", row.submit_team_name, row.submit_participation_id),
            ),
            fact(
                "Flag owner",
                format!("{} (participation {})", row.source_team_name, row.source_participation_id),
            ),
            fact(
                "Submitting user",
                row.submit_user_name.unwrap_or_else(|| "not captured".to_string()),
            ),
            fact("Challenge", row.challenge_title),
            fact("Evidence schema", format!("v{}", row.evidence_version)),
        ],
    });
    Ok(())
}

fn parse_i32_key(value: &str, prefix: &str) -> Option<i32> {
    value.strip_prefix(prefix)?.parse().ok()
}

fn parse_uuid_user_key(value: &str, prefix: &str) -> Option<Uuid> {
    Uuid::parse_str(value.strip_prefix(prefix)?.strip_prefix("user:")?).ok()
}

fn parse_hash_key(value: &str, prefix: &str) -> Option<Vec<u8>> {
    let encoded = value.strip_prefix(prefix)?;
    let decoded = hex::decode(encoded).ok()?;
    (decoded.len() == 32).then_some(decoded)
}

/// `GET /api/game/{id}/cheatreport/events/{eventId}` — requires Monitor.
///
/// This is deliberately separate from the polled report. It performs the
/// heavier source joins only when an administrator opens one incident.
pub async fn suspicion_event_evidence(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path((game_id, event_id)): Path<(i32, i32)>,
) -> AppResult<RequestResponse<SuspicionEvidenceReview>> {
    let _ = load_game(&st, game_id).await?;
    let event = load_event(st.pg(), game_id, event_id).await?;
    let ty = SuspicionType::from_kind(event.kind)
        .ok_or_else(|| AppError::internal("unsupported suspicion event kind"))?;
    let mut review = base_review(&event, ty);

    if event.evidence_key.starts_with("legacy-untrusted:") {
        review.source_status = EvidenceSourceStatus::Quarantined;
        review.is_direct_proof = false;
        review.limitations.insert(
            0,
            "This pre-cutover event was quarantined and contributes no score; its original source contract cannot be trusted."
                .to_string(),
        );
        return Ok(RequestResponse::ok(review));
    }
    if event.evidence_key.starts_with("demo:synthetic:") {
        review.source_status = EvidenceSourceStatus::Synthetic;
        review.is_direct_proof = false;
        sources::add_synthetic_preview(&event, ty, &mut review);
        review.limitations.insert(
            0,
            "This is a synthetic showcase fixture. It demonstrates the detector UI but does not represent real participant behavior."
                .to_string(),
        );
        return Ok(RequestResponse::ok(review));
    }

    match ty {
        SuspicionType::StolenFlag => {
            add_stolen_flag_source(st.pg(), &event, &mut review).await?;
        }
        SuspicionType::CrossTeamContainerAccess => {
            sources::add_cross_team_access_source(st.pg(), &event, &mut review).await?;
        }
        SuspicionType::SharedIp
        | SuspicionType::SharedFingerprint
        | SuspicionType::FingerprintChurn
        | SuspicionType::IpChurn
        | SuspicionType::CrossTeamIp
        | SuspicionType::SubnetOverlap
        | SuspicionType::SessionConcurrency => {
            sources::add_identity_source(st.pg(), &event, ty, &mut review).await?;
        }
        SuspicionType::SequenceSimilarity | SuspicionType::SolutionRelay => {
            sources::add_pair_source(st.pg(), &event, ty, &mut review).await?;
        }
        SuspicionType::Burst => {
            sources::add_burst_source(st.pg(), &event, &mut review).await?;
        }
        _ => {
            sources::add_submission_source(st.pg(), &event, &mut review).await?;
            sources::add_challenge_source(st.pg(), &event, ty, &mut review).await?;
        }
    }

    if review.source_status == EvidenceSourceStatus::Unavailable {
        review.limitations.insert(
            0,
            "No canonical source row could be matched to this event. Treat the score row as insufficient for an administrative finding."
                .to_string(),
        );
    }
    Ok(RequestResponse::ok(review))
}

#[cfg(test)]
#[path = "cheat_evidence_tests.rs"]
mod tests;
