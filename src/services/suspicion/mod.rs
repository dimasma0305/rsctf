//! services/suspicion.rs — ported from RSCTF `Services/SuspicionService.cs` and
//! `Services/SuspicionScoringService.cs` (plus `Models/Internal/SuspicionType.cs`).
//!
//! The anti-cheat "suspicion" subsystem. Each `Participation` carries a running
//! `suspicion_score` (see `models::data::play::participation`). Detectors persist
//! evidence and its score delta atomically; admins read the total back with
//! [`suspicion_of`]. [`add_suspicion`] remains for compatible non-detector calls.
//!
//! Every individual signal is persisted to the `suspicion_event` audit table by
//! [`evaluate_submission`] (surfaced in the admin cheat-reports view); per-rule
//! weights use the compiled-in [`default_weight`] (a live admin-overridable
//! `SuspicionRule` table stores admin-overridable weights, seeded on startup). The pure tiered-scoring aggregation
//! ([`compute_breakdown`]) is faithfully ported and works off in-memory event
//! rows, so it is ready the moment the audit table lands.

use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};

use crate::models::data::{participation, suspicion_rule};
use crate::utils::error::{AppError, AppResult};

const GLOBAL_EVIDENCE_KEY: &str = "global";

#[inline]
fn challenge_evidence_key(challenge_id: i32) -> String {
    format!("challenge:{challenge_id}")
}

#[inline]
fn submission_evidence_key(submission_id: i32) -> String {
    format!("submission:{submission_id}")
}

// ─────────────────────────────────────────────────────────────────────────────
// Rule codes (SuspicionType.cs) and evidence tiers
// ─────────────────────────────────────────────────────────────────────────────

/// Evidence tier — ordered by how strongly a signal implicates a team.
/// Mirrors RSCTF `SuspicionTier`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SuspicionTier {
    /// Network/identity correlation. Direct score is always 0; corroborates only.
    Context = 0,
    /// Timing / similarity heuristics. Capped low; never alarming alone.
    Behavioral = 1,
    /// Automation / scanner behaviour. Actionable, capped below "confirmed".
    Strong = 2,
    /// Cross-team flag/session movement. Uncapped; forces the EVIDENCED band.
    Hard = 3,
}

/// The full set of suspicion rule codes, mirroring RSCTF `SuspicionType`.
/// The `str` value of each variant is the wire/DB rule code used everywhere the
/// C# code passes a string constant (e.g. `SuspicionType.StolenFlag`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum SuspicionType {
    StolenFlag,
    SharedIp,
    SharedFingerprint,
    FingerprintChurn,
    IpChurn,
    UnknownIp,
    CrossTeamIp,
    TokenAbuse,
    Hoarding,
    Burst,
    NoDownload,
    NoContainer,
    FastSolveOpen,
    FastSolveDownload,
    FastSolveContainer,
    SequenceSimilarity,
    CollusionGroup,
    ZeroWrongAttempts,
    WrongFlagLeakage,
    SolutionRelay,
    AdaptiveFastSolve,
    DirectedSolving,
    ClusteredRegistration,
    SubnetOverlap,
    HighWrongRate,
    AutomatedPattern,
    SessionConcurrency,
    FirstBloodAnomaly,
    HoneypotHit,
    HoneypotProtocolHit,
    HoneypotCanaryFlag,
    HoneypotChain,
    FlagEgress,
    CrossTeamContainerAccess,
    DelayedSolveSubmission,
    InstantSubmitAfterAccess,
    SubmitterNeverAccessedContainer,
    AccessIpMismatchAtSubmission,
}

impl SuspicionType {
    /// The canonical rule code string, identical to the RSCTF constant value.
    pub fn code(self) -> &'static str {
        use SuspicionType::*;
        match self {
            StolenFlag => "StolenFlag",
            SharedIp => "SharedIP",
            SharedFingerprint => "SharedFingerprint",
            FingerprintChurn => "FingerprintChurn",
            IpChurn => "IpChurn",
            UnknownIp => "UnknownIP",
            CrossTeamIp => "CrossTeamIP",
            TokenAbuse => "TokenAbuse",
            Hoarding => "Hoarding",
            Burst => "Burst",
            NoDownload => "NoDownload",
            NoContainer => "NoContainer",
            FastSolveOpen => "FastSolve-Open",
            FastSolveDownload => "FastSolve-Download",
            FastSolveContainer => "FastSolve-Container",
            SequenceSimilarity => "SequenceSimilarity",
            CollusionGroup => "CollusionGroup",
            ZeroWrongAttempts => "ZeroWrongAttempts",
            WrongFlagLeakage => "WrongFlagLeakage",
            SolutionRelay => "SolutionRelay",
            AdaptiveFastSolve => "AdaptiveFastSolve",
            DirectedSolving => "DirectedSolving",
            ClusteredRegistration => "ClusteredRegistration",
            SubnetOverlap => "SubnetOverlap",
            HighWrongRate => "HighWrongRate",
            AutomatedPattern => "AutomatedPattern",
            SessionConcurrency => "SessionConcurrency",
            FirstBloodAnomaly => "FirstBloodAnomaly",
            HoneypotHit => "HoneypotHit",
            HoneypotProtocolHit => "HoneypotProtocolHit",
            HoneypotCanaryFlag => "HoneypotCanaryFlag",
            HoneypotChain => "HoneypotChain",
            FlagEgress => "FlagEgress",
            CrossTeamContainerAccess => "CrossTeamContainerAccess",
            DelayedSolveSubmission => "DelayedSolveSubmission",
            InstantSubmitAfterAccess => "InstantSubmitAfterAccess",
            SubmitterNeverAccessedContainer => "SubmitterNeverAccessedContainer",
            AccessIpMismatchAtSubmission => "AccessIpMismatchAtSubmission",
        }
    }

    /// Reverse lookup from a rule code string, for events read back from the DB.
    pub fn from_code(code: &str) -> Option<Self> {
        DEFAULTS
            .iter()
            .find(|(ty, _, _)| ty.code() == code)
            .map(|(ty, _, _)| *ty)
    }

    /// The compact `i16` rule code persisted in `SuspicionEvents.kind`. Derived
    /// from the rule's position in [`DEFAULTS`] (StolenFlag = 0, SharedIp = 1, …),
    /// which is stable as long as the 38-rule table is only appended to.
    pub fn kind(self) -> i16 {
        DEFAULTS
            .iter()
            .position(|(ty, _, _)| *ty == self)
            .map(|i| i as i16)
            .unwrap_or(-1)
    }

    /// Reverse of [`kind`] — resolve a persisted `SuspicionEvents.kind` back to
    /// its rule variant.
    pub fn from_kind(kind: i16) -> Option<Self> {
        usize::try_from(kind)
            .ok()
            .and_then(|i| DEFAULTS.get(i))
            .map(|(ty, _, _)| *ty)
    }

    /// Default weight + human-readable description (`SuspicionType.Defaults`).
    pub fn default_entry(self) -> (i32, &'static str) {
        DEFAULTS
            .iter()
            .find(|(ty, _, _)| *ty == self)
            .map(|(_, w, d)| (*w, *d))
            .unwrap_or((10, ""))
    }

    /// Evidence tier for this rule (`SuspicionType.GetTier`).
    /// Unknown rules default to `Behavioral`, matching RSCTF.
    pub fn tier(self) -> SuspicionTier {
        use SuspicionTier::*;
        use SuspicionType::*;
        match self {
            // Hard — cross-team flag/session possession
            StolenFlag
            | CrossTeamContainerAccess
            | WrongFlagLeakage
            | TokenAbuse
            | HoneypotCanaryFlag => Hard,
            // Strong — automation / scanner behaviour
            AutomatedPattern | HighWrongRate | SolutionRelay | HoneypotChain
            | HoneypotProtocolHit => Strong,
            // Context — network / identity correlation (NEVER scores on its own)
            SharedIp
            | CrossTeamIp
            | UnknownIp
            | IpChurn
            | SubnetOverlap
            | SessionConcurrency
            | ClusteredRegistration
            | FingerprintChurn
            | SharedFingerprint
            | AccessIpMismatchAtSubmission
            | FlagEgress => Context,
            // Behavioral — timing / similarity heuristics (everything else)
            _ => Behavioral,
        }
    }

    /// Per-rule incident cap (`SuspicionType.GetMaxIncidents`); default 3.
    pub fn max_incidents(self) -> i32 {
        use SuspicionType::*;
        match self {
            StolenFlag | CrossTeamContainerAccess | WrongFlagLeakage => 10,
            TokenAbuse => 5,
            HoneypotCanaryFlag => 3,
            AutomatedPattern | HighWrongRate => 3,
            SolutionRelay => 2,
            HoneypotChain => 1,
            HoneypotProtocolHit => 3,
            FastSolveOpen | FastSolveDownload | FastSolveContainer => 3,
            ZeroWrongAttempts | Burst | Hoarding | SequenceSimilarity => 3,
            CollusionGroup => 1,
            AdaptiveFastSolve => 3,
            DirectedSolving => 1,
            FirstBloodAnomaly => 4,
            DelayedSolveSubmission => 5,
            InstantSubmitAfterAccess => 3,
            SubmitterNeverAccessedContainer => 3,
            HoneypotHit => 5,
            NoDownload | NoContainer => 3,
            _ => 3,
        }
    }

    /// Corroboration weight a context signal lends to *existing* hard evidence
    /// (`SuspicionType.CorroborationUnit`).
    pub fn corroboration_unit(self) -> i32 {
        use SuspicionType::*;
        match self {
            SharedFingerprint => 20,
            CrossTeamIp => 10,
            SessionConcurrency => 10,
            _ => 5,
        }
    }
}

/// Tier subtotal ceiling — a whole tier cannot contribute more than this.
/// `Hard` is intentionally uncapped (`i32::MAX`). Mirrors `TierCeiling`.
pub fn tier_ceiling(tier: SuspicionTier) -> i32 {
    match tier {
        SuspicionTier::Strong => 60,
        SuspicionTier::Behavioral => 25,
        SuspicionTier::Context => 0,
        SuspicionTier::Hard => i32::MAX,
    }
}

/// `(rule, default weight, description)` — the full `SuspicionType.Defaults`
/// table, in declaration order.
pub static DEFAULTS: &[(SuspicionType, i32, &str)] = &[
    (SuspicionType::StolenFlag, 100, "Flag stolen from another team"),
    (SuspicionType::SharedIp, 10, "Multiple team members using same IP"),
    (SuspicionType::SharedFingerprint, 60, "Multiple users with same browser fingerprint"),
    (SuspicionType::FingerprintChurn, 30, "Single user using many different browser fingerprints"),
    (SuspicionType::IpChurn, 20, "Single user using many different IP addresses"),
    (SuspicionType::UnknownIp, 10, "Using IP not seen in game before"),
    (SuspicionType::CrossTeamIp, 20, "IP used by members from multiple teams"),
    (SuspicionType::TokenAbuse, 80, "Multiple people using same submission token"),
    (SuspicionType::Hoarding, 30, "Solved challenge long after container destroy"),
    (SuspicionType::Burst, 30, "Multiple challenges solved in a very short time"),
    (SuspicionType::NoDownload, 80, "Solved without downloading attachment"),
    (SuspicionType::NoContainer, 80, "Solved without starting container"),
    (SuspicionType::FastSolveOpen, 50, "Solved very quickly after opening challenge"),
    (SuspicionType::FastSolveDownload, 50, "Solved very quickly after downloading attachment"),
    (SuspicionType::FastSolveContainer, 50, "Solved very quickly after starting container"),
    (SuspicionType::SequenceSimilarity, 40, "High similarity in solve order and timing"),
    (SuspicionType::CollusionGroup, 10, "Member of a detected collusion group"),
    (SuspicionType::ZeroWrongAttempts, 50, "Solved dynamic challenge on first attempt with no wrong submissions"),
    (SuspicionType::WrongFlagLeakage, 80, "Submitted another team's valid dynamic flag as a wrong answer"),
    (SuspicionType::SolutionRelay, 60, "Consistently solves challenges shortly after another team with constant lag"),
    (SuspicionType::AdaptiveFastSolve, 60, "Solved far faster than the community median solve time"),
    (SuspicionType::DirectedSolving, 30, "Only opened challenges they solved — no exploratory browsing"),
    (SuspicionType::ClusteredRegistration, 40, "Multiple team accounts registered from the same IP within 48h"),
    (SuspicionType::SubnetOverlap, 5, "Teams share the same /24 subnet"),
    (SuspicionType::HighWrongRate, 40, "Burst of wrong flag submissions — possible brute force"),
    (SuspicionType::AutomatedPattern, 50, "Machine-speed flag submission intervals — likely scripted"),
    (SuspicionType::SessionConcurrency, 30, "Same user account active from two different IPs within 10 minutes"),
    (SuspicionType::FirstBloodAnomaly, 20, "First blood on a hard challenge not solved by others for 2+ hours"),
    (SuspicionType::HoneypotHit, 70, "Hit a platform honeypot HTTP route — automated reconnaissance"),
    (SuspicionType::HoneypotProtocolHit, 90, "Connected to a platform honeypot protocol service (SSH, Redis, etc.) — broad infra scan"),
    (SuspicionType::HoneypotCanaryFlag, 100, "Submitted a canary flag exposed only via honeypot — automated scrape pipeline"),
    (SuspicionType::HoneypotChain, 150, "Followed multiple cross-referenced honeypot baits — automated link-following scanner or agent"),
    (SuspicionType::FlagEgress, 80, "Team flag observed in proxied container traffic — exfil pipeline or automated solver"),
    (SuspicionType::CrossTeamContainerAccess, 120, "A non-admin user from a different team opened the proxy WebSocket on this team's container"),
    (SuspicionType::DelayedSolveSubmission, 40, "Submitter personally opened the container long before they submitted the flag"),
    (SuspicionType::InstantSubmitAfterAccess, 50, "Submission within seconds of the submitter's first proxy access — automated solver pipeline"),
    (SuspicionType::SubmitterNeverAccessedContainer, 30, "Submitter never personally opened the container; a teammate did"),
    (SuspicionType::AccessIpMismatchAtSubmission, 30, "Submitter's IP at submission time does not match any IP they used to access the container"),
];

/// Default compiled-in weight for a rule code (`SuspicionService.GetDefaultWeight`).
/// Unknown codes fall back to `10`, matching RSCTF.
pub fn default_weight(rule_code: &str) -> i32 {
    SuspicionType::from_code(rule_code)
        .map(|ty| ty.default_entry().0)
        .unwrap_or(10)
}

/// Effective `(weight, description)` for a rule: the admin-configured
/// `SuspicionRule.Weight` for its code (DB) or the compiled-in default (RSCTF
/// `SuspicionService.GetWeight`). A missing row uses the default; a database
/// failure is propagated so an incorrect fallback is never frozen into an
/// immutable evidence row.
pub async fn resolve_entry(
    db: &DatabaseConnection,
    ty: SuspicionType,
) -> AppResult<(i32, &'static str)> {
    let (default_w, desc) = ty.default_entry();
    let weight = sqlx::query_scalar::<_, i32>(
        r#"SELECT weight
             FROM "SuspicionRules"
            WHERE rule_code = $1"#,
    )
    .bind(ty.code())
    .fetch_optional(db.get_postgres_connection_pool())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .unwrap_or(default_w);
    Ok((weight, desc))
}

// ─────────────────────────────────────────────────────────────────────────────
// DB operations on participation.suspicion_score
// ─────────────────────────────────────────────────────────────────────────────

/// Atomically bump a participation's running suspicion score by `delta`.
///
/// Detector code must use `record_with_dedup`, which couples this increment to
/// a newly inserted evidence row. This public helper remains for compatibility
/// with non-detector callers and uses one SQL update so concurrent increments
/// cannot overwrite one another.
pub async fn add_suspicion(
    db: &DatabaseConnection,
    participation_id: i32,
    delta: i32,
    reason: &str,
) -> AppResult<()> {
    let new_score: Option<i32> = sqlx::query_scalar(
        r#"UPDATE "Participations"
              SET suspicion_score = suspicion_score + $2
            WHERE id = $1
        RETURNING suspicion_score"#,
    )
    .bind(participation_id)
    .bind(delta)
    .fetch_optional(db.get_postgres_connection_pool())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let new_score = new_score.ok_or_else(|| AppError::not_found("participation not found"))?;

    tracing::info!(
        participation_id,
        delta,
        reason,
        new_score,
        "suspicion score updated"
    );
    Ok(())
}

/// Read back a participation's current suspicion score.
pub async fn suspicion_of(db: &DatabaseConnection, participation_id: i32) -> AppResult<i32> {
    let part = participation::Entity::find_by_id(participation_id)
        .one(db)
        .await?
        .ok_or_else(|| AppError::not_found("participation not found"))?;
    Ok(part.suspicion_score)
}

/// Seed the built-in detector rules into `SuspicionRules` (RSCTF `PrelaunchHelper`
/// seeds `SuspicionRule.DefaultRules`) so admins can view/edit weights. Idempotent.
pub async fn seed_default_rules(db: &DatabaseConnection) -> AppResult<()> {
    for (ty, weight, desc) in DEFAULTS.iter() {
        let code = ty.code();
        let exists = suspicion_rule::Entity::find()
            .filter(suspicion_rule::Column::RuleCode.eq(code))
            .one(db)
            .await?
            .is_some();
        if !exists {
            let _ = suspicion_rule::ActiveModel {
                rule_code: Set(code.to_string()),
                weight: Set(*weight),
                description: Set(desc.to_string()),
                ..Default::default()
            }
            .insert(db)
            .await;
        }
    }
    Ok(())
}

mod cheat_checks;
mod cheat_stat;
mod container_access;
mod correlation;
mod detectors;
mod honeypot;
mod scoring;
pub use cheat_checks::*;
pub use cheat_stat::*;
pub use container_access::*;
pub use correlation::*;
pub use detectors::*;
pub use honeypot::*;
pub use scoring::*;
