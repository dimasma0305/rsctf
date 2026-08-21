//! services/suspicion.rs — ported from RSCTF `Services/SuspicionService.cs` and
//! `Services/SuspicionScoringService.cs` (plus `Models/Internal/SuspicionType.cs`).
//!
//! The anti-cheat "suspicion" subsystem. Each `Participation` carries a cached
//! `suspicion_score` projection (see `models::data::play::participation`). The
//! immutable event ledger is authoritative: detectors persist evidence, then
//! recompute the projection from that ledger in the same transaction. Admins
//! read the canonical total back with [`suspicion_of`].
//!
//! Every individual signal is persisted to the `suspicion_event` audit table by
//! [`evaluate_submission`] (surfaced in the admin cheat-reports view); per-rule
//! weights use the compiled-in [`default_weight`] (a live admin-overridable
//! `SuspicionRule` table stores admin-overridable weights, seeded on startup).
//! The pure tiered-scoring aggregation ([`compute_breakdown`]) works from the
//! immutable in-memory event snapshot.

use sea_orm::DatabaseConnection;

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
    /// Non-actionable context/telemetry. Direct score is always 0; selected
    /// identity rules may corroborate hard evidence.
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
#[repr(i16)]
pub enum SuspicionType {
    StolenFlag = 0,
    SharedIp = 1,
    SharedFingerprint = 2,
    FingerprintChurn = 3,
    IpChurn = 4,
    UnknownIp = 5,
    CrossTeamIp = 6,
    TokenAbuse = 7,
    Hoarding = 8,
    Burst = 9,
    NoDownload = 10,
    NoContainer = 11,
    FastSolveOpen = 12,
    FastSolveDownload = 13,
    FastSolveContainer = 14,
    SequenceSimilarity = 15,
    CollusionGroup = 16,
    ZeroWrongAttempts = 17,
    WrongFlagLeakage = 18,
    SolutionRelay = 19,
    AdaptiveFastSolve = 20,
    DirectedSolving = 21,
    ClusteredRegistration = 22,
    SubnetOverlap = 23,
    HighWrongRate = 24,
    AutomatedPattern = 25,
    SessionConcurrency = 26,
    FirstBloodAnomaly = 27,
    HoneypotHit = 28,
    HoneypotProtocolHit = 29,
    HoneypotCanaryFlag = 30,
    HoneypotChain = 31,
    FlagEgress = 32,
    CrossTeamContainerAccess = 33,
    DelayedSolveSubmission = 34,
    InstantSubmitAfterAccess = 35,
    SubmitterNeverAccessedContainer = 36,
    AccessIpMismatchAtSubmission = 37,
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

    /// The compact, stable `i16` rule identity persisted in
    /// `SuspicionEvents.kind`. Discriminants are explicit so reordering the
    /// defaults table cannot reinterpret historical evidence.
    pub fn kind(self) -> i16 {
        self as i16
    }

    /// Reverse of [`kind`] — resolve a persisted `SuspicionEvents.kind` back to
    /// its rule variant.
    pub fn from_kind(kind: i16) -> Option<Self> {
        use SuspicionType::*;
        Some(match kind {
            0 => StolenFlag,
            1 => SharedIp,
            2 => SharedFingerprint,
            3 => FingerprintChurn,
            4 => IpChurn,
            5 => UnknownIp,
            6 => CrossTeamIp,
            7 => TokenAbuse,
            8 => Hoarding,
            9 => Burst,
            10 => NoDownload,
            11 => NoContainer,
            12 => FastSolveOpen,
            13 => FastSolveDownload,
            14 => FastSolveContainer,
            15 => SequenceSimilarity,
            16 => CollusionGroup,
            17 => ZeroWrongAttempts,
            18 => WrongFlagLeakage,
            19 => SolutionRelay,
            20 => AdaptiveFastSolve,
            21 => DirectedSolving,
            22 => ClusteredRegistration,
            23 => SubnetOverlap,
            24 => HighWrongRate,
            25 => AutomatedPattern,
            26 => SessionConcurrency,
            27 => FirstBloodAnomaly,
            28 => HoneypotHit,
            29 => HoneypotProtocolHit,
            30 => HoneypotCanaryFlag,
            31 => HoneypotChain,
            32 => FlagEgress,
            33 => CrossTeamContainerAccess,
            34 => DelayedSolveSubmission,
            35 => InstantSubmitAfterAccess,
            36 => SubmitterNeverAccessedContainer,
            37 => AccessIpMismatchAtSubmission,
            _ => return None,
        })
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
            AutomatedPattern | HighWrongRate | SolutionRelay => Strong,
            // Context — non-actionable context/telemetry (never scores directly).
            // FlagEgress and absence-derived telemetry remain visible for audit,
            // with zero corroboration as well.
            SharedIp
            | CrossTeamIp
            | UnknownIp
            | IpChurn
            | SubnetOverlap
            | SessionConcurrency
            | FastSolveOpen
            | FastSolveDownload
            | FastSolveContainer
            | DirectedSolving
            | ClusteredRegistration
            | HoneypotHit
            | HoneypotProtocolHit
            | HoneypotChain
            | FingerprintChurn
            | SharedFingerprint
            | AccessIpMismatchAtSubmission
            | FlagEgress
            | NoDownload
            | NoContainer
            | SubmitterNeverAccessedContainer => Context,
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
            NoDownload
            | NoContainer
            | FastSolveOpen
            | FastSolveDownload
            | FastSolveContainer
            | DirectedSolving
            | ClusteredRegistration
            | HoneypotHit
            | HoneypotProtocolHit
            | HoneypotChain
            | FlagEgress
            | SubmitterNeverAccessedContainer => 0,
            SharedFingerprint => 20,
            CrossTeamIp => 10,
            SessionConcurrency => 10,
            _ => 5,
        }
    }
}

/// Tier subtotal ceiling — a whole tier cannot contribute more than this.
/// `Hard` is intentionally uncapped (`i64::MAX`). Mirrors `TierCeiling`.
pub fn tier_ceiling(tier: SuspicionTier) -> i64 {
    match tier {
        SuspicionTier::Strong => 60,
        SuspicionTier::Behavioral => 25,
        SuspicionTier::Context => 0,
        SuspicionTier::Hard => i64::MAX,
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
// Canonical projection of the suspicion event ledger
// ─────────────────────────────────────────────────────────────────────────────

/// Read back the canonical ledger projection cached on a participation.
pub async fn suspicion_of(db: &DatabaseConnection, participation_id: i32) -> AppResult<i32> {
    sqlx::query_scalar::<_, i32>(
        r#"SELECT suspicion_score
             FROM "Participations"
            WHERE id = $1"#,
    )
    .bind(participation_id)
    .fetch_optional(db.get_postgres_connection_pool())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("participation not found"))
}

const SEED_DEFAULT_RULES_SQL: &str = r#"
    INSERT INTO "SuspicionRules" (rule_code, weight, description)
    SELECT rule_code, weight, description
      FROM UNNEST($1::text[], $2::integer[], $3::text[])
             AS defaults(rule_code, weight, description)
    ON CONFLICT (rule_code) DO NOTHING
"#;

/// Seed the built-in detector rules into `SuspicionRules` (RSCTF `PrelaunchHelper`
/// seeds `SuspicionRule.DefaultRules`) so admins can view/edit weights. Idempotent.
pub async fn seed_default_rules(db: &DatabaseConnection) -> AppResult<()> {
    let rule_codes = DEFAULTS
        .iter()
        .map(|(ty, _, _)| ty.code().to_owned())
        .collect::<Vec<_>>();
    let weights = DEFAULTS
        .iter()
        .map(|(_, weight, _)| *weight)
        .collect::<Vec<_>>();
    let descriptions = DEFAULTS
        .iter()
        .map(|(_, _, description)| (*description).to_owned())
        .collect::<Vec<_>>();

    sqlx::query(SEED_DEFAULT_RULES_SQL)
        .bind(rule_codes)
        .bind(weights)
        .bind(descriptions)
        .execute(db.get_postgres_connection_pool())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    Ok(())
}

#[cfg(test)]
mod rule_identity_tests {
    use super::{SuspicionType, DEFAULTS, SEED_DEFAULT_RULES_SQL};

    #[test]
    fn defaults_seed_is_one_set_based_idempotent_insert() {
        assert_eq!(SEED_DEFAULT_RULES_SQL.matches("INSERT INTO").count(), 1);
        assert!(SEED_DEFAULT_RULES_SQL.contains("FROM UNNEST"));
        assert!(SEED_DEFAULT_RULES_SQL.contains("ON CONFLICT (rule_code) DO NOTHING"));
    }

    #[test]
    fn all_38_historical_kinds_round_trip_through_stable_discriminants() {
        let expected = [
            SuspicionType::StolenFlag,
            SuspicionType::SharedIp,
            SuspicionType::SharedFingerprint,
            SuspicionType::FingerprintChurn,
            SuspicionType::IpChurn,
            SuspicionType::UnknownIp,
            SuspicionType::CrossTeamIp,
            SuspicionType::TokenAbuse,
            SuspicionType::Hoarding,
            SuspicionType::Burst,
            SuspicionType::NoDownload,
            SuspicionType::NoContainer,
            SuspicionType::FastSolveOpen,
            SuspicionType::FastSolveDownload,
            SuspicionType::FastSolveContainer,
            SuspicionType::SequenceSimilarity,
            SuspicionType::CollusionGroup,
            SuspicionType::ZeroWrongAttempts,
            SuspicionType::WrongFlagLeakage,
            SuspicionType::SolutionRelay,
            SuspicionType::AdaptiveFastSolve,
            SuspicionType::DirectedSolving,
            SuspicionType::ClusteredRegistration,
            SuspicionType::SubnetOverlap,
            SuspicionType::HighWrongRate,
            SuspicionType::AutomatedPattern,
            SuspicionType::SessionConcurrency,
            SuspicionType::FirstBloodAnomaly,
            SuspicionType::HoneypotHit,
            SuspicionType::HoneypotProtocolHit,
            SuspicionType::HoneypotCanaryFlag,
            SuspicionType::HoneypotChain,
            SuspicionType::FlagEgress,
            SuspicionType::CrossTeamContainerAccess,
            SuspicionType::DelayedSolveSubmission,
            SuspicionType::InstantSubmitAfterAccess,
            SuspicionType::SubmitterNeverAccessedContainer,
            SuspicionType::AccessIpMismatchAtSubmission,
        ];

        assert_eq!(expected.len(), 38);
        assert_eq!(DEFAULTS.len(), expected.len());
        for (kind, ty) in expected.into_iter().enumerate() {
            let kind = i16::try_from(kind).expect("historical kind fits i16");
            assert_eq!(ty.kind(), kind);
            assert_eq!(SuspicionType::from_kind(kind), Some(ty));
            assert_eq!(SuspicionType::from_code(ty.code()), Some(ty));
            assert_eq!(DEFAULTS[usize::try_from(kind).unwrap()].0, ty);
        }
        assert_eq!(SuspicionType::from_kind(-1), None);
        assert_eq!(SuspicionType::from_kind(38), None);
    }
}

mod cheat_checks;
mod cheat_stat;
mod container_access;
mod correlation;
mod detectors;
mod honeypot;
mod outbox;
mod scoring;
pub use cheat_checks::*;
pub use cheat_stat::*;
pub use container_access::*;
pub use correlation::*;
pub use detectors::*;
pub use honeypot::*;
#[cfg(test)]
pub(crate) use outbox::seal_reconciled_game_for_test;
pub use outbox::*;
pub use scoring::*;
